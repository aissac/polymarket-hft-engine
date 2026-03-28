// Binance WebSocket Module for BTC/ETH Price Feed
// Used for T-10s sniper strategy and directional momentum

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::VecDeque;
use tokio::sync::mpsc;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// Binance Book Ticker payload
#[derive(Debug, Deserialize)]
pub struct BookTicker {
    #[serde(rename = "s")]
    pub symbol: String,      // "BTCUSDT"
    #[serde(rename = "b")]
    pub best_bid: String,    // Best bid price
    #[serde(rename = "B")]
    pub best_bid_qty: String,
    #[serde(rename = "a")]
    pub best_ask: String,    // Best ask price
    #[serde(rename = "A")]
    pub best_ask_qty: String,
}

/// Price point for momentum calculation
#[derive(Debug, Clone)]
pub struct PricePoint {
    pub timestamp: i64,
    pub mid_price: f64,
}

/// Momentum result
#[derive(Debug, Clone)]
pub struct Momentum {
    pub symbol: String,
    pub direction: f64,      // -1.0 to +1.0
    pub confidence: f64,     // 0.0 to 1.0
    pub distance_to_strike: f64,  // For T-10s sniper
    pub price: f64,          // Current price
}

/// Binance WebSocket Actor
pub struct BinanceActor {
    /// Ring buffer for last 290 seconds of prices (4m50s)
    price_history: VecDeque<PricePoint>,
    /// Current best prices
    best_bid: f64,
    best_ask: f64,
    /// Symbol being tracked
    symbol: String,
    /// Channel to send momentum updates
    momentum_tx: mpsc::UnboundedSender<Momentum>,
}

impl BinanceActor {
    pub fn new(symbol: String, momentum_tx: mpsc::UnboundedSender<Momentum>) -> Self {
        Self {
            price_history: VecDeque::with_capacity(1000),
            best_bid: 0.0,
            best_ask: 0.0,
            symbol,
            momentum_tx,
        }
    }

    /// Calculate mid price from bid/ask
    fn calculate_mid(&self) -> f64 {
        if self.best_bid > 0.0 && self.best_ask > 0.0 {
            (self.best_bid + self.best_ask) / 2.0
        } else {
            0.0
        }
    }

    /// Add price point to history
    fn add_price(&mut self, timestamp: i64, mid_price: f64) {
        // Keep only last 290 seconds (4m50s)
        let cutoff = timestamp - 290_000;
        while let Some(front) = self.price_history.front() {
            if front.timestamp < cutoff {
                self.price_history.pop_front();
            } else {
                break;
            }
        }
        self.price_history.push_back(PricePoint { timestamp, mid_price });
    }

    /// Calculate EMA over given period (in seconds)
    fn calculate_ema(&self, period_seconds: i64, now: i64) -> Option<f64> {
        let cutoff = now - period_seconds * 1000;
        let prices: Vec<f64> = self.price_history
            .iter()
            .filter(|p| p.timestamp >= cutoff)
            .map(|p| p.mid_price)
            .collect();

        if prices.is_empty() {
            return None;
        }

        let alpha = 2.0 / (prices.len() as f64 + 1.0);
        let mut ema = prices[0];
        for price in prices.iter().skip(1) {
            ema = alpha * price + (1.0 - alpha) * ema;
        }
        Some(ema)
    }

    /// Calculate momentum and confidence
    fn calculate_momentum(&self) -> Option<Momentum> {
        let now = Utc::now().timestamp_millis();
        let mid = self.calculate_mid();
        
        if mid == 0.0 {
            return None;
        }

        // Calculate 5s and 60s EMAs
        let ema_5 = self.calculate_ema(5, now)?;
        let ema_60 = self.calculate_ema(60, now)?;

        // Direction: -1 to +1
        let direction = if ema_60 > 0.0 {
            (ema_5 - ema_60) / ema_60
        } else {
            0.0
        };

        // Confidence: based on how long the trend has persisted
        let price_count_5s = self.price_history
            .iter()
            .filter(|p| p.timestamp >= now - 5000)
            .count();
        let price_count_60s = self.price_history
            .iter()
            .filter(|p| p.timestamp >= now - 60000)
            .count();

        let confidence = if price_count_60s > 10 {
            (price_count_5s as f64 / price_count_60s as f64).min(1.0)
        } else {
            0.0
        };

        Some(Momentum {
            symbol: self.symbol.clone(),
            direction: direction.clamp(-1.0, 1.0),
            confidence,
            distance_to_strike: 0.0, // Set by caller
            price: mid,
        })
    }

    /// Process incoming book ticker message
    async fn process_message(&mut self, msg: &str) -> Option<Momentum> {
        // Parse book ticker using serde_json
        let ticker: BookTicker = match serde_json::from_str(msg) {
            Ok(t) => t,
            Err(_) => return None,
        };

        // Parse prices
        self.best_bid = ticker.best_bid.parse().ok()?;
        self.best_ask = ticker.best_ask.parse().ok()?;

        // Calculate mid and add to history
        let mid = self.calculate_mid();
        let now = Utc::now().timestamp_millis();
        self.add_price(now, mid);

        // Calculate momentum
        self.calculate_momentum()
    }

    /// Run the WebSocket actor
    pub async fn run(&mut self) {
        let url = format!(
            "wss://stream.binance.com:9443/ws/{}@bookTicker",
            self.symbol.to_lowercase()
        );

        info!("🔌 Connecting to Binance: {}", url);

        loop {
            match connect_async(&url).await {
                Ok((ws_stream, _)) => {
                    info!("✅ Binance WebSocket connected for {}", self.symbol);
                    
                    let (mut write, mut read) = ws_stream.split();

                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Text(text)) => {
                                if let Some(momentum) = self.process_message(&text).await {
                                    // Send momentum update
                                    if self.momentum_tx.send(momentum).is_err() {
                                        warn!("Momentum channel closed");
                                        break;
                                    }
                                }
                            }
                            Ok(Message::Ping(data)) => {
                                let _ = write.send(Message::Pong(data)).await;
                            }
                            Ok(Message::Close(_)) => {
                                warn!("Binance WebSocket closed, reconnecting...");
                                break;
                            }
                            Err(e) => {
                                warn!("Binance WebSocket error: {}, reconnecting...", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to connect to Binance: {}, retrying in 5s...", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }
}

/// Spawn Binance WebSocket actor
pub fn spawn_binance_actor(
    symbol: String,
) -> mpsc::UnboundedReceiver<Momentum> {
    let (tx, rx) = mpsc::unbounded_channel();
    
    tokio::spawn(async move {
        let mut actor = BinanceActor::new(symbol, tx);
        actor.run().await;
    });

    rx
}