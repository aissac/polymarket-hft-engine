//! OrderBook Tracker for Pingpong Strategy - Phase 2: DashMap for Ghost Detection
//! 
//! Thread-safe orderbook tracker using DashMap for concurrent access.
//! Enables real-time ghost liquidity detection during 500ms taker queue.

use dashmap::DashMap;
use std::sync::Arc;

/// Market price data with depth tracking
#[derive(Debug, Clone)]
pub struct MarketPrices {
    pub condition_id: String,
    pub yes_best_bid: Option<f64>,
    pub yes_best_ask: Option<f64>,
    pub no_best_bid: Option<f64>,
    pub no_best_ask: Option<f64>,
    pub yes_depth: f64,  // Size at best ask
    pub no_depth: f64,   // Size at best ask
    pub last_update: i64,
    /// Track depth changes during 500ms queue (ghost detection)
    pub queue_start_yes_depth: Option<f64>,
    pub queue_start_no_depth: Option<f64>,
}

impl MarketPrices {
    pub fn new(condition_id: String) -> Self {
        Self {
            condition_id,
            yes_best_bid: None,
            yes_best_ask: None,
            no_best_bid: None,
            no_best_ask: None,
            yes_depth: 0.0,
            no_depth: 0.0,
            last_update: 0,
            queue_start_yes_depth: None,
            queue_start_no_depth: None,
        }
    }

    pub fn combined_cost(&self) -> Option<f64> {
        match (self.yes_best_ask, self.no_best_ask) {
            (Some(yes), Some(no)) => Some(yes + no),
            _ => None,
        }
    }
    
    pub fn has_arbitrage(&self, target: f64) -> bool {
        self.combined_cost()
            .map(|c| c < target)
            .unwrap_or(false)
    }

    /// Check if liquidity vanished during queue (ghost detection)
    pub fn liquidity_vanished_during_queue(&self) -> bool {
        match (self.queue_start_yes_depth, self.queue_start_no_depth) {
            (Some(start_yes), Some(start_no)) => {
                // If depth dropped by >50%, liquidity vanished (maker pulled quote)
                let yes_vanished = self.yes_depth < start_yes * 0.5;
                let no_vanished = self.no_depth < start_no * 0.5;
                yes_vanished || no_vanished
            }
            _ => false,
        }
    }

    /// Record depth at start of 500ms queue
    pub fn mark_queue_start(&mut self) {
        self.queue_start_yes_depth = Some(self.yes_depth);
        self.queue_start_no_depth = Some(self.no_depth);
    }

    /// Clear queue markers after execution
    pub fn clear_queue_markers(&mut self) {
        self.queue_start_yes_depth = None;
        self.queue_start_no_depth = None;
    }
}

/// Thread-safe orderbook tracker using DashMap (lock-free concurrent access)
pub struct OrderBookTracker {
    markets: Arc<DashMap<String, MarketPrices>>,
}

impl OrderBookTracker {
    pub fn new() -> Self {
        Self {
            markets: Arc::new(DashMap::new()),
        }
    }
    
    /// Update prices for a market
    pub fn update(&self, condition_id: &str, yes_ask: Option<f64>, no_ask: Option<f64>, yes_depth: f64, no_depth: f64) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        
        let mut market = self.markets
            .entry(condition_id.to_string())
            .or_insert_with(|| MarketPrices::new(condition_id.to_string()));
        
        market.yes_best_ask = yes_ask;
        market.no_best_ask = no_ask;
        market.yes_depth = yes_depth;
        market.no_depth = no_depth;
        market.last_update = timestamp;
    }

    /// Get market prices (read-only, lock-free)
    pub fn get(&self, condition_id: &str) -> Option<MarketPrices> {
        self.markets.get(condition_id).map(|r| r.clone())
    }

    /// Mark queue start for ghost detection
    pub fn mark_queue_start(&self, condition_id: &str) {
        if let Some(mut market) = self.markets.get_mut(condition_id) {
            market.mark_queue_start();
        }
    }

    /// Check if liquidity vanished during queue
    pub fn check_ghost_liquidity(&self, condition_id: &str) -> bool {
        self.markets
            .get(condition_id)
            .map(|r| r.liquidity_vanished_during_queue())
            .unwrap_or(false)
    }

    /// Clear queue markers after execution
    pub fn clear_queue_markers(&self, condition_id: &str) {
        if let Some(mut market) = self.markets.get_mut(condition_id) {
            market.clear_queue_markers();
        }
    }

    /// Get all markets (for reporting)
    pub fn get_all_markets(&self) -> Vec<MarketPrices> {
        self.markets.iter().map(|r| r.clone()).collect()
    }

    /// Count active markets
    pub fn market_count(&self) -> usize {
        self.markets.len()
    }
}

impl Default for OrderBookTracker {
    fn default() -> Self {
        Self::new()
    }
}
