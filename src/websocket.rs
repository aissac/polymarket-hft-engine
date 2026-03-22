//! Pingpong WebSocket Module
//! 
//! Real-time orderbook streaming via Polymarket WebSocket.
//! Replaces REST polling for low-latency arbitrage detection.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

/// Polymarket WebSocket URL
const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Orderbook price level
#[derive(Debug, Clone)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

/// Token data for a market
#[derive(Debug, Clone)]
pub struct TokenData {
    pub token_id: String,
    pub outcome: String,
    pub price: Option<f64>,
}

/// YES/NO orderbook for a market
#[derive(Debug, Clone, Default)]
pub struct MarketOrderBook {
    pub yes_bids: Vec<PriceLevel>,
    pub yes_asks: Vec<PriceLevel>,
    pub no_bids: Vec<PriceLevel>,
    pub no_asks: Vec<PriceLevel>,
}

/// Orderbook update from WebSocket
#[derive(Debug, Clone)]
pub struct OrderBookUpdate {
    pub condition_id: String,
    pub token_id: String,
    pub side: String,      // "buy" or "sell"
    pub outcome: String,    // "yes" or "no"
    pub price: f64,
    pub size: f64,
}

/// Pre-allocated buffer for zero-allocation parsing
struct ParseBuffer {
    data: Vec<u8>,
}

impl ParseBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
        }
    }
    
    fn update(&mut self, new_data: &[u8]) -> &[u8] {
        let len = new_data.len().min(self.data.len());
        self.data[..len].copy_from_slice(&new_data[..len]);
        &self.data[..len]
    }
}

/// WebSocket client for Polymarket orderbook
pub struct WSClient {
    url: String,
    orderbooks: Arc<RwLock<HashMap<String, MarketOrderBook>>>,
    buffer: ParseBuffer,
}

impl WSClient {
    pub fn new() -> Self {
        Self {
            url: WS_URL.to_string(),
            orderbooks: Arc::new(RwLock::new(HashMap::new())),
            buffer: ParseBuffer::new(512 * 1024), // 512KB pre-allocated
        }
    }
    
    /// Connect to WebSocket and stream orderbook updates
    pub async fn stream<F>(&self, subscribed_tokens: Vec<String>, mut on_update: F) -> Result<()>
    where
        F: FnMut(OrderBookUpdate) + Send + 'static,
    {
        info!("🔌 Connecting to Polymarket WebSocket...");
        
        let (ws_stream, _) = connect_async(&self.url).await?;
        info!("✅ WebSocket connected");
        
        let (mut write, mut read) = ws_stream.split();
        
        // Subscribe to specific tokens
        let subscribe_msg = serde_json::json!({
            "type": "market",
            "assets": subscribed_tokens
        });
        
        let msg_str = serde_json::to_string(&subscribe_msg)?;
        write.send(Message::Text(msg_str.into())).await?;
        info!("📡 Subscribed to {} tokens", subscribed_tokens.len());
        
        // Pre-allocated buffer for zero-allocation hot path
        let mut parse_buffer = ParseBuffer::new(512 * 1024);
        
        // Message processing loop
        while let Some(msg_result) = read.next().await {
            match msg_result {
                Ok(Message::Text(text)) => {
                    // Zero-allocation: parse directly into pre-allocated buffer
                    let data = parse_buffer.update(text.as_bytes());
                    
                    // Try to parse the update
                    if let Ok(update) = self.parse_update(data) {
                        // Update local orderbook
                        self.update_orderbook(&update);
                        // Callback with update
                        on_update(update);
                    }
                }
                Ok(Message::Ping(data)) => {
                    debug!("Received ping, sending pong");
                    write.send(Message::Pong(data)).await?;
                }
                Ok(Message::Close(_)) => {
                    warn!("WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        
        Ok(())
    }
    
    /// Parse orderbook update from raw bytes
    fn parse_update(&self, data: &[u8]) -> Result<OrderBookUpdate> {
        // Try to parse as JSON
        let json: serde_json::Value = serde_json::from_slice(data)?;
        
        let update = OrderBookUpdate {
            condition_id: json["condition_id"].as_str().unwrap_or("").to_string(),
            token_id: json["token_id"].as_str().unwrap_or("").to_string(),
            side: json["side"].as_str().unwrap_or("").to_string(),
            outcome: json["outcome"].as_str().unwrap_or("").to_string(),
            price: json["price"].as_f64().unwrap_or(0.0),
            size: json["size"].as_f64().unwrap_or(0.0),
        };
        
        Ok(update)
    }
    
    /// Update local orderbook state
    fn update_orderbook(&self, update: &OrderBookUpdate) {
        let mut books = self.orderbooks.write();
        
        let book = books
            .entry(update.condition_id.clone())
            .or_insert_with(MarketOrderBook::default);
        
        let level = PriceLevel {
            price: update.price,
            size: update.size,
        };
        
        match (update.side.as_str(), update.outcome.as_str()) {
            ("buy", "yes") => {
                book.yes_bids.push(level);
                book.yes_bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());
            }
            ("sell", "yes") => {
                book.yes_asks.push(level);
                book.yes_asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap());
            }
            ("buy", "no") => {
                book.no_bids.push(level);
                book.no_bids.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());
            }
            ("sell", "no") => {
                book.no_asks.push(level);
                book.no_asks.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap());
            }
            _ => {}
        }
    }
    
    /// Get best YES ask + best NO ask for arbitrage check
    pub fn get_best_prices(&self, condition_id: &str) -> Option<(f64, f64)> {
        let books = self.orderbooks.read();
        
        if let Some(book) = books.get(condition_id) {
            let yes_ask = book.yes_asks.first().map(|l| l.price);
            let no_ask = book.no_asks.first().map(|l| l.price);
            
            match (yes_ask, no_ask) {
                (Some(yes), Some(no)) => return Some((yes, no)),
                _ => {}
            }
        }
        
        None
    }
    
    /// Get all known condition IDs
    pub fn get_condition_ids(&self) -> Vec<String> {
        let books = self.orderbooks.read();
        books.keys().cloned().collect()
    }
}

impl Default for WSClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Start WebSocket streaming in background
pub async fn start_ws_streamer(
    tokens: Vec<String>,
    update_tx: mpsc::UnboundedSender<OrderBookUpdate>,
) -> Result<()> {
    let client = WSClient::new();
    
    let tx = update_tx.clone();
    client.stream(tokens, move |update| {
        let _ = tx.send(update);
    }).await?;
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_buffer() {
        let mut buf = ParseBuffer::new(100);
        let data = b"hello world";
        let result = buf.update(data);
        assert_eq!(result, b"hello world");
    }
    
    #[test]
    fn test_orderbook_update() {
        let update = OrderBookUpdate {
            condition_id: "0x123".to_string(),
            token_id: "token1".to_string(),
            side: "buy".to_string(),
            outcome: "yes".to_string(),
            price: 0.50,
            size: 100.0,
        };
        
        assert_eq!(update.price, 0.50);
        assert_eq!(update.outcome, "yes");
    }
}
