//! Pingpong Bot - Phase 2: REST API + Orderbook + Trading
//! 
//! Uses Polymarket REST API to fetch markets and track prices.
//! Executes arbitrage trades via polymarket-client-sdk.

mod orderbook;
mod ghost_simulator;
mod api;
mod strategy;
mod trading;
mod maker_hybrid;
mod simd_hot_path;
pub mod hft_hot_path;
pub mod market_rollover;
mod hot_path_optimized;
mod websocket;
// mod polyfill_integration; // Disabled - polyfill-rs bug
pub mod production;
// Live trading modules
pub mod signing;
pub mod execution;
pub mod merge_worker;
pub mod stop_loss;
pub mod state;
pub mod condition_map;
pub mod crypto_markets;
pub mod token_map;
pub mod background_wiring;
pub mod maker_taker_routing;
pub mod fees;
pub mod user_ws;
pub mod websocket_reader;

pub use orderbook::OrderBookTracker;
pub use api::{PolyClient, SimplifiedMarket, TokenData};
pub use strategy::PingpongStrategy;
pub use trading::{TradingEngine, TradingConfig, ArbitrageSignal};
pub use maker_hybrid::{MakerSignal, MakerSide, InventoryTracker, evaluate_maker_opportunity};
pub use production::{ProductionGuard, CircuitBreaker, LiquidityCheck, GasEstimate, OrderManager, TradeType, TradingStats};
// Live trading exports
pub use signing::{init_signer};

pub use execution::{submit_order, build_l2_headers, fetch_fee_rate, create_order_payload, generate_salt};
pub use merge_worker::{run_merge_worker, MergeTask};
pub use stop_loss::{start_stop_loss_timer, execute_fak_order, handle_trade_event};
pub use state::{ExecutionState as HedgeState, HedgeContext};
pub use condition_map::{build_maps, get_current_periods};
pub use user_ws::run_user_ws;

/// Target combined cost - if YES_ask + NO_ask < this, we have arbitrage
/// 0.95 gives us ~3% profit after 2% Polymarket fee + slippage buffer
pub const TARGET_HEDGE_SUM: f64 = 0.95;

/// Minimum spread to consider (avoiding ghost liquidity)
pub const MIN_SPREAD: f64 = 0.001;

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_arbitrage_detection() {
        // Case 1: Arbitrage exists (combined < 0.95)
        let combined = 0.50 + 0.44; // = 0.94
        let has_arb = combined < TARGET_HEDGE_SUM;
        assert!(has_arb);
        
        // Case 2: No arbitrage (combined >= 0.95)
        let combined = 0.50 + 0.46; // = 0.96
        let has_arb = combined < TARGET_HEDGE_SUM;
        assert!(!has_arb);
    }
    
    #[test]
    fn test_profit_math() {
        // Buy YES@$0.50 + NO@$0.45 = $0.95
        // After 2% fee on $1.00 payout: $1.00 - $0.02 = $0.98
        // Profit = $0.98 - $0.95 = $0.03 per share
        let yes = 0.50;
        let no = 0.45;
        let combined = yes + no;
        let payout = 1.0;
        let fee = payout * 0.02; // 2% of $1
        let profit = payout - fee - combined;
        
        assert_eq!(combined, 0.95);
        assert!(profit > 0.0);
        assert!(profit < 0.05); // ~$0.03
    }
}
pub mod hft_metrics;
pub mod jsonl_logger;
pub mod sdk_websocket;
pub use sdk_websocket::OrderbookSnapshot;
pub mod pre_order_merge;
pub mod maker_execution;
pub use hft_hot_path::RolloverCommandV2;
pub use market_rollover::run_rollover_thread;
pub mod one_sided_fill;
pub mod laddering;
pub mod signal_filter;
pub mod price_cache;
pub use price_cache::{PriceCache, create_price_cache, update_price, get_price, is_market_ranging};
pub mod toxic_flow;
pub mod dry_run_engine;
