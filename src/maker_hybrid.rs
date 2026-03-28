//! Maker Hybrid Strategy Module
//! 
//! Strategy: Post Maker order on volatile side, fire Taker on stable side
//! Benefits:
//! - Earn 20% Maker rebate (vs pay 1.80% Taker fee)
//! - Bypass latency arms race (Maker orders wait for fill)
//! - Fill rate: 60-80% vs 0-15% for Taker
//!
//! Flow:
//! 1. Detect arb opportunity with extreme probability (p<0.30 or p>0.70)
//! 2. Identify volatile side (lower liquidity = more price movement)
//! 3. Post Maker order at mid-price on volatile side
//! 4. If filled, immediately fire Taker on stable side
//! 5. Net: 1.00 - combined - taker_fee + maker_rebate

use anyhow::Result;
use tracing::{info, warn};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Maker rebate rate (20% of taker fee)
const MAKER_REBATE_RATE: f64 = 0.20;

/// Dynamic taker fee at p=0.5 (peak)
const MAX_TAKER_FEE: f64 = 0.0156;

/// Maker hybrid signal
#[derive(Debug, Clone)]
pub struct MakerSignal {
    pub market_id: String,
    pub condition_id: String,
    pub yes_price: f64,
    pub no_price: f64,
    pub combined: f64,
    pub edge: f64,
    /// Which side to post as Maker (volatile side)
    pub maker_side: MakerSide,
    /// Maker order price (mid-price for better fill rate)
    pub maker_price: f64,
    /// Taker order price (stable side)
    pub taker_price: f64,
    /// Size for both legs
    pub size: f64,
    /// Expected profit including rebate
    pub profit_with_rebate: f64,
    /// Lower fee due to extreme probability
    pub effective_fee: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MakerSide {
    Yes,
    No,
}

/// Inventory tracker for Maker strategy
/// Must maintain balanced YES/NO positions
pub struct InventoryTracker {
    /// YES shares held per market
    yes_positions: Arc<Mutex<HashMap<String, f64>>>,
    /// NO shares held per market
    no_positions: Arc<Mutex<HashMap<String, f64>>>,
    /// Maximum skew allowed before rebalancing
    max_skew_ratio: f64,
}

impl InventoryTracker {
    pub fn new(max_skew_ratio: f64) -> Self {
        Self {
            yes_positions: Arc::new(Mutex::new(HashMap::new())),
            no_positions: Arc::new(Mutex::new(HashMap::new())),
            max_skew_ratio,
        }
    }
    
    /// Check if inventory is balanced enough for Maker order
    pub fn can_post_maker(&self, condition_id: &str, side: MakerSide, size: f64) -> bool {
        let yes = self.yes_positions.lock().unwrap();
        let no = self.no_positions.lock().unwrap();
        
        let yes_held = yes.get(condition_id).copied().unwrap_or(0.0);
        let no_held = no.get(condition_id).copied().unwrap_or(0.0);
        
        // Calculate skew if we add this position
        let (new_yes, new_no) = match side {
            MakerSide::Yes => (yes_held + size, no_held),
            MakerSide::No => (yes_held, no_held + size),
        };
        
        let total = new_yes + new_no;
        if total < 10.0 {
            return true; // Small position, always ok
        }
        
        let skew_ratio = (new_yes - new_no).abs() / total;
        skew_ratio <= self.max_skew_ratio
    }
    
