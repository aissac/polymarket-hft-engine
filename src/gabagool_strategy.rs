//! Pingpong Strategy Module - Gabagool Variant
//! 
//! Complete-set arbitrage with expensive-side skew and directional betting.
//! Based on RD Olivaw's proven strategy.
//!
//! Key features:
//! - 0.1% minimum edge (combined <= $0.999)
//! - 3x skew when price > $0.55 (expensive side)
//! - Directional betting when confidence > 70%
//! - Position limits: max 150 shares imbalance
//! - Taker mode when edge > 5 cents

use std::collections::VecDeque;
use std::sync::Arc;
use parking_lot::Mutex;
use chrono::{DateTime, Utc};

use crate::api;
use crate::orderbook::OrderBookTracker;
use crate::trading::{ArbitrageSignal, TradingEngine};
use tokio::sync::mpsc;

/// Configuration for the Gabagool variant strategy
#[derive(Debug, Clone)]
pub struct GabagoolConfig {
    // Thresholds
    pub min_edge: f64,                    // 0.001 (0.1%)
    pub expensive_threshold: f64,         // 0.55
    pub expensive_multiplier: f64,         // 3.0
    pub skip_threshold: f64,              // 1.02
    pub taker_max_edge: f64,             // 0.05 (5 cents)
    pub taker_max_spread: f64,           // 0.10 (10 cents)
    
    // Position limits
    pub max_position_imbalance: i32,     // 150 shares
    pub quote_size: f64,                // $5 per order
    pub quote_size_bankroll_fraction: f64, // 0.05 (5%)
    pub max_bankroll_fraction: f64,     // 1.0 (100%)
    
    // Directional betting
    pub directional_enabled: bool,
    pub directional_confidence_threshold: f64, // 0.70 (70%)
    pub directional_min_edge: f64,      // 0.002
    
    // Top-up
    pub top_up_enabled: bool,
    pub top_up_min_shares: i32,        // 80
    pub top_up_seconds_to_end: i32,    // 120
    
    // Timing
    pub min_seconds_to_end: i32,      // 0
    pub max_seconds_to_end: i32,       // 3600
    pub tick_interval_ms: u64,        // 500
}

impl Default for GabagoolConfig {
    fn default() -> Self {
        Self {
            min_edge: 0.001,           // 0.1%
            expensive_threshold: 0.55,
            expensive_multiplier: 3.0,
            skip_threshold: 1.02,
            taker_max_edge: 0.05,
            taker_max_spread: 0.10,
            max_position_imbalance: 150,
            quote_size: 5.0,
            quote_size_bankroll_fraction: 0.05,
            max_bankroll_fraction: 1.0,
            directional_enabled: true,
            directional_confidence_threshold: 0.70,
            directional_min_edge: 0.002,
            top_up_enabled: true,
            top_up_min_shares: 80,
            top_up_seconds_to_end: 120,
            min_seconds_to_end: 0,
            max_seconds_to_end: 3600,
            tick_interval_ms: 500,
        }
    }
}

/// Position state
#[derive(Debug, Clone)]
pub struct Position {
    pub yes_shares: i32,
    pub no_shares: i32,
    pub in_flight_yes: i32,
    pub in_flight_no: i32,
}

impl Position {
    pub fn new() -> Self {
        Self {
            yes_shares: 0,
            no_shares: 0,
            in_flight_yes: 0,
            in_flight_no: 0,
        }
    }
    
    pub fn imbalance(&self) -> i32 {
        (self.yes_shares + self.in_flight_yes - (self.no_shares + self.in_flight_no)).abs()
    }
    
    pub fn net_position(&self) -> i32 {
        (self.yes_shares + self.in_flight_yes) - (self.no_shares + self.in_flight_no)
    }
}

/// Price tick for velocity calculation
#[derive(Debug, Clone)]
struct PriceTick {
    timestamp: i64,
    mid_price: f64,
}

/// Confidence calculation result
#[derive(Debug, Clone)]
pub struct ConfidenceScore {
    pub orderbook_imbalance: f64,  // 0.0 to 1.0
    pub momentum: f64,             // -1.0 to 1.0
    pub edge: f64,                // calculated edge
    pub confidence: f64,           // 0.0 to 1.0
}

