//! WebSocket Client for Polymarket CLOB
//! 
//! Connects to Polymarket's WebSocket API and subscribes to orderbook updates.
//! Phase 1: Read-only - just tracking prices, no trading.

use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tokio_tungstenite::tungstenite::http::Request;
use tracing::{info, warn, error, debug};

use crate::orderbook::OrderBookTracker;
use crate::strategy::PingpongStrategy;

const TARGET_HEDGE_SUM: f64 = 0.95;

/// Polymarket WebSocket URL
const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws";

/// Subscription message to send after connect
#[derive(Debug, Serialize)]
struct SubscribeMessage {
    #[serde(rename = "type")]
    msg_type: String,
    channel: String,
    #[serde(rename = "request_id")]
    request_id: String,
    #[serde(rename = "jsonrpc")]
    jsonrpc: String,
}

impl SubscribeMessage {
    fn new(channel: &str, request_id: &str) -> Self {
        Self {
            msg_type: "subscribe".to_string(),
            channel: channel.to_string(),
            request_id: request_id.to_string(),
            jsonrpc: "2.0".to_string(),
        }
    }
}

/// Orderbook update from Polymarket WebSocket
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
pub enum CLOBMessage {
    /// Orderbook update with price levels
    #[serde(rename = "orderbook")]
    Orderbook(OrderbookUpdate),
    
    /// Trade/fill notification
    #[serde(rename = "auction")]
    Auction(AuctionUpdate),
    
    /// Subscription confirmed
    #[serde(rename = "subscribe")]
    Subscribed(SubscriptionStatus),
    
    /// Error response
    #[serde(rename = "error")]
    Error(ErrorResponse),
    
    /// Pong (heartbeat response)
    #[serde(rename = "pong")]
    Pong,
    
    /// Unknown message type
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, Clone)]
pub struct OrderbookUpdate {
    pub market_slug: Option<String>,
    pub token_id: Option<String>,
    pub side: Option<String>,  // "buy" or "sell"
    #[serde(rename = "pricePoints")]
    pub price_points: Option<Vec<PricePoint>>,
    pub ts: Option<i64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PricePoint {
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuctionUpdate {
    pub market_slug: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SubscriptionStatus {
    pub channel: Option<String>,
    pub request_id: Option<String>,
    pub success: Option<bool>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ErrorResponse {
    pub code: Option<i32>,
    pub message: Option<String>,
}

/// Polymarket WebSocket Client
pub struct PolymarketWsClient {
    ws_url: String,
    tracker: Arc<OrderBookTracker>,
    running: Arc<parking_lot::RwLock<bool>>,
}

impl PolymarketWsClient {
    pub fn new(tracker: Arc<OrderBookTracker>) -> Self {
        Self {
            ws_url: WS_URL.to_string(),
            tracker,
            running: Arc::new(parking_lot::RwLock::new(false)),
        }
    }
    
    /// Connect and start the WebSocket reader loop
    pub async fn connect(&self) -> Result<()> {
        info!("🔌 Connecting to Polymarket WebSocket: {}", self.ws_url);
        
        let request = Request::builder()
            .uri(&self.ws_url)
            .header("Sec-WebSocket-Protocol", "graphql-ws")
            .body(())?;
        
        let (ws_stream, _) = connect_async(request).await?;
        
        {
            let mut running = self.running.write();
            *running = true;
        }
        
        info!("✅ Connected! Starting message loop...");
        
        let (mut write, mut read) = ws_stream.split();
        
        // Subscribe to orderbook channel
        let subscribe_msg = SubscribeMessage::new("orderbook", "orderbook-sub-1");
        let json = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(json.into())).await?;
        
        info!("📡 Subscribed to orderbook channel");
        
        // Message loop
        let tracker = self.tracker.clone();
        let running = self.running.clone();
        
        tokio::spawn(async move {
            let mut heartbeat_count = 0u64;
            
            while *running.read() {
                tokio::select! {
                    // Incoming messages
                    msg = read.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                Self::handle_message(&tracker, &text).await;
                            }
                            Some(Ok(Message::Ping(data))) => {
                                // Respond to ping with pong
                                if let Ok(pong) = String::from_utf8(data.to_vec()) {
                                    debug!("Received ping: {}", pong);
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                // Pong received - connection alive
                            }
                            Some(Ok(Message::Close(_))) => {
                                warn!("WebSocket closed by server");
                                break;
                            }
                            Some(Err(e)) => {
                                error!("WebSocket error: {}", e);
                                break;
                            }
                            None => {
                                warn!("WebSocket stream ended");
                                break;
                            }
                            _ => {}
                        }
                    }
                    // Heartbeat every 30 seconds
                    _ = tokio::time::sleep(tokio::time::Duration::from_secs(30)) => {
                        heartbeat_count += 1;
                        debug!("❤️ Heartbeat #{}", heartbeat_count);
                        // Try to send a ping
                        if let Err(e) = write.send(Message::Ping(vec![].into())).await {
                            warn!("Failed to send ping: {}", e);
                        }
                    }
                }
            }
            
            {
                let mut running = running.write();
                *running = false;
            }
            
            warn!("WebSocket reader loop ended");
        });
        
        Ok(())
    }
    
    /// Handle incoming WebSocket message
    async fn handle_message(tracker: &Arc<OrderBookTracker>, text: &str) {
        // Try to parse as CLOBMessage
        let msg: CLOBMessage = match serde_json::from_str(text) {
            Ok(msg) => msg,
            Err(_) => {
                // Not a JSON message we understand
                return;
            }
        };
        
        match msg {
            CLOBMessage::Orderbook(update) => {
                Self::handle_orderbook_update(tracker, update).await;
            }
            CLOBMessage::Subscribed(status) => {
                if let Some(success) = status.success {
                    if success {
                        info!("✅ Subscription confirmed for channel: {:?}", status.channel);
                    } else {
                        warn!("⚠️ Subscription failed: {:?}", status);
                    }
                }
            }
            CLOBMessage::Error(err) => {
                error!("❌ WebSocket error: {:?}", err);
            }
            CLOBMessage::Pong => {
                // Heartbeat acknowledged
            }
            _ => {
                // Ignore other message types in Phase 1
            }
        }
    }
    
    /// Handle orderbook update
    async fn handle_orderbook_update(tracker: &Arc<OrderBookTracker>, update: OrderbookUpdate) {
        let market_slug = match &update.market_slug {
            Some(s) => s.clone(),
            None => return,
        };
        
        let token_id = match &update.token_id {
            Some(t) => t.clone(),
            None => return,
        };
        
        let side = match &update.side {
            Some(s) => s.clone(),
            None => return,
        };
        
        let price_points = match update.price_points {
            Some(pp) => pp,
            None => return,
        };
        
        let timestamp = update.ts.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
        });
        