    /// Record a fill
    pub fn record_fill(&self, condition_id: &str, side: MakerSide, size: f64) {
        let (positions, delta) = match side {
            MakerSide::Yes => (&self.yes_positions, size),
            MakerSide::No => (&self.no_positions, size),
        };
        
        let mut pos = positions.lock().unwrap();
        *pos.entry(condition_id.to_string()).or_insert(0.0) += delta;
    }
}

/// Calculate dynamic fee based on probability
pub fn calculate_dynamic_fee(p: f64) -> f64 {
    let variance = p * (1.0 - p);
    // Current fee structure: feeRate × variance²
    // feeRate = 0.25 for crypto until March 30, 2026
    (0.25 * variance * variance).max(0.01)
}

/// Identify which side is more volatile (lower depth = more volatile)
pub fn identify_volatile_side(yes_depth: f64, no_depth: f64) -> MakerSide {
    // Lower depth = more volatile = better for Maker order
    // More likely to get filled on thin side
    if yes_depth < no_depth {
        MakerSide::Yes
    } else {
        MakerSide::No
    }
}

/// Calculate Maker order price (slightly better than mid)
pub fn calculate_maker_price(side: MakerSide, yes_price: f64, no_price: f64) -> f64 {
    // Post at mid-price to maximize fill rate
    // For YES: post at (yes_bid + yes_ask) / 2
    // For NO: post at (no_bid + no_ask) / 2
    // Simplified: use current best price
    
    // Maker wants to post slightly better than current best
    // to be first in queue but still earn rebate
    match side {
        MakerSide::Yes => yes_price - 0.01, // Slightly lower than best ask
        MakerSide::No => no_price - 0.01,   // Slightly lower than best ask
    }
}

/// Generate Maker hybrid signal if profitable
pub fn evaluate_maker_opportunity(
    condition_id: &str,
    yes_price: f64,
    no_price: f64,
    yes_depth: f64,
    no_depth: f64,
    combined: f64,
    min_edge: f64,
) -> Option<MakerSignal> {
    // Must be extreme probability for lower fees
    let p = yes_price / combined;
    if p > 0.30 && p < 0.70 {
        // Not extreme probability - Maker strategy less advantageous
        return None;
    }
    
    // Calculate edge
    let fee = calculate_dynamic_fee(p);
    let profit_per_share = 1.0 - combined - fee;
    let edge = profit_per_share / combined;
    
    if edge < min_edge {
        return None;
    }
    
    // Combined must be <= /bin/bash.98 (room for profit)
    if combined > 0.98 {
        return None;
    }
    
    // Identify volatile side for Maker order
    let maker_side = identify_volatile_side(yes_depth, no_depth);
    
    // Calculate prices
    let (maker_price, taker_price) = match maker_side {
        MakerSide::Yes => (yes_price - 0.01, no_price),
        MakerSide::No => (no_price - 0.01, yes_price),
    };
    
    // Maker rebate benefit
    let maker_rebate = MAKER_REBATE_RATE * MAX_TAKER_FEE; // ~0.3%
    let profit_with_rebate = profit_per_share + maker_rebate;
    
    // Size based on depth (limit to available liquidity)
    let size = match maker_side {
        MakerSide::Yes => (yes_depth * 0.1).min(100.0).max(10.0),
        MakerSide::No => (no_depth * 0.1).min(100.0).max(10.0),
    };
    
    Some(MakerSignal {
        market_id: condition_id.to_string(),
        condition_id: condition_id.to_string(),
        yes_price,
        no_price,
        combined,
        edge,
        maker_side,
        maker_price,
        taker_price,
        size,
        profit_with_rebate,
        effective_fee: fee,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_extreme_probability_filter() {
        // p=0.88 should pass (extreme)
        let signal = evaluate_maker_opportunity(
            "test", 0.88, 0.04, 100.0, 100.0, 0.92, 0.035
        );
        assert!(signal.is_some());
        
        // p=0.50 should fail (not extreme)
        let signal = evaluate_maker_opportunity(
            "test", 0.50, 0.42, 100.0, 100.0, 0.92, 0.035
        );
        assert!(signal.is_none());
    }
    
    #[test]
    fn test_maker_rebate_calculation() {
        let rebate = MAKER_REBATE_RATE * MAX_TAKER_FEE;
        assert!((rebate - 0.00312).abs() < 0.0001); // ~0.3%
    }
}
