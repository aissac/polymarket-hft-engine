//! Gabagool Bot - Phase 1: WebSocket + Orderbook Tracking
//! 
//! This module handles:
//! - WebSocket connection to Polymarket CLOB
//! - Orderbook state management (best YES/NO prices)
//! - Arbitrage opportunity detection
//!
//! Phase 1 is READ-ONLY - no money risked, just proving the data pipeline

mod orderbook;
mod websocket;
mod strategy;

pub use orderbook::{OrderBookTracker, PriceLevel, TokenOrderBook, MarketOrderBook};
pub use websocket::PolymarketWsClient;
pub use strategy::GabagoolStrategy;

/// Target combined cost - if YES_ask + NO_ask < this, we have arbitrage
/// 0.95 gives us ~3% profit after 2% Polymarket fee + slippage buffer
pub const TARGET_HEDGE_SUM: f64 = 0.95;

/// Minimum spread to consider (avoiding ghost liquidity)
pub const MIN_SPREAD: f64 = 0.001;

/// Log arbitrage opportunity when detected
pub fn check_arbitrage(yes_ask: f64, no_ask: f64, market_slug: &str) -> Option<ArbitrageOpportunity> {
    let combined = yes_ask + no_ask;
    
    if combined < TARGET_HEDGE_SUM && yes_ask > 0.0 && no_ask > 0.0 {
        let profit_per_share = 1.0 - combined - (combined * 0.02); // After 2% fee
        Some(ArbitrageOpportunity {
            market_slug: market_slug.to_string(),
            yes_ask,
            no_ask,
            combined_cost: combined,
            profit_per_share,
        })
    } else {
        None
    }
}

#[derive(Debug, Clone)]
pub struct ArbitrageOpportunity {
    pub market_slug: String,
    pub yes_ask: f64,
    pub no_ask: f64,
    pub combined_cost: f64,
    pub profit_per_share: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_arbitrage_detection() {
        // Case 1: Arbitrage exists (combined < 0.95)
        let opp = check_arbitrage(0.50, 0.44, "btc-updown-5m-test");
        assert!(opp.is_some());
        let opp = opp.unwrap();
        assert_eq!(opp.combined_cost, 0.94);
        assert!(opp.profit_per_share > 0.0);
        
        // Case 2: No arbitrage (combined >= 0.95)
        let opp = check_arbitrage(0.50, 0.46, "btc-updown-5m-test");
        assert!(opp.is_none());
        
        // Case 3: Zero prices (invalid)
        let opp = check_arbitrage(0.0, 0.44, "btc-updown-5m-test");
        assert!(opp.is_none());
    }
    
    #[test]
    fn test_target_math() {
        // Verify profit math: buy YES@$0.50 + NO@$0.45 = $0.95
        // After 2% fee on $1.00 payout: $1.00 - $0.02 = $0.98
        // Profit = $0.98 - $0.95 = $0.03 per share
        let yes = 0.50;
        let no = 0.45;
        let combined = yes + no;
        let payout = 1.0;
        let fee = combined * 0.02;
        let profit = payout - fee - combined;
        
        assert_eq!(combined, 0.95);
        assert!(profit > 0.0);
        assert!(profit < 0.05); // ~$0.03
    }
}