        // Get best price point (first one is best)
        if let Some(best) = price_points.first() {
            // Determine YES or NO based on token_id convention
            // Convention: YES tokens end with 'yes' or we check the orderbook
            let is_yes = token_id.contains("yes") || token_id.ends_with("1") || token_id.ends_with("0");
            
            if side == "sell" || side == "buy" {
                // For asks (sell side), we care about the best ask
                if side == "sell" {
                    if is_yes {
                        tracker.update_yes_ask(&market_slug, best.price, best.size, timestamp);
                    } else {
                        tracker.update_no_ask(&market_slug, best.price, best.size, timestamp);
                    }
                } else {
                    // Bids (buy side)
                    if is_yes {
                        tracker.update_yes_bid(&market_slug, best.price, best.size, timestamp);
                    } else {
                        tracker.update_no_bid(&market_slug, best.price, best.size, timestamp);
                    }
                }
            }
            
            // Check for arbitrage opportunity
            if let Some(combined) = tracker.get_combined_cost(&market_slug) {
                if combined < TARGET_HEDGE_SUM {
                    if let Some(yes_ask) = tracker.get_yes_ask(&market_slug) {
                        if let Some(no_ask) = tracker.get_no_ask(&market_slug) {
                            let profit = 1.0 - combined - (combined * 0.02);
                            info!(
                                "🎯 ARBITRAGE DETECTED! {}: YES@${:.4} + NO@${:.4} = ${:.4} (profit: ${:.4}/share)",
                                market_slug, yes_ask, no_ask, combined, profit
                            );
                        }
                    }
                }
            }
        }
    }
    
    /// Stop the WebSocket connection
    pub fn stop(&self) {
        let mut running = self.running.write();
        *running = false;
        info!("🛑 WebSocket client stopping...");
    }
    
    /// Check if connected
    pub fn is_running(&self) -> bool {
        *self.running.read()
    }
}
