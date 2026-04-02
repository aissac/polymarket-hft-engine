//! Laddering - Tapered Position Sizing for Pre-Order Merge Trap
//! 
//! Based on NotebookLM validation (2026-04-02)
//! 
//! Ladder Structure (BTC/ETH):
//! - Level 1 (Bait): 50 shares @ 0.45 ($22.50)
//! - Level 2 (Trap): 100 shares @ 0.40 ($40.00)
//! - Level 3 (Deep Tail): 200 shares @ 0.30 ($60.00)
//! Total: 350 shares per side, $122.50 per side, $245 total
//!
//! For SOL/XRP (wider ladder):
//! - Level 1: 25 shares @ 0.45
//! - Level 2: 50 shares @ 0.35
//! - Level 3: 100 shares @ 0.20

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Single ladder level
#[derive(Debug, Clone)]
pub struct LadderLevel {
    pub price: f64,
    pub size: f64,
}

/// Ladder configuration for one side (YES or NO)
#[derive(Debug, Clone)]
pub struct LadderConfig {
    pub levels: Vec<LadderLevel>,
    pub total_capital: f64,
    pub total_shares: f64,
    pub weighted_avg_entry: f64,
}

impl LadderConfig {
    /// Optimal ladder for BTC/ETH (tight liquidity)
    pub fn btc_eth_ladder() -> Self {
        let levels = vec![
            LadderLevel { price: 0.45, size: 50.0 },   // Level 1: Bait
            LadderLevel { price: 0.40, size: 100.0 },  // Level 2: Trap
            LadderLevel { price: 0.30, size: 200.0 },  // Level 3: Deep Tail
        ];
        
        let total_shares: f64 = levels.iter().map(|l| l.size).sum();
        let total_capital: f64 = levels.iter().map(|l| l.size * l.price).sum();
        let weighted_avg_entry = total_capital / total_shares;
        
        Self {
            levels,
            total_capital,
            total_shares,
            weighted_avg_entry,
        }
    }
    
    /// Wider ladder for SOL/XRP (thin liquidity, high volatility)
    pub fn sol_xrp_ladder() -> Self {
        let levels = vec![
            LadderLevel { price: 0.45, size: 25.0 },   // Level 1: Bait
            LadderLevel { price: 0.35, size: 50.0 },   // Level 2: Trap
            LadderLevel { price: 0.20, size: 100.0 },  // Level 3: Deep Tail
        ];
        
        let total_shares: f64 = levels.iter().map(|l| l.size).sum();
        let total_capital: f64 = levels.iter().map(|l| l.size * l.price).sum();
        let weighted_avg_entry = total_capital / total_shares;
        
        Self {
            levels,
            total_capital,
            total_shares,
            weighted_avg_entry,
        }
    }
    
    /// Conservative ladder (small bankroll, 25/50/100)
    pub fn conservative_ladder() -> Self {
        let levels = vec![
            LadderLevel { price: 0.45, size: 25.0 },
            LadderLevel { price: 0.40, size: 50.0 },
            LadderLevel { price: 0.30, size: 100.0 },
        ];
        
        let total_shares: f64 = levels.iter().map(|l| l.size).sum();
        let total_capital: f64 = levels.iter().map(|l| l.size * l.price).sum();
        let weighted_avg_entry = total_capital / total_shares;
        
        Self {
            levels,
            total_capital,
            total_shares,
            weighted_avg_entry,
        }
    }
    
    /// Get all orders to place (token_id, price, size)
    pub fn get_orders(&self, token_id: &str) -> Vec<(String, f64, f64)> {
        self.levels.iter()
            .map(|l| (token_id.to_string(), l.price, l.size))
            .collect()
    }
}

/// Track filled levels for reconciliation
#[derive(Debug, Clone)]
pub struct LadderFillState {
    pub yes_filled: f64,
    pub no_filled: f64,
    pub yes_cost: f64,
    pub no_cost: f64,
    pub mergeable: f64,
    pub excess_yes: f64,
    pub excess_no: f64,
}

impl LadderFillState {
    pub fn new() -> Self {
        Self {
            yes_filled: 0.0,
            no_filled: 0.0,
            yes_cost: 0.0,
            no_cost: 0.0,
            mergeable: 0.0,
            excess_yes: 0.0,
            excess_no: 0.0,
        }
    }
    
    /// Calculate mergeable amount and excess
    pub fn reconcile(&mut self) {
        self.mergeable = self.yes_filled.min(self.no_filled);
        self.excess_yes = self.yes_filled - self.mergeable;
        self.excess_no = self.no_filled - self.mergeable;
    }
    
    /// Calculate PnL for full dual-fill
    pub fn calc_full_merge_pnl(&self) -> f64 {
        let total_cost = self.yes_cost + self.no_cost;
        let merge_revenue = self.mergeable * 1.0;
        let maker_rebate = total_cost * 0.0032; // ~0.32% rebate
        merge_revenue - total_cost + maker_rebate
    }
    
    /// Calculate PnL for timeout exit (one-sided dump at 0.35)
    pub fn calc_timeout_exit_pnl(&self, dump_price: f64) -> f64 {
        let total_cost = self.yes_cost + self.no_cost;
        let merge_revenue = self.mergeable * 1.0;
        let dump_revenue = (self.excess_yes + self.excess_no) * dump_price;
        let taker_fee = dump_revenue * 0.018; // 1.80% taker fee
        let maker_rebate = total_cost * 0.0032;
        merge_revenue + dump_revenue - total_cost - taker_fee + maker_rebate
    }
}
