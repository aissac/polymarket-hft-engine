//! Shared Execution State
//! 
//! Tracks pending hedges, order relationships, fill status, and condition mapping.

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
