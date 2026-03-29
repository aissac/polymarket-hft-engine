//! Shared Execution State
//! 
//! Tracks pending hedges, order relationships, and fill status for stop-loss and CTF merge.

use dashmap::DashMap;
use alloy_primitives::B256;

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
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            pending_hedges: DashMap::new(),
            taker_to_maker: DashMap::new(),
        }
    }

    /// Record a new hedge pair after order submission
    pub fn record_hedge(
        &self,
        maker_order_id: String,
        taker_order_id: String,
        taker_token_id: String,
        condition_id: B256,
        target_size: u64,
    ) {
        // Map taker -> maker for fill routing
        self.taker_to_maker.insert(taker_order_id, maker_order_id.clone());

        // Track pending hedge
        self.pending_hedges.insert(maker_order_id, HedgeContext {
            taker_token_id,
            condition_id,
            target_size,
            remaining_taker_size: target_size,
            maker_mined: false,
            taker_mined: false,
        });
    }

    /// Check if both legs are MINED (ready for CTF merge)
    pub fn is_ready_for_merge(&self, maker_order_id: &str) -> bool {
        if let Some(ctx) = self.pending_hedges.get(maker_order_id) {
            ctx.maker_mined && ctx.taker_mined
        } else {
            false
        }
    }

    /// Get remaining Taker size
    pub fn get_remaining_taker_size(&self, maker_order_id: &str) -> Option<u64> {
        self.pending_hedges.get(maker_order_id).map(|ctx| ctx.remaining_taker_size)
    }

    /// Clean up after merge is dispatched
    pub fn cleanup(&self, maker_order_id: &str) {
        self.pending_hedges.remove(maker_order_id);
        // Note: taker_to_maker cleanup would require iterating or storing taker_id
    }
}

impl Default for ExecutionState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::B256;

    #[test]
    fn test_record_hedge() {
        let state = ExecutionState::new();
        let condition_id = B256::ZERO;
        
        state.record_hedge(
            "maker_123".to_string(),
            "taker_456".to_string(),
            "token_789".to_string(),
            condition_id,
            1000000,
        );

        assert!(state.pending_hedges.contains_key("maker_123"));
        assert!(state.taker_to_maker.contains_key("taker_456"));
    }

    #[test]
    fn test_is_ready_for_merge() {
        let state = ExecutionState::new();
        let condition_id = B256::ZERO;
        
        state.record_hedge(
            "maker_123".to_string(),
            "taker_456".to_string(),
            "token_789".to_string(),
            condition_id,
            1000000,
        );

        // Not ready initially
        assert!(!state.is_ready_for_merge("maker_123"));

        // Mark both as mined
        if let Some(mut ctx) = state.pending_hedges.get_mut("maker_123") {
            ctx.maker_mined = true;
            ctx.taker_mined = true;
        }

        assert!(state.is_ready_for_merge("maker_123"));
    }
}