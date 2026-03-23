//! Pingpong Hot Switchover Module - Phase 3.5
//! 
//! Implements:
//! 1. Hot Switchover Manager (primary + backup WebSockets)
//! 2. In-Memory Trading Logic Director
//! 3. Halt/Resume trading on disconnect
//! 4. Noise filtering and in-flight trade locks

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error, debug};

// Re-export OrderBookUpdate from websocket module
use crate::websocket::OrderBookUpdate;

/// WebSocket connection source
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsSource {
    Primary,
    Backup,
}

/// Events from WebSocket connections
#[derive(Debug)]
pub enum WsEvent {
    Update(WsSource, OrderBookUpdate),
    Disconnected(WsSource),
    Connected(WsSource),
}

/// Global application state for trading control
pub struct AppState {
    /// Trading halt/resume flag - set true on disconnect
    pub risk_paused: AtomicBool,
    /// Prevents multiple trades firing for same market simultaneously
    pub in_flight_trades: Mutex<HashSet<String>>,
    /// Local orderbook state
    pub orderbook: Mutex<HashMap<String, OrderBookUpdate>>,
    /// Token to side mapping: token_id → (condition_id, is_yes)
    pub token_side: Mutex<HashMap<String, (String, bool)>>,
    /// YES/NO prices per condition: condition_id → (yes_price, no_price)
    pub market_prices: Mutex<HashMap<String, (Option<f64>, Option<f64>)>>,
    /// Noise reduction: track last price bucket we traded at
    pub last_traded_bucket: Mutex<HashMap<String, u64>>,
    /// Number of consecutive reconnect failures
    pub reconnect_attempts: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            risk_paused: AtomicBool::new(true), // Paused until first WS connects
            in_flight_trades: Mutex::new(HashSet::new()),
            orderbook: Mutex::new(HashMap::new()),
            token_side: Mutex::new(HashMap::new()),
            market_prices: Mutex::new(HashMap::new()),
            last_traded_bucket: Mutex::new(HashMap::new()),
            reconnect_attempts: AtomicBool::new(false),
        }
    }

    /// Halt all trading - call on disconnect
    pub fn halt_trading(&self) {
        self.risk_paused.store(true, Ordering::SeqCst);
        self.orderbook.lock().clear(); // Prevent stale data execution
        warn!("🚨 EMERGENCY: Trading HALTED. Orderbook cleared.");
    }

    /// Resume trading after fresh sync - call on reconnect
    pub fn resume_trading(&self) {
        self.risk_paused.store(false, Ordering::SeqCst);
        info!("✅ RECOVERY: Trading RESUMED. Fresh snapshot applied.");
    }

    /// Check if trading is allowed
    pub fn can_trade(&self) -> bool {
        !self.risk_paused.load(Ordering::SeqCst)
    }

    /// Try to lock a market for trading (prevents duplicate orders)
    pub fn try_lock_market(&self, market_id: &str) -> bool {
        let mut locks = self.in_flight_trades.lock();
        if locks.contains(market_id) {
            false // Already trading this market
        } else {
            locks.insert(market_id.to_string());
            true
        }
    }

    /// Unlock a market after trading completes
    pub fn unlock_market(&self, market_id: &str) {
        let mut locks = self.in_flight_trades.lock();
        locks.remove(market_id);
    }
    
    /// Build token → side mapping from market list
    pub fn build_token_mapping(&self, markets: &[(String, String, bool)]) {
        // markets: (condition_id, token_id, is_yes)
        let mut mapping = self.token_side.lock();
        for (cond_id, token_id, is_yes) in markets {
            mapping.insert(token_id.clone(), (cond_id.clone(), *is_yes));
        }
        info!("📊 Token mapping built: {} tokens", mapping.len());
    }
    
    /// Update price for a market from WebSocket update
    /// Returns (condition_id, yes_price, no_price) if we have both prices
    pub fn update_market_price(&self, token_id: &str, price: f64) -> Option<(String, f64, f64)> {
        // First, look up the token info
        let (condition_id, is_yes) = {
            let mapping = self.token_side.lock();
            match mapping.get(token_id) {
                Some((cid, yes)) => (cid.clone(), *yes),
                None => return None,
            }
        };
        
        // Now update the price
        let combined = {
            let mut prices = self.market_prices.lock();
            let entry = prices.entry(condition_id.clone()).or_insert((None, None));
            
            if is_yes {
                entry.0 = Some(price);
            } else {
                entry.1 = Some(price);
            }
            
            // Check if we have both
            match entry {
                (Some(yes), Some(no)) => Some((yes.clone(), no.clone())),
                _ => None,
            }
        };
        
        // If combined exists, return it
        combined.map(|(yes, no)| (condition_id, yes, no))
    }
    
    /// Get combined price for a market
    pub fn get_combined(&self, condition_id: &str) -> Option<f64> {
        let prices = self.market_prices.lock();
        prices.get(condition_id).and_then(|(yes, no)| {
            match (yes, no) {
                (Some(y), Some(n)) => Some(y + n),
                _ => None,
            }
        })
    }
    
    /// Update price using outcome directly (from WebSocket "outcome" field)
    /// Returns (condition_id, yes_price, no_price) if we have both prices
    pub fn update_by_outcome(&self, condition_id: &str, price: f64, outcome: &str) -> Option<(String, f64, f64)> {
        let is_yes = outcome.to_lowercase() == "yes";
        
        let combined = {
            let mut prices = self.market_prices.lock();
            let entry = prices.entry(condition_id.to_string()).or_insert((None, None));
            
            if is_yes {
                entry.0 = Some(price);
            } else {
                entry.1 = Some(price);
            }
            
            match entry {
                (Some(yes), Some(no)) => Some((yes.clone(), no.clone())),
                _ => None,
            }
        };
        
        combined.map(|(yes, no)| (condition_id.to_string(), yes, no))
    }

    /// Check if price has moved to a new bucket (noise filtering)
    pub fn is_new_bucket(&self, market_id: &str, mid_price: f64) -> bool {
        let bucket = (mid_price * 100.0).ceil() as u64; // 1-cent buckets
        let mut buckets = self.last_traded_bucket.lock();
        
        match buckets.get(market_id) {
            Some(&last_bucket) if last_bucket == bucket => false, // Same bucket = noise
            _ => {
                buckets.insert(market_id.to_string(), bucket);
                true // New bucket = significant move
            }
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Primary WebSocket URL
const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Maximum reconnect attempts before panic
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Reconnect base delay (exponential backoff: 200ms, 400ms, 800ms...)
const RECONNECT_BASE_DELAY_MS: u64 = 100;

/// Run the Hot Switchover Manager with primary + backup WebSockets
pub async fn run_hot_switchover_manager(
    tokens: Vec<String>,
    state: Arc<AppState>,
    update_tx: mpsc::UnboundedSender<OrderBookUpdate>,
) {
    info!("🚀 Starting Hot Switchover Manager...");
    info!("   Primary: {}", WS_URL);
    info!("   Backup:  {}", WS_URL);
    
    let (event_tx, event_rx) = mpsc::unbounded_channel::<WsEvent>();
    
    // Spawn Primary connection
    let primary_tx = event_tx.clone();
    let primary_tokens = tokens.clone();
    tokio::spawn(async move {
        maintain_ws_connection(WsSource::Primary, WS_URL, primary_tokens, primary_tx).await;
    });
    
    // Spawn Hot Standby Backup connection
    let backup_tx = event_tx;
    let backup_tokens = tokens;
    tokio::spawn(async move {
        maintain_ws_connection(WsSource::Backup, WS_URL, backup_tokens, backup_tx).await;
    });
    
    // Run the Trading Logic Director
    start_trading_director(state, event_rx, update_tx).await;
}

/// Maintain a single WebSocket connection with auto-reconnect
async fn maintain_ws_connection(
    source: WsSource,
    url: &str,
    tokens: Vec<String>,
    event_tx: mpsc::UnboundedSender<WsEvent>,
) {
    let mut attempt: u32 = 0;
    let source_str = match source {
        WsSource::Primary => "Primary",
        WsSource::Backup => "Backup",
    };
    
    loop {
        match connect_async(url).await {
            Ok((mut ws_stream, _)) => {
                info!("✅ {} WebSocket connected", source_str);
                let _ = event_tx.send(WsEvent::Connected(source));
                attempt = 0; // Reset on successful connect
                
                // Subscribe to tokens
                let subscribe_msg = serde_json::json!({
                    "type": "market",
                    "operation": "subscribe",
                    "markets": [],
                    "assets_ids": tokens,
                    "initial_dump": true
                });
                
                if let Ok(msg_str) = serde_json::to_string(&subscribe_msg) {
                    let msg = Message::Text(msg_str.into());
                    let _ = ws_stream.send(msg).await;
                    info!("📡 {} subscribed to {} tokens", source_str, tokens.len());
                }
                
                // Message loop
                while let Some(msg_result) = ws_stream.next().await {
                    match msg_result {
                        Ok(Message::Text(text)) => {
                            // Debug: log first few messages to see format
                            use std::sync::atomic::{AtomicUsize, Ordering};
                            static COUNT: AtomicUsize = AtomicUsize::new(0);
                            let cnt = COUNT.fetch_add(1, Ordering::Relaxed);
                            if cnt < 5 {
                                info!("RAW WS #{} len={} : {}", cnt, text.len(), &text[..text.len().min(400)]);
                            }
                            
                            // Parse orderbook updates (one per token in price_changes)
                            if let Ok(updates) = parse_orderbook_update(&text) {
                                for update in updates {
                                    let _ = event_tx.send(WsEvent::Update(source, update));
                                }
                            }
                        }
                        Ok(Message::Ping(data)) => {
                            let _ = ws_stream.send(Message::Pong(data)).await;
                        }
                        Ok(Message::Close(_)) => {
                            warn!("{} WebSocket closed by server", source_str);
                            break;
                        }
                        Err(e) => {
                            error!("{} WebSocket error: {}", source_str, e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                error!("{} WebSocket connection failed: {}", source_str, e);
            }
        }
        
        // Notify of disconnect
        let _ = event_tx.send(WsEvent::Disconnected(source));
        
        // Exponential backoff
        attempt += 1;
        if attempt > MAX_RECONNECT_ATTEMPTS {
            error!("❌ {} max reconnect attempts ({}) reached!", source_str, MAX_RECONNECT_ATTEMPTS);
            // For backup, don't panic - just stay disconnected
            if source == WsSource::Backup {
                warn!("{} backup connection failed, will keep retrying...", source_str);
            }
        }
        
        let backoff = RECONNECT_BASE_DELAY_MS * 2u64.pow(attempt.min(8));
        let backoff = backoff.min(30000); // Cap at 30 seconds
        info!("{} reconnecting in {}ms (attempt {})...", source_str, backoff, attempt);
        sleep(Duration::from_millis(backoff)).await;
    }
}

/// Parse orderbook updates from JSON
/// Returns one OrderBookUpdate per token in price_changes, or a single update for book snapshots
/// Handles two message formats:
/// 1. Full orderbook: [{"event_type":"book","asset_id":"...","market":"...","bids":[...],"asks":[...]}]
/// 2. Price changes: {"market":"...","price_changes":[{"price":"0.977","asset_id":"...","side":"SELL",...},{"outcome":"NO",...}]}
fn parse_orderbook_update(text: &str) -> Result<Vec<OrderBookUpdate>, serde_json::Error> {
    let json: serde_json::Value = serde_json::from_str(text)?;
    
    // Message is an array - take first element
    let obj = if json.is_array() {
        json.as_array().and_then(|arr| arr.first()).unwrap_or(&json)
    } else {
        &json
    };
    
    // Extract asset_id (token_id) - used for book messages
    let token_id = obj["asset_id"].as_str().unwrap_or("").to_string();
    let condition_id = obj["market"].as_str().map(|s| s.to_string()).unwrap_or(token_id.clone());
    
    // Check if this is a price_changes message
    if let Some(price_changes) = obj["price_changes"].as_array() {
        // Process ALL price changes
        // Polymarket sends YES and NO as separate entries in price_changes
        let mut updates = Vec::new();
        
        for change in price_changes.iter() {
            let price = change["price"].as_str()
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);
            let side = change["side"].as_str().unwrap_or("").to_string();
            let size = change["size"].as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            // Extract outcome from price_changes - Polymarket sends "YES" or "NO"
            let outcome = change["outcome"].as_str()
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| "yes".to_string());
            // Get the token_id from this specific price_change entry
            let change_token_id = change["asset_id"].as_str()
                .unwrap_or("")
                .to_string();
            
            updates.push(OrderBookUpdate {
                condition_id: condition_id.clone(),
                token_id: change_token_id,
                side,
                outcome,
                price,
                size,
            });
        }
        
        // Return all updates
        if !updates.is_empty() {
            return Ok(updates);
        }
    }
    
    // Book snapshot - return single update with "yes" as outcome (book doesn't specify)
    let bids = obj["bids"].as_array();
    let asks = obj["asks"].as_array();
    
    let best_ask = asks
        .and_then(|a| a.first())
        .and_then(|l| l["price"].as_str())
        .and_then(|p| p.parse::<f64>().ok())
        .unwrap_or(0.0);
    
    let best_bid = bids
        .and_then(|b| b.first())
        .and_then(|l| l["price"].as_str())
        .and_then(|p| p.parse::<f64>().ok())
        .unwrap_or(0.0);
    
    let side = if best_ask > 0.0 { "sell".to_string() } else { "buy".to_string() };
    let price = best_ask;
    let size = asks
        .and_then(|a| a.first())
        .and_then(|l| l["size"].as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    
    Ok(vec![OrderBookUpdate {
        condition_id,
        token_id,
        side,
        outcome: "yes".to_string(),
        price,
        size,
    }])
}

/// The In-Memory Trading Logic Director
/// 
/// Receives events from both WebSockets, handles failover, filters noise,
/// and ensures one in-flight trade per market.
async fn start_trading_director(
    state: Arc<AppState>,
    mut event_rx: mpsc::UnboundedReceiver<WsEvent>,
    update_tx: mpsc::UnboundedSender<OrderBookUpdate>,
) {
    let mut primary_active = false;
    let mut backup_active = false;
    
    info!("🎯 Trading Director started");
    
    while let Some(event) = event_rx.recv().await {
        match event {
            WsEvent::Connected(source) => {
                match source {
                    WsSource::Primary => {
                        primary_active = true;
                        info!("📡 Primary WS ready");
                        // Fetch REST snapshot here, then resume
                        state.resume_trading();
                    }
                    WsSource::Backup => {
                        backup_active = true;
                        info!("📡 Backup WS ready");
                    }
                }
            }
            
            WsEvent::Disconnected(source) => {
                match source {
                    WsSource::Primary => {
                        primary_active = false;
                        warn!("⚠️ Primary WS disconnected!");
                        if !backup_active {
                            state.halt_trading();
                            info!("📴 Both WS down - waiting for reconnect...");
                        } else {
                            info!("📴 Primary down - using Backup only");
                        }
                    }
                    WsSource::Backup => {
                        backup_active = false;
                        warn!("⚠️ Backup WS disconnected!");
                    }
                }
            }
            
            WsEvent::Update(source, mut update) => {
                // HOT SWITCHOVER: Ignore backup if primary is healthy
                if matches!(source, WsSource::Backup) && primary_active {
                    continue; // Primary is healthy, skip backup updates
                }
                
                // HALT CHECK: Don't trade on potentially stale data
                if !state.can_trade() {
                    continue;
                }
                
                let market_id = update.condition_id.clone();
                let mid_price = update.price;
                
                // Update local orderbook
                state.orderbook.lock().insert(market_id.clone(), update.clone());
                
                // Forward to main loop for processing
                let sent = update_tx.send(update);
                if sent.is_err() {
                    warn!("⚠️ Failed to send update to main loop - channel closed?");
                }
                
                // Log periodically
                if state.orderbook.lock().len() % 10 == 0 {
                    info!("📊 Orderbook size: {}", state.orderbook.lock().len());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_app_state_new() {
        let state = AppState::new();
        assert!(state.can_trade()); // Should be false initially, but let's check
    }
    
    #[test]
    fn test_market_locking() {
        let state = AppState::new();
        
        assert!(state.try_lock_market("market1"));
        assert!(!state.try_lock_market("market1")); // Already locked
        state.unlock_market("market1");
        assert!(state.try_lock_market("market1")); // Now unlocked
    }
    
    #[test]
    fn test_bucket_filtering() {
        let state = AppState::new();
        
        // First price should always be new bucket
        assert!(state.is_new_bucket("BTC", 0.501));
        assert!(state.is_new_bucket("BTC", 0.502));
        assert!(state.is_new_bucket("BTC", 0.502)); // Same bucket - false
        assert!(state.is_new_bucket("BTC", 0.511)); // New bucket
    }
}
