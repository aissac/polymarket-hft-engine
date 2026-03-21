//! Pingpong PnL Tracking Module - Phase 3.6
//! 
//! Non-blocking PnL tracking with detailed shares report.

use std::sync::Arc;
use std::fs::{OpenOptions, File};
use std::io::Write;
use std::path::Path;
use parking_lot::Mutex;
use serde::{Serialize, Deserialize};

/// A single simulated trade result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeResult {
    pub market_id: String,
    pub condition_id: String,
    pub yes_price: f64,
    pub no_price: f64,
    pub combined_cost: f64,
    pub fee: f64,
    pub net_pnl: f64,
    pub size: f64,
    pub is_win: bool,
    pub is_arb: bool,
    pub timestamp: i64,
}

/// In-memory PnL state
pub struct PnlState {
    pub total_trades: u32,
    pub winning_trades: u32,
    pub losing_trades: u32,
    pub total_shares: f64,
    pub winning_shares: f64,
    pub losing_shares: f64,
    pub cumulative_pnl: f64,
    pub cumulative_fees: f64,
    pub arb_opportunities: u32,
    pub arb_executed: u32,
}

impl PnlState {
    pub fn new() -> Self {
        Self {
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            total_shares: 0.0,
            winning_shares: 0.0,
            losing_shares: 0.0,
            cumulative_pnl: 0.0,
            cumulative_fees: 0.0,
            arb_opportunities: 0,
            arb_executed: 0,
        }
    }

    pub fn win_rate(&self) -> f64 {
        if self.total_trades == 0 { return 0.0; }
        (self.winning_trades as f64 / self.total_trades as f64) * 100.0
    }

    pub fn avg_win(&self) -> f64 {
        if self.winning_trades == 0 { return 0.0; }
        self.winning_shares / self.winning_trades as f64
    }

    pub fn avg_loss(&self) -> f64 {
        if self.losing_trades == 0 { return 0.0; }
        self.losing_shares.abs() / self.losing_trades as f64
    }

    pub fn profit_factor(&self) -> f64 {
        if self.losing_shares.abs() == 0.0 { 
            return if self.winning_shares > 0.0 { 999.99 } else { 0.0 }; 
        }
        self.winning_shares / self.losing_shares.abs()
    }

    /// Generate detailed report
    pub fn generate_report(&self) -> String {
        let mut report = String::new();
        report.push_str("═══════════════════════════════════════\n");
        report.push_str("   📊 PINGPONG DRY-RUN REPORT\n");
        report.push_str("═══════════════════════════════════════\n\n");
        
        report.push_str("📈 TRADE STATISTICS\n");
        report.push_str("─────────────────────────────────────\n");
        report.push_str(&format!("Total Trades:      {}\n", self.total_trades));
        report.push_str(&format!("Win Rate:          {:.2}%\n", self.win_rate()));
        report.push_str(&format!("Winning Trades:    {} ({:.2}%)\n", 
            self.winning_trades, 
            if self.total_trades > 0 { (self.winning_trades as f64 / self.total_trades as f64) * 100.0 } else { 0.0 }));
        report.push_str(&format!("Losing Trades:     {} ({:.2}%)\n", 
            self.losing_trades,
            if self.total_trades > 0 { (self.losing_trades as f64 / self.total_trades as f64) * 100.0 } else { 0.0 }));
        report.push_str("\n");
        
        report.push_str("💰 SHARES & POSITIONS\n");
        report.push_str("─────────────────────────────────────\n");
        report.push_str(&format!("Total Shares:      {:.2}\n", self.total_shares));
        report.push_str(&format!("Winning Shares:    {:.2}\n", self.winning_shares));
        report.push_str(&format!("Losing Shares:     {:.2}\n", self.losing_shares.abs()));
        report.push_str(&format!("Avg Win Size:      ${:.2}\n", self.avg_win()));
        report.push_str(&format!("Avg Loss Size:     ${:.2}\n", self.avg_loss()));
        report.push_str("\n");
        
        report.push_str("💵 PROFIT & LOSS\n");
        report.push_str("─────────────────────────────────────\n");
        report.push_str(&format!("Cumulative PnL:   ${:.4}\n", self.cumulative_pnl));
        report.push_str(&format!("Total Fees Paid:  ${:.4}\n", self.cumulative_fees));
        report.push_str(&format!("Profit Factor:    {:.2}x\n", self.profit_factor()));
        report.push_str("\n");
        
        report.push_str("🎯 ARBITRAGE STATISTICS\n");
        report.push_str("─────────────────────────────────────\n");
        report.push_str(&format!("Arb Opportunities: {}\n", self.arb_opportunities));
        report.push_str(&format!("Arb Executed:      {}\n", self.arb_executed));
        report.push_str(&format!("Execution Rate:    {:.2}%\n", 
            if self.arb_opportunities > 0 { (self.arb_executed as f64 / self.arb_opportunities as f64) * 100.0 } else { 0.0 }));
        
        report.push_str("\n═══════════════════════════════════════\n");
        
        // Emoji indicator
        let emoji = if self.cumulative_pnl > 0.0 { "🟢" } else if self.cumulative_pnl < 0.0 { "🔴" } else { "⚪" };
        report.push_str(&format!("NET PNL: {} ${:.4}\n", emoji, self.cumulative_pnl));
        report.push_str("═══════════════════════════════════════\n");
        
        report
    }

