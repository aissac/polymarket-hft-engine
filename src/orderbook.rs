//! OrderBook Tracker for Pingpong Strategy
//! 
//! Thread-safe tracker for YES/NO prices across multiple markets.

use parking_lot::RwLock;
use std::collections::HashMap;

/// Arbitrage opportunity found
#[derive(Debug, Clone)]
pub struct ArbitrageInfo {
    pub yes_ask: f64,
    pub no_ask: f64,
    pub combined_cost: f64,
    pub profit_per_share: f64,
}

/// Market price data
#[derive(Debug, Clone)]
pub struct MarketPrices {
    pub condition_id: String,
    pub yes_best_bid: Option<f64>,
    pub yes_best_ask: Option<f64>,
    pub no_best_bid: Option<f64>,
    pub no_best_ask: Option<f64>,
    pub last_update: i64,
}

impl MarketPrices {
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
    
    pub fn arbitrage_info(&self, target: f64, fee_pct: f64) -> Option<ArbitrageInfo> {
        let (yes_ask, no_ask) = (self.yes_best_ask?, self.no_best_ask?);
        let combined = yes_ask + no_ask;
        
        if combined < target {
            let payout = 1.0;
            let fee = payout * fee_pct;
            let profit = payout - fee - combined;
            Some(ArbitrageInfo {
                yes_ask,
                no_ask,
                combined_cost: combined,
                profit_per_share: profit,
            })
        } else {
            None
        }
    }
}

/// Thread-safe orderbook tracker
pub struct OrderBookTracker {
    markets: RwLock<HashMap<String, MarketPrices>>,
}

impl OrderBookTracker {
    pub fn new() -> Self {
        Self {
            markets: RwLock::new(HashMap::new()),
        }
    }
    
    /// Update prices for a market
    pub fn update(&self, condition_id: &str, yes_ask: Option<f64>, no_ask: Option<f64>) {
        let mut markets = self.markets.write();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        
        if let Some(existing) = markets.get_mut(condition_id) {
            existing.yes_best_ask = yes_ask.or(existing.yes_best_ask);
            existing.no_best_ask = no_ask.or(existing.no_best_ask);
            existing.last_update = timestamp;
        } else {
            markets.insert(condition_id.to_string(), MarketPrices {
                condition_id: condition_id.to_string(),
                yes_best_bid: None,
                yes_best_ask: yes_ask,
                no_best_bid: None,
                no_best_ask: no_ask,
                last_update: timestamp,
            });
        }
    }
    
    /// Get combined cost for a market
    pub fn get_combined_cost(&self, condition_id: &str) -> Option<f64> {
        let markets = self.markets.read();
        markets.get(condition_id).and_then(|m| m.combined_cost())
    }
    
    /// Find all arbitrage opportunities
    pub fn find_arbitrages(&self, target: f64) -> Vec<(String, ArbitrageInfo)> {
        let markets = self.markets.read();
        let mut results = Vec::new();
        
        for (condition_id, market) in markets.iter() {
            if let Some(info) = market.arbitrage_info(target, 0.02) {
                results.push((condition_id.clone(), info));
            }
        }
        
        results
    }
    
    /// Get all tracked markets
    pub fn get_all_markets(&self) -> Vec<(String, Option<f64>)> {
        let markets = self.markets.read();
        markets.iter()
            .map(|(id, m)| (id.clone(), m.combined_cost()))
            .collect()
    }
}

impl Default for OrderBookTracker {
    fn default() -> Self {
        Self::new()
    }
}
