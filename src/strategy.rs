//! Gabagool Strategy Engine
//! 
//! Phase 1: Read-only strategy that monitors for arbitrage opportunities.
//! No actual trading - just proving the data pipeline.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{info, warn, debug};

use crate::orderbook::OrderBookTracker;

const TARGET_HEDGE_SUM: f64 = 0.95;

/// Strategy state
#[derive(Debug, Clone)]
pub enum StrategyState {
    Scanning,
    OpportunityFound,
    Executing,
    InPosition,
    Error,
}

/// Strategy event
#[derive(Debug, Clone)]
pub enum StrategyEvent {
    ArbitrageDetected {
        market: String,
        yes_ask: f64,
        no_ask: f64,
        combined: f64,
        profit: f64,
    },
    StateChanged(StrategyState),
    Error(String),
}

/// Main strategy engine
pub struct GabagoolStrategy {
    state: StrategyState,
    tracker: Arc<OrderBookTracker>,
    events: mpsc::UnboundedSender<StrategyEvent>,
}

impl GabagoolStrategy {
    pub fn new(tracker: Arc<OrderBookTracker>, events: mpsc::UnboundedSender<StrategyEvent>) -> Self {
        Self {
            state: StrategyState::Scanning,
            tracker,
            events,
        }
    }
    
    /// Main strategy loop (Phase 1: read-only monitoring)
    pub async fn run(&mut self) {
        info!("🚀 Gabagool Strategy starting (PHASE 1: Read-Only Mode)");
        info!("📊 Target: {} for arbitrage detection", TARGET_HEDGE_SUM);
        
        loop {
            // Check all markets for arbitrage
            let opportunites = self.tracker.find_arbitrages(TARGET_HEDGE_SUM);
            
            if !opportunites.is_empty() {
                for (market, details) in opportunites {
                    info!(
                        "🎯 ARBITRAGE OPPORTUNITY: {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Profit: ${:.4}/share",
                        market,
                        details.yes_ask,
                        details.no_ask,
                        details.combined_cost,
                        details.profit_per_share
                    );
                    
                    // In Phase 1, just log it - don't trade
                    let _ = self.events.send(StrategyEvent::ArbitrageDetected {
                        market: market.clone(),
                        yes_ask: details.yes_ask,
                        no_ask: details.no_ask,
                        combined: details.combined_cost,
                        profit: details.profit_per_share,
                    });
                }
                
                self.state = StrategyState::OpportunityFound;
            } else {
                self.state = StrategyState::Scanning;
            }
            
            // Check every 100ms
            sleep(Duration::from_millis(100)).await;
        }
    }
    
    /// Get current state
    pub fn state(&self) -> &StrategyState {
        &self.state
    }
}

/// Async logger for strategy events (sends to CloudWatch via stdout)
pub async fn start_event_logger(mut rx: mpsc::UnboundedReceiver<StrategyEvent>) {
    info!("📝 Strategy event logger started");
    
    while let Some(event) = rx.recv().await {
        match event {
            StrategyEvent::ArbitrageDetected { market, yes_ask, no_ask, combined, profit } => {
                // Log to stdout - CloudWatch agent will pick this up
                println!(
                    "ARB_DETECTED {} YES={:.4} NO={:.4} COMBINED={:.4} PROFIT={:.4}",
                    market, yes_ask, no_ask, combined, profit
                );
            }
            StrategyEvent::StateChanged(state) => {
                println!("STATE_CHANGE {:?}", state);
            }
            StrategyEvent::Error(msg) => {
                eprintln!("STRATEGY_ERROR {}", msg);
            }
        }
    }
}