/// Trading signal with direction
#[derive(Debug, Clone)]
pub enum TradingSignal {
    // Complete set arbitrage (buy both)
    Arbitrage {
        yes_price: f64,
        no_price: f64,
        combined: f64,
        edge: f64,
        skew_multiplier: f64,
    },
    // Directional (bet on one side)
    Directional {
        side: Side,
        price: f64,
        confidence: f64,
        edge: f64,
        skew_multiplier: f64,
    },
    // Taker (cross spread)
    Taker {
        side: Side,
        price: f64,
        edge: f64,
    },
    // Top-up (rebalance)
    TopUp {
        side: Side,
        shares: i32,
    },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Yes,
    No,
}

/// Main strategy engine
pub struct GabagoolStrategy {
    config: GabagoolConfig,
    position: Arc<Mutex<Position>>,
    price_history: Arc<Mutex<VecDeque<PriceTick>>>,
    trading_tx: Option<mpsc::UnboundedSender<ArbitrageSignal>>,
}

impl GabagoolStrategy {
    pub fn new(config: GabagoolConfig) -> Self {
        Self {
            config,
            position: Arc::new(Mutex::new(Position::new())),
            price_history: Arc::new(Mutex::new(VecDeque::new())),
            trading_tx: None,
        }
    }
    
    pub fn with_trading_channel(mut self, tx: mpsc::UnboundedSender<ArbitrageSignal>) -> Self {
        self.trading_tx = Some(tx);
        self
    }
    
    /// Calculate if price is on expensive side (> threshold)
    fn is_expensive(&self, price: f64) -> bool {
        price > self.config.expensive_threshold
    }
    
    /// Get skew multiplier based on price
    fn get_skew_multiplier(&self, price: f64) -> f64 {
        if self.is_expensive(price) {
            self.config.expensive_multiplier
        } else {
            1.0
        }
    }
    
    /// Calculate confidence score based on market microstructure
    fn calculate_confidence(&self, yes_price: f64, no_price: f64) -> ConfidenceScore {
        // Orderbook imbalance (assume mid prices represent this)
        let yes_prob = yes_price;
        let no_prob = no_price;
        let imbalance = yes_prob.max(no_prob) / (yes_prob + no_prob);
        
        // Edge calculation (difference from fair value)
        let fair_value = 0.50;
        let edge = (yes_prob - fair_value).abs().max(no_prob - fair_value);
        
        // Momentum from price history
        let mut momentum = 0.0;
        let history = self.price_history.lock();
        if history.len() >= 2 {
            let recent: Vec<f64> = history.iter().rev().take(5).map(|t| t.mid_price).collect();
            if recent.len() >= 2 {
                let first = recent[recent.len() - 1];
                let last = recent[0];
                momentum = (last - first) / first;
            }
        }
        
        // Combined confidence (weighted average)
        let raw_confidence = (imbalance * 0.6) + (momentum.abs() * 0.4);
        let confidence = raw_confidence.clamp(0.0, 1.0);
        
        ConfidenceScore {
            orderbook_imbalance: imbalance,
            momentum,
            edge,
            confidence,
        }
    }
    
    /// Record a price tick for momentum calculation
    pub fn record_tick(&self, mid_price: f64) {
        let tick = PriceTick {
            timestamp: Utc::now().timestamp_millis(),
            mid_price,
        };
        
        let mut history = self.price_history.lock();
        history.push_back(tick);
        
        // Keep last 60 seconds of history
        let cutoff = Utc::now().timestamp_millis() - 60000;
        while history.front().map(|t| t.timestamp < cutoff).unwrap_or(false) {
            history.pop_front();
        }
        
        // Limit size
        if history.len() > 120 {
            history.pop_front();
        }
    }
    
    /// Evaluate market and generate trading signal
    pub fn evaluate(&self, market: &api::SimplifiedMarket) -> TradingSignal {
        let yes_price = market.yes_price().unwrap_or(0.0);
        let no_price = market.no_price().unwrap_or(0.0);
        
        self.evaluate_prices(yes_price, no_price)
    }
    