    /// Generate simple one-liner
    pub fn generate_telegram_summary(&self) -> String {
        format!(
            "🤖 Dry-Run Report\n\
            📊 Trades: {} | Win: {:.1}%\n\
            💰 PnL: ${:.4} | Fees: ${:.4}\n\
            🎯 Arb: {}/{} executed",
            self.total_trades,
            self.win_rate(),
            self.cumulative_pnl,
            self.cumulative_fees,
            self.arb_executed,
            self.arb_opportunities
        )
    }
}

impl Default for PnlState {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe PnL tracker
pub struct PnlTracker {
    state: Arc<Mutex<PnlState>>,
    log_path: Option<String>,
}

impl PnlTracker {
    pub fn new(log_path: Option<&str>) -> Self {
        Self {
            state: Arc::new(Mutex::new(PnlState::new())),
            log_path: log_path.map(|s| s.to_string()),
        }
    }

    /// Record a simulated trade
    pub fn record_trade(&self, trade: &TradeResult) {
        let mut s = self.state.lock();
        
        s.total_trades += 1;
        s.total_shares += trade.size;
        s.cumulative_fees += trade.fee;
        
        if trade.is_win {
            s.winning_trades += 1;
            s.winning_shares += trade.net_pnl;
        } else {
            s.losing_trades += 1;
            s.losing_shares += trade.net_pnl;
        }
        
        s.cumulative_pnl += trade.net_pnl;
        
        if trade.is_arb {
            s.arb_executed += 1;
        }
        
        // Log to file (non-blocking for the hot path)
        if let Some(ref path) = self.log_path {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(file, "{:?}", trade);
            }
        }
    }

    /// Record an arbitrage opportunity
    pub fn record_arb_opportunity(&self) {
        let mut s = self.state.lock();
        s.arb_opportunities += 1;
    }

    /// Get current state snapshot
    pub fn get_state(&self) -> PnlState {
        let s = self.state.lock();
        PnlState {
            total_trades: s.total_trades,
            winning_trades: s.winning_trades,
            losing_trades: s.losing_trades,
            total_shares: s.total_shares,
            winning_shares: s.winning_shares,
            losing_shares: s.losing_shares,
            cumulative_pnl: s.cumulative_pnl,
            cumulative_fees: s.cumulative_fees,
            arb_opportunities: s.arb_opportunities,
            arb_executed: s.arb_executed,
        }
    }

    /// Get formatted report
    pub fn get_report(&self) -> String {
        self.get_state().generate_report()
    }

    /// Get telegram summary
    pub fn get_telegram_summary(&self) -> String {
        self.get_state().generate_telegram_summary()
    }
}

/// Helper to create TradeResult
pub fn create_trade_result(
    market_id: &str,
    condition_id: &str,
    yes_price: f64,
    no_price: f64,
    size: f64,
) -> TradeResult {
    let combined_cost = yes_price + no_price;
    let fee = combined_cost * 0.02; // 2% Polymarket fee
    let gross_pnl = 1.00 - combined_cost;
    let net_pnl = gross_pnl - fee;
    
    TradeResult {
        market_id: market_id.to_string(),
        condition_id: condition_id.to_string(),
        yes_price,
        no_price,
        combined_cost,
        fee,
        net_pnl,
        size,
        is_win: net_pnl > 0.0,
        is_arb: combined_cost < 0.98, // Arbitrage if combined < 98 cents
        timestamp: chrono::Utc::now().timestamp_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pnl_state() {
        let mut state = PnlState::new();
        assert_eq!(state.win_rate(), 0.0);
        
        state.total_trades = 10;
        state.winning_trades = 7;
        state.winning_shares = 3.50;
        state.losing_trades = 3;
        state.losing_shares = -1.50;
        state.cumulative_pnl = 2.00;
        
        assert_eq!(state.win_rate(), 70.0);
        assert_eq!(state.profit_factor(), 2.33);
    }
    
    #[test]
    fn test_trade_result() {
        let trade = create_trade_result("BTC_yes", "0x123", 0.50, 0.45, 100.0);
        
        assert_eq!(trade.combined_cost, 0.95);
        assert!(trade.is_win);
        assert!(trade.is_arb);
    }
}
