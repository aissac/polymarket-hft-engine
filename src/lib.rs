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
mod hot_path_optimized;
mod websocket;
// mod polyfill_integration; // Disabled - polyfill-rs bug
pub mod production;

pub use orderbook::OrderBookTracker;
pub use api::{PolyClient, SimplifiedMarket, TokenData};
pub use strategy::PingpongStrategy;
pub use trading::{TradingEngine, TradingConfig, ArbitrageSignal};
pub use maker_hybrid::{MakerSignal, MakerSide, InventoryTracker, evaluate_maker_opportunity};
pub use production::{ProductionGuard, CircuitBreaker, LiquidityCheck, GasEstimate, OrderManager, TradeType, TradingStats};

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