    /// Evaluate raw prices and generate trading signal
    pub fn evaluate_prices(&self, yes_price: f64, no_price: f64) -> TradingSignal {
        if yes_price <= 0.0 || no_price <= 0.0 {
            return TradingSignal::None;
        }
        
        let combined = yes_price + no_price;
        let edge = 1.0 - combined;
        
        // Record tick for momentum
        let mid_price = (yes_price + no_price) / 2.0;
        self.record_tick(mid_price);
        
        // Skip if too expensive
        if combined > self.config.skip_threshold {
            return TradingSignal::None;
        }
        
        // Check position limits
        let pos = self.position.lock();
        let imbalance = pos.imbalance();
        drop(pos);
        
        // === COMPLETE SET ARBITRAGE ===
        if combined <= (1.0 - self.config.min_edge) {
            let skew = self.get_skew_multiplier(yes_price.min(no_price));
            return TradingSignal::Arbitrage {
                yes_price,
                no_price,
                combined,
                edge,
                skew_multiplier: skew,
            };
        }
        
        // === TAKER MODE ===
        if edge >= self.config.taker_max_edge && combined <= (1.0 + self.config.taker_max_spread) {
            let side = if yes_price > no_price { Side::Yes } else { Side::No };
            return TradingSignal::Taker {
                side,
                price: if side == Side::Yes { yes_price } else { no_price },
                edge,
            };
        }
        
        // === DIRECTIONAL BETTING ===
        if self.config.directional_enabled {
            let confidence = self.calculate_confidence(yes_price, no_price);
            
            if confidence.confidence >= self.config.directional_confidence_threshold 
                && edge >= self.config.directional_min_edge {
                
                let side = if yes_price > no_price { Side::Yes } else { Side::No };
                let skew = self.get_skew_multiplier(yes_price.max(no_price));
                
                return TradingSignal::Directional {
                    side,
                    price: if side == Side::Yes { yes_price } else { no_price },
                    confidence: confidence.confidence,
                    edge,
                    skew_multiplier: skew,
                };
            }
        }
        
        // === TOP-UP CHECK ===
        if self.config.top_up_enabled {
            let pos = self.position.lock();
            if imbalance > self.config.top_up_min_shares {
                let side = if pos.yes_shares > pos.no_shares { Side::No } else { Side::Yes };
                return TradingSignal::TopUp {
                    side,
                    shares: imbalance,
                };
            }
        }
        
        TradingSignal::None
    }
    
    /// Calculate order size based on skew
    pub fn calculate_size(&self, base_size: f64, skew_multiplier: f64, imbalance: i32) -> f64 {
        let size = base_size * skew_multiplier;
        
        // Apply imbalance penalty
        let imbalance_fraction = (imbalance as f64 / self.config.max_position_imbalance as f64).min(1.0);
        let adjusted_size = size * (1.0 - imbalance_fraction);
        
        adjusted_size.max(1.0) // Minimum $1
    }
    
    /// Update position from trade result
    pub fn update_position(&self, side: Side, shares: i32, filled: bool) {
        let mut pos = self.position.lock();
        
        if filled {
            match side {
                Side::Yes => pos.yes_shares += shares,
                Side::No => pos.no_shares += shares,
            }
        } else {
            // Remove from in-flight
            match side {
                Side::Yes => pos.in_flight_yes = (pos.in_flight_yes - shares).max(0),
                Side::No => pos.in_flight_no = (pos.in_flight_no - shares).max(0),
            }
        }
    }
    
    /// Lock position for in-flight order
    pub fn lock_position(&self, side: Side, shares: i32) {
        let mut pos = self.position.lock();
        match side {
            Side::Yes => pos.in_flight_yes += shares,
            Side::No => pos.in_flight_no += shares,
        }
    }
    
    /// Get current position state
    pub fn get_position(&self) -> Position {
        self.position.lock().clone()
    }
    
    /// Check if we can trade (within limits)
    pub fn can_trade(&self, required_shares: i32) -> bool {
        let pos = self.position.lock();
        pos.imbalance() + required_shares <= self.config.max_position_imbalance
    }
}

impl Default for Position {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expensive_side() {
        let strategy = GabagoolStrategy::new(GabagoolConfig::default());
        
        assert!(!strategy.is_expensive(0.50));
        assert!(!strategy.is_expensive(0.55));
        assert!(strategy.is_expensive(0.56));
        assert!(strategy.is_expensive(0.99));
    }
    
    #[test]
    fn test_skew_multiplier() {
        let strategy = GabagoolStrategy::new(GabagoolConfig::default());
        
        assert_eq!(strategy.get_skew_multiplier(0.50), 1.0);
        assert_eq!(strategy.get_skew_multiplier(0.55), 1.0);
        assert_eq!(strategy.get_skew_multiplier(0.56), 3.0);
        assert_eq!(strategy.get_skew_multiplier(0.99), 3.0);
    }
    
    #[test]
    fn test_position_imbalance() {
        let pos = Position::new();
        assert_eq!(pos.imbalance(), 0);
        
        let mut pos = pos;
        pos.yes_shares = 100;
        pos.no_shares = 30;
        assert_eq!(pos.imbalance(), 70);
    }
}
