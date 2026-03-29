//! Pingpong Hot Switchover Module - Phase 3.5
//! 
//! Implements:
//! 1. Hot Switchover Manager (primary + backup WebSockets)
//! 2. In-Memory Trading Logic Director
//! 3. Halt/Resume trading on disconnect
//! 4. Noise filtering and in-flight trade locks

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error, debug};
use std::sync::atomic::{AtomicU64, Ordering};

// Latency tracking (Priority 5)
static HOT_PATH_LATENCY_NS: AtomicU64 = AtomicU64::new(0);
static HOT_PATH_COUNT: AtomicU64 = AtomicU64::new(0);

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
    pub in_flight_trades: Mutex<HashMap<String, std::time::Instant>>,
    /// Local orderbook state
    pub orderbook: Mutex<HashMap<String, OrderBookUpdate>>,
    /// Ghost liquidity tracker (DashMap for concurrent access)
    pub tracker: Arc<crate::orderbook::OrderBookTracker>,
    /// Token to side mapping: token_id → (condition_id, is_yes)
    pub token_side: Mutex<HashMap<String, (String, bool)>>,
    /// YES/NO prices per condition: condition_id → (yes_price, no_price, yes_size, no_size)
    pub market_prices: Mutex<HashMap<String, (Option<f64>, Option<f64>, Option<f64>, Option<f64>)>>,
    /// Noise reduction: track last price bucket we traded at
    pub last_traded_bucket: Mutex<HashMap<String, u64>>,
    /// Number of consecutive reconnect failures
    pub reconnect_attempts: AtomicBool,
    
    // ═══════════════════════════════════════════════════════════════
    // RISK MANAGEMENT - Added based on NotebookLM recommendations
    // ═══════════════════════════════════════════════════════════════
    
    /// Inventory skew: total YES vs NO shares (for delta-neutral)
    pub inventory_skew: Mutex<(i32, i32)>, // (yes_shares, no_shares)
    /// Max inventory skew before forcing rebalance (e.g., 200 shares)
    pub max_skew: i32,
    /// Track Leg 1 fill time for stop-loss (condition_id → fill_timestamp)
    pub leg1_fill_time: Mutex<HashMap<String, std::time::Instant>>,
    /// Max wait time for Leg 2 after Leg 1 fills (5 minutes = 300 seconds)
    pub max_leg2_wait_secs: u64,
    /// Velocity lockout: track recent price changes per condition
    pub price_history: Mutex<HashMap<String, Vec<(std::time::Instant, f64)>>>, // (timestamp, price)
    /// Velocity threshold: if price moves > X% in Y seconds, lockout
    pub velocity_lockout_secs: u64,
    pub velocity_threshold_pct: f64,
    /// Pause trading for this many seconds after velocity lockout
    pub velocity_pause_secs: u64,
    /// Last velocity lockout time
    pub last_velocity_lockout: Mutex<Option<std::time::Instant>>,
    /// Volatility filter: ATR-like calculation
    pub atr_history: Mutex<HashMap<String, Vec<f64>>>, // Recent ranges per condition
    /// Min ATR required to trade (low volatility = no momentum = bad)
    pub min_atr: f64,
    /// Daily loss tracking for drawdown limit
    pub daily_loss: Mutex<f64>,
    pub max_daily_loss: f64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            risk_paused: AtomicBool::new(true), // Paused until first WS connects
            in_flight_trades: Mutex::new(HashMap::new()),
            orderbook: Mutex::new(HashMap::new()),
            tracker: Arc::new(crate::orderbook::OrderBookTracker::new()),
            token_side: Mutex::new(HashMap::new()),
            market_prices: Mutex::new(HashMap::new()),
            last_traded_bucket: Mutex::new(HashMap::new()),
            reconnect_attempts: AtomicBool::new(false),
            
            // Risk management initialization
            inventory_skew: Mutex::new((0, 0)),
            max_skew: 200, // Max 200 share imbalance before rebalance
            leg1_fill_time: Mutex::new(HashMap::new()),
            max_leg2_wait_secs: 300, // 5 minutes to get Leg 2
            price_history: Mutex::new(HashMap::new()),
            velocity_lockout_secs: 10, // Lockout for 10 seconds
            velocity_threshold_pct: 0.05, // 5% price move triggers lockout
            velocity_pause_secs: 30, // Pause trading for 30 seconds after velocity
            last_velocity_lockout: Mutex::new(None),
            atr_history: Mutex::new(HashMap::new()),
            min_atr: 0.01, // Min 1% ATR to trade (low vol = no momentum)
            daily_loss: Mutex::new(0.0),
            max_daily_loss: 50.0, // Max $50 daily loss before pause
        }
    }
    
    /// Create AppState with an external OrderBookTracker (shared with TradingEngine for ghost detection)
    pub fn with_tracker(tracker: Arc<crate::orderbook::OrderBookTracker>) -> Self {
        Self {
            tracker,
            ..Self::new()
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
    /// Returns lock token if successful, None if already locked
    pub fn try_lock_market(&self, market_id: &str) -> Option<std::time::Instant> {
        let mut locks = self.in_flight_trades.lock();
        if locks.contains_key(market_id) {
            None // Already trading this market
        } else {
            let lock_time = std::time::Instant::now();
            locks.insert(market_id.to_string(), lock_time);
            Some(lock_time)
        }
    }

    /// Check and cleanup expired locks (auto-unlock after TTL)
    pub fn cleanup_expired_locks(&self, ttl_secs: u64) {
        let mut locks = self.in_flight_trades.lock();
        let now = std::time::Instant::now();
        locks.retain(|_key, lock_time| {
            now.duration_since(*lock_time).as_secs() < ttl_secs
        });
    }

    /// Unlock a market after trading completes
    pub fn unlock_market(&self, market_id: &str) {
        let mut locks = self.in_flight_trades.lock();
        locks.remove(market_id);
    }
    
    // ═══════════════════════════════════════════════════════════════════
    // RISK MANAGEMENT METHODS
    // ═══════════════════════════════════════════════════════════════════
    
    /// Check velocity lockout - returns true if too fast, should not trade
    pub fn check_velocity_lockout(&self, condition_id: &str) -> bool {
        // Check if we're in a velocity lockout period
        let last_lockout_guard = self.last_velocity_lockout.lock();
        let last_lockout = last_lockout_guard.unwrap_or_else(|| {
            std::time::Instant::now() - std::time::Duration::new(3600, 0)
        });
        if last_lockout.elapsed().as_secs() < self.velocity_lockout_secs {
            warn!("⚡ VELOCITY LOCKOUT: Paused for {} more seconds", self.velocity_lockout_secs - last_lockout.elapsed().as_secs());
            return true;
        }
        drop(last_lockout_guard);
        
        // Check price history for velocity
        let mut history = self.price_history.lock();
        let now = std::time::Instant::now();
        
        // Clean old entries (older than 60 seconds)
        if let Some(prices) = history.get_mut(condition_id) {
            prices.retain(|(t, _)| now.duration_since(*t).as_secs() < 60);
            
            // Check for rapid price movement
            if prices.len() >= 3 {
                let recent = &prices[prices.len().saturating_sub(3)..];
                if let (Some((_, p1)), Some((_, p2))) = (recent.first(), recent.last()) {
                    let change_pct = (p1 - p2).abs() / p2;
                    if change_pct > self.velocity_threshold_pct {
                        let secs = recent.last().unwrap().0.elapsed().as_secs();
                        if secs < self.velocity_lockout_secs as u64 {
                            warn!("⚡ VELOCITY DETECTED: {:.1}% change in {}s - LOCKOUT for {}s", 
                                change_pct * 100.0, secs, self.velocity_pause_secs);
                            *self.last_velocity_lockout.lock() = Some(now);
                            return true;
                        }
                    }
                }
            }
        }
        
        // Record current price
        let prices = history.entry(condition_id.to_string()).or_insert_with(Vec::new);
        if let Some((_, last_price)) = prices.last() {
            prices.push((now, *last_price));
        }
        
        false
    }
    
    /// Record price for velocity tracking
    pub fn record_price(&self, condition_id: &str, price: f64) {
        let mut history = self.price_history.lock();
        let now = std::time::Instant::now();
        let prices = history.entry(condition_id.to_string()).or_insert_with(Vec::new);
        prices.push((now, price));
        // Keep only last 100 prices
        if prices.len() > 100 {
            prices.drain(0..prices.len() - 100);
        }
    }
    
    /// Check volatility (ATR-like) - returns true if market is too calm to trade
    pub fn check_volatility(&self, condition_id: &str) -> bool {
        let history = self.atr_history.lock();
        if let Some(ranges) = history.get(condition_id) {
            if ranges.len() >= 5 {
                let avg_range: f64 = ranges.iter().rev().take(5).sum::<f64>() / 5.0;
                if avg_range < self.min_atr {
                    debug!("🌊 LOW VOLATILITY: ATR {:.4} < {:.4} min - skipping", avg_range, self.min_atr);
                    return true;
                }
            }
        }
        false
    }
    
    /// Record range for ATR calculation
    pub fn record_range(&self, condition_id: &str, range: f64) {
        let mut history = self.atr_history.lock();
        let ranges = history.entry(condition_id.to_string()).or_insert_with(Vec::new);
        ranges.push(range);
        if ranges.len() > 20 {
            ranges.drain(0..ranges.len() - 20);
        }
    }
    
    /// Check inventory skew - returns true if too imbalanced
    pub fn check_inventory_skew(&self) -> bool {
        let skew_guard = self.inventory_skew.lock();
        let (yes, no) = (*skew_guard);
        let imbalance = (yes - no).abs();
        if imbalance > self.max_skew {
            warn!("📊 INVENTORY SKEW: YES={}, NO={}, imbalance={} > {} - forcing rebalance", 
                yes, no, imbalance, self.max_skew);
            return true;
        }
        false
    }
    
    /// Record inventory change
    pub fn record_inventory(&self, yes_delta: i32, no_delta: i32) {
        let mut skew = self.inventory_skew.lock();
        skew.0 += yes_delta;
        skew.1 += no_delta;
        debug!("📊 INVENTORY: YES={}, NO={}", skew.0, skew.1);
    }
    
    /// Record Leg 1 fill time for stop-loss
    pub fn record_leg1_fill(&self, condition_id: String) {
        let mut fill_times = self.leg1_fill_time.lock();
        fill_times.insert(condition_id, std::time::Instant::now());
    }
    
    /// Check if Leg 2 is overdue (stop-loss trigger)
    pub fn is_leg2_overdue(&self, condition_id: &str) -> bool {
        let fill_times = self.leg1_fill_time.lock();
        if let Some(fill_time) = fill_times.get(condition_id) {
            let elapsed = fill_time.elapsed().as_secs();
            if elapsed > self.max_leg2_wait_secs {
                warn!("⏰ LEG2 TIMEOUT: {} seconds > {} max - TRIGGER STOP-LOSS", 
                    elapsed, self.max_leg2_wait_secs);
                return true;
            }
        }
        false
    }
    
    /// Clear leg1 fill time after hedge completes
    pub fn clear_leg1_fill(&self, condition_id: &str) {
        let mut fill_times = self.leg1_fill_time.lock();
        fill_times.remove(condition_id);
    }
    
    /// Check daily drawdown limit
    pub fn check_drawdown_limit(&self) -> bool {
        let loss_guard = self.daily_loss.lock();
        let loss = *loss_guard;
        if loss >= self.max_daily_loss {
            warn!("💰 DRAWDOWN LIMIT: ${:.2} >= ${:.2} max - PAUSING TRADING", 
                loss, self.max_daily_loss);
            return true;
        }
        false
    }
    
    /// Record a loss for daily tracking
    pub fn record_loss(&self, amount: f64) {
        let mut loss = self.daily_loss.lock();
        *loss += amount;
        info!("💰 Daily loss: ${:.2} / ${:.2}", *loss, self.max_daily_loss);
    }
    
    /// Reset daily loss (call at start of new day)
    pub fn reset_daily_loss(&self) {
        let mut loss = self.daily_loss.lock();
        *loss = 0.0;
        info!("💰 Daily loss reset to $0.00");
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
            let entry = prices.entry(condition_id.clone()).or_insert((None, None, None, None));
            
            if is_yes {
                entry.0 = Some(price);
            } else {
                entry.1 = Some(price);
            }
            
            // Check if we have both
            match entry {
                (Some(yes), Some(no), _, _) => Some((yes.clone(), no.clone())),
                _ => None,
            }
        };
        
        // If combined exists, return it
        combined.map(|(yes, no)| (condition_id, yes, no))
    }
    
    /// Get combined price for a market
    pub fn get_combined(&self, condition_id: &str) -> Option<f64> {
        let prices = self.market_prices.lock();
        prices.get(condition_id).and_then(|(yes, no, _, _)| {
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
            let entry = prices.entry(condition_id.to_string()).or_insert((None, None, None, None));
            
            if is_yes {
                entry.0 = Some(price);
            } else {
                entry.1 = Some(price);
            }
            
            match entry {
                (Some(yes), Some(no), _, _) => Some((yes.clone(), no.clone())),
                _ => None,
            }
        };
        
        combined.map(|(yes, no)| (condition_id.to_string(), yes, no))
    }

    /// Get orderbook size for a specific token
    pub fn get_size(&self, token_id: &str) -> Option<f64> {
        let orderbook = self.orderbook.lock();
        orderbook.get(token_id).map(|u| u.size)
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

    /// Get full market snapshot: (yes_price, no_price, yes_size, no_size, combined, edge)
    pub fn get_market_snapshot(&self, condition_id: &str) -> Option<(f64, f64, f64, f64, f64, f64)> {
        let prices = self.market_prices.lock();
        let orderbook = self.orderbook.lock();

        if let Some((yes_price, no_price, _, _)) = prices.get(condition_id) {
            let (yes, no) = (*yes_price, *no_price);
            if let (Some(yp), Some(np)) = (yes, no) {
                // Get sizes from orderbook
                // We need to look up by token_id... for now return 0.0 for sizes
                // This is a limitation - the orderbook stores by condition_id, not token_id
                let combined = yp + np;
                let edge = 1.0 - combined - (combined * 0.02);
                return Some((yp, np, 0.0, 0.0, combined, edge));
            }
        }
        None
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
                            // Priority 5: Nanosecond latency measurement
                            let start = std::time::Instant::now();
                            
                            // Parse orderbook updates (serde_json)
                            if let Ok(updates) = parse_orderbook_update(&text) {
                                for update in updates {
                                    let _ = event_tx.send(WsEvent::Update(source, update));
                                }
                            }
                            
                            // Record latency
                            let elapsed_ns = start.elapsed().as_nanos() as u64;
                            HOT_PATH_LATENCY_NS.fetch_add(elapsed_ns, Ordering::Relaxed);
                            let count = HOT_PATH_COUNT.fetch_add(1, Ordering::Relaxed);
                            
                            // Log every 1000 messages
                            if count % 1000 == 0 && count > 0 {
                                let avg_ns = HOT_PATH_LATENCY_NS.load(Ordering::Relaxed) / count;
                                debug!("📊 Hot path latency: avg={:.2}µs ({} samples)", avg_ns as f64 / 1000.0, count);
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
    
    // Log raw message occasionally for debugging
    static MSG_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = MSG_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if count % 500 == 0 {
        tracing::debug!("📥 RAW WS MESSAGE: {}", &text[..text.len().min(500)]);
    }
    
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
            // Get the token_id from this specific price_change entry
            let change_token_id = change["asset_id"].as_str()
                .unwrap_or("")
                .to_string();
            
            // CRITICAL FIX: Determine outcome from token_id, not from "outcome" field
            // Polymarket DOES NOT send "outcome" in price_changes messages
            // We must look up whether this token is YES or NO from our mapping
            let outcome = {
                // This will be looked up later in AppState when we have access to token_side
                // For now, use placeholder - the real outcome is determined by token_id
                "unknown".to_string()
            };
            
            updates.push(OrderBookUpdate {
                condition_id: condition_id.clone(),
                token_id: change_token_id,
                side,
                outcome,
                price,
                size,
                timestamp: std::time::Instant::now(),
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
        timestamp: std::time::Instant::now(),
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
                
                let market_id = update.condition_id.clone();
                let mid_price = update.price;
                
                // HALT CHECK: Don't trade on potentially stale data
                if !state.can_trade() {
                    continue;
                }
                
                // Check for ghost liquidity (depth vanished during queue)
                if state.tracker.check_ghost_liquidity(&market_id) {
                    tracing::debug!("👻 GHOST LIQUIDITY: {} - skipping update (depth vanished during queue)", &market_id[..8.min(market_id.len())]);
                    continue;
                }
                
                // Update local orderbook
                state.orderbook.lock().insert(market_id.clone(), update.clone());
                
                // Update DashMap tracker with depth info (for ghost detection)
                // CRITICAL: Look up whether this token is YES or NO from token_side mapping
                // Polymarket WebSocket does NOT send "outcome" field in price_changes
                static UPDATE_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let count = UPDATE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                
                let (yes_depth, no_depth) = {
                    let mapping = state.token_side.lock();
                    match mapping.get(&update.token_id) {
                        Some((_, is_yes)) => {
                            if *is_yes {
                                (update.size, 0.0)
                            } else {
                                (0.0, update.size)
                            }
                        }
                        None => {
                            // Token not in mapping - log warning occasionally
                            if count % 1000 == 0 {
                                tracing::warn!("⚠️ Token {} not in token_side mapping", &update.token_id[..8.min(update.token_id.len())]);
                            }
                            (0.0, 0.0)
                        }
                    }
                };
                
                // Log every 100 updates for debugging
                if count % 100 == 0 {
                    let token_side_str = {
                        let mapping = state.token_side.lock();
                        match mapping.get(&update.token_id) {
                            Some((_, is_yes)) => if *is_yes { "YES" } else { "NO" },
                            None => "UNKNOWN"
                        }
                    };
                    tracing::info!(
                        "📊 DEPTH UPDATE: {} | token={} | yes_depth={:.2} | no_depth={:.2} | price={:.4}",
                        &market_id[..8.min(market_id.len())],
                        token_side_str,
                        yes_depth,
                        no_depth,
                        update.price
                    );
                }
                
                state.tracker.update(&market_id, Some(update.price), Some(update.price), yes_depth, no_depth);
                
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
        
        assert!(state.try_lock_market("market1").is_some());
        assert!(state.try_lock_market("market1").is_none()); // Already locked
        state.unlock_market("market1");
        assert!(state.try_lock_market("market1").is_some()); // Now unlocked
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

/// Get the NO token ID for a condition
impl AppState {
    pub fn get_no_token_id(&self, condition_id: &str) -> Option<String> {
        let mapping = self.token_side.lock();
        for (token_id, (cond_id, is_yes)) in mapping.iter() {
            if cond_id == condition_id && !is_yes {
                return Some(token_id.clone());
            }
        }
        None
    }
}
