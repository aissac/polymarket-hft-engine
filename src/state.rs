//! Shared Execution State
//! 
//! Tracks pending hedges, order relationships, fill status, and condition mapping.

use dashmap::DashMap;
use alloy_primitives::B256;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

/// Context for a pending hedge (Maker + Taker arbitrage pair)
pub struct HedgeContext {
    /// The Taker token ID (opposite leg of the arbitrage)
    pub taker_token_id: String,
    /// The condition ID for CTF merge (same for YES/NO pair)
    pub condition_id: B256,
    /// Target size in micro-USDC
    pub target_size: u64,
    /// Remaining Taker size to be filled
    pub remaining_taker_size: u64,
    /// Whether Maker leg has reached MINED status
    pub maker_mined: bool,
    /// Whether Taker leg has reached MINED status
    pub taker_mined: bool,
}

/// Shared state across REST executor, User WS, and Stop-Loss timer
pub struct ExecutionState {
    /// Maps Maker Order ID -> Hedge Context
    pub pending_hedges: DashMap<String, HedgeContext>,
    /// Maps Taker Order ID -> Maker Order ID (reverse lookup)
    pub taker_to_maker: DashMap<String, String>,
    /// Maps token_id -> condition_id (built at startup)
    pub condition_map: DashMap<String, String>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            pending_hedges: DashMap::new(),
            taker_to_maker: DashMap::new(),
            condition_map: DashMap::new(),
        }
    }
}

impl Default for ExecutionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Token orderbook state using fixed-size arrays
/// Index = price in cents (0-99), Value = size
#[derive(Clone, Debug)]
pub struct TokenBookState {
    pub bids: [u64; 100],  // bids[44] = size at $0.44
    pub asks: [u64; 100],  // asks[46] = size at $0.46
}

impl Default for TokenBookState {
    fn default() -> Self {
        Self {
            bids: [0; 100],
            asks: [0; 100],
        }
    }
}

impl TokenBookState {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Get best bid (highest buy price with size > 0)
    pub fn get_best_bid(&self) -> Option<(u64, u64)> {
        for price in (1..100).rev() {
            if self.bids[price] > 0 {
                return Some((price as u64, self.bids[price]));
            }
        }
        None
    }
    
    /// Get best ask (lowest sell price with size > 0)
    pub fn get_best_ask(&self) -> Option<(u64, u64)> {
        for price in 1..100 {
            if self.asks[price] > 0 {
                return Some((price as u64, self.asks[price]));
            }
        }
        None
    }
    
    /// Update bid level (price in fixed-point * 1,000,000)
    pub fn update_bid(&mut self, price_u64: u64, size_u64: u64) {
        let price_cents = (price_u64 / 10_000) as usize;
        if price_cents > 0 && price_cents < 100 {
            self.bids[price_cents] = size_u64;
        }
    }
    
    /// Update ask level (price in fixed-point * 1,000,000)
    pub fn update_ask(&mut self, price_u64: u64, size_u64: u64) {
        let price_cents = (price_u64 / 10_000) as usize;
        if price_cents > 0 && price_cents < 100 {
            self.asks[price_cents] = size_u64;
        }
    }
}

/// Opportunity detected (sent to background thread)
#[derive(Clone, Debug)]
pub struct OpportunitySnapshot {
    pub yes_token_hash: u64,
    pub no_token_hash: u64,
    pub yes_best_bid: u64,
    pub yes_best_ask: u64,
    pub yes_ask_size: u64,
    pub no_best_bid: u64,
    pub no_best_ask: u64,
    pub no_ask_size: u64,
    pub combined_ask: u64,
    pub timestamp_nanos: u64,
}

/// Fast hash for token IDs
pub fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Parse fixed-point price (multiply by 1,000,000 for 6 decimal precision)
pub fn parse_fixed_6(bytes: &[u8]) -> u64 {
    let mut result: u64 = 0;
    let mut decimal_seen = false;
    let mut decimal_places = 0u32;
    
    for &b in bytes {
        match b {
            b'.' => decimal_seen = true,
            b'0'..=b'9' => {
                result = result * 10 + (b - b'0') as u64;
                if decimal_seen { decimal_places += 1; }
            }
            _ => {}
        }
    }
    
    while decimal_places < 6 {
        result *= 10;
        decimal_places += 1;
    }
    result
}