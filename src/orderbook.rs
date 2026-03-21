//! OrderBook Tracker for Gabagool Strategy
//! 
//! Tracks best bid/ask for YES and NO tokens separately,
//! then computes combined cost for arbitrage detection.

use parking_lot::RwLock;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// Price level (price, size)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

/// Orderbook state for a single market's YES or NO token
#[derive(Debug, Clone)]
pub struct TokenOrderBook {
    pub token_id: String,
    pub best_bid: Option<PriceLevel>,
    pub best_ask: Option<PriceLevel>,
    pub last_update: i64,
}

impl TokenOrderBook {
    pub fn new(token_id: String) -> Self {
        Self {
            token_id,
            best_bid: None,
            best_ask: None,
            last_update: 0,
        }
    }
    
    /// Update best bid
    pub fn update_bid(&mut self, price: f64, size: f64, timestamp: i64) {
        if size > 0.0 {
            self.best_bid = Some(PriceLevel { price, size });
        } else {
            self.best_bid = None; // Level removed
        }
        self.last_update = timestamp;
    }
    
    /// Update best ask
    pub fn update_ask(&mut self, price: f64, size: f64, timestamp: i64) {
        if size > 0.0 {
            self.best_ask = Some(PriceLevel { price, size });
        } else {
            self.best_ask = None; // Level removed
        }
        self.last_update = timestamp;
    }
}

/// Complete orderbook for a market (YES + NO tokens)
#[derive(Debug, Clone)]
pub struct MarketOrderBook {
    pub market_slug: String,
    pub yes: TokenOrderBook,
    pub no: TokenOrderBook,
}

impl MarketOrderBook {
    pub fn new(market_slug: String, yes_token_id: String, no_token_id: String) -> Self {
        Self {
            market_slug,
            yes: TokenOrderBook::new(yes_token_id),
            no: TokenOrderBook::new(no_token_id),
        }
    }
    
    /// Get best combined cost for Gabagool (best YES ask + best NO ask)
    pub fn combined_cost(&self) -> Option<f64> {
        let yes_ask = self.yes.best_ask?.price;
        let no_ask = self.no.best_ask?.price;
        Some(yes_ask + no_ask)
    }
    
    /// Check if arbitrage exists (combined cost < target)
    pub fn has_arbitrage(&self, target: f64) -> bool {
        self.combined_cost()
            .map(|cost| cost < target)
            .unwrap_or(false)
    }
    
    /// Get arbitrage details
    pub fn arbitrage_details(&self, target: f64) -> Option<ArbitrageInfo> {
        let yes_ask = self.yes.best_ask?.price;
        let no_ask = self.no.best_ask?.price;
        let combined = yes_ask + no_ask;
        
        if combined < target {
            Some(ArbitrageInfo {
                yes_ask,
                no_ask,
                combined_cost: combined,
                profit_per_share: 1.0 - combined - (combined * 0.02), // After 2% fee
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageInfo {
    pub yes_ask: f64,
    pub no_ask: f64,
    pub combined_cost: f64,
    pub profit_per_share: f64,
}

/// Thread-safe orderbook tracker for multiple markets
pub struct OrderBookTracker {
    markets: RwLock<HashMap<String, MarketOrderBook>>,
}

impl OrderBookTracker {
    pub fn new() -> Self {
        Self {
            markets: RwLock::new(HashMap::new()),
        }
    }
    
    /// Get or create a market
    pub fn get_or_create(&self, market_slug: &str, yes_token_id: &str, no_token_id: &str) {
        let mut markets = self.markets.write();
        if !markets.contains_key(market_slug) {
            markets.insert(
                market_slug.to_string(),
                MarketOrderBook::new(
                    market_slug.to_string(),
                    yes_token_id.to_string(),
                    no_token_id.to_string(),
                ),
            );
        }
    }
    
    /// Update YES best ask for a market
    pub fn update_yes_ask(&self, market_slug: &str, price: f64, size: f64, timestamp: i64) {
        let mut markets = self.markets.write();
        if let Some(market) = markets.get_mut(market_slug) {
            market.yes.update_ask(price, size, timestamp);
        }
    }
    
    /// Update YES best bid for a market
    pub fn update_yes_bid(&self, market_slug: &str, price: f64, size: f64, timestamp: i64) {
        let mut markets = self.markets.write();
        if let Some(market) = markets.get_mut(market_slug) {
            market.yes.update_bid(price, size, timestamp);
        }
    }
    
    /// Update NO best ask for a market
    pub fn update_no_ask(&self, market_slug: &str, price: f64, size: f64, timestamp: i64) {
        let mut markets = self.markets.write();
        if let Some(market) = markets.get_mut(market_slug) {
            market.no.update_ask(price, size, timestamp);
        }
    }
    
    /// Update NO best bid for a market
    pub fn update_no_bid(&self, market_slug: &str, price: f64, size: f64, timestamp: i64) {
        let mut markets = self.markets.write();
        if let Some(market) = markets.get_mut(market_slug) {
            market.no.update_bid(price, size, timestamp);
        }
    }
    
    /// Get combined cost for a specific market
    pub fn get_combined_cost(&self, market_slug: &str) -> Option<f64> {
        let markets = self.markets.read();
        markets.get(market_slug).and_then(|m| m.combined_cost())
    }
    
    /// Get YES best ask for a specific market
    pub fn get_yes_ask(&self, market_slug: &str) -> Option<f64> {
        let markets = self.markets.read();
        markets.get(market_slug).and_then(|m| m.yes.best_ask.map(|p| p.price))
    }
    
    /// Get NO best ask for a specific market
    pub fn get_no_ask(&self, market_slug: &str) -> Option<f64> {
        let markets = self.markets.read();
        markets.get(market_slug).and_then(|m| m.no.best_ask.map(|p| p.price))
    }
    
    /// Check all markets for arbitrage
    pub fn find_arbitrages(&self, target: f64) -> Vec<(String, ArbitrageInfo)> {
        let markets = self.markets.read();
        let mut results = Vec::new();
        
        for (slug, market) in markets.iter() {
            if let Some(info) = market.arbitrage_details(target) {
                results.push((slug.clone(), info));
            }
        }
        
        results
    }
}

impl Default for OrderBookTracker {
    fn default() -> Self {
        Self::new()
    }
}
