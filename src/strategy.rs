//! Pingpong Strategy Engine
//! 
//! Phase 1: Read-only strategy that monitors for arbitrage opportunities.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{info, debug, warn};

use crate::orderbook::OrderBookTracker;
use crate::api::{PolyClient, SimplifiedMarket};

const TARGET_HEDGE_SUM: f64 = 0.95;

/// Strategy state
#[derive(Debug, Clone)]
pub enum StrategyState {
    Initializing,
    Scanning,
    OpportunityFound,
    Error,
}

/// Strategy event for logging/analytics
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
    ApiError(String),
}

/// Main strategy engine
pub struct PingpongStrategy {
    state: StrategyState,
    tracker: Arc<OrderBookTracker>,
    api: Arc<PolyClient>,
    events: mpsc::UnboundedSender<StrategyEvent>,
}

impl PingpongStrategy {
    pub fn new(
        tracker: Arc<OrderBookTracker>, 
        api: Arc<PolyClient>,
        events: mpsc::UnboundedSender<StrategyEvent>,
    ) -> Self {
        Self {
            state: StrategyState::Initializing,
            tracker,
            api,
            events,
        }
    }
    
    /// Main strategy loop (Phase 1: read-only monitoring)
    pub async fn run(&mut self) {
        info!("🚀 Pingpong Strategy starting (PHASE 1: Read-Only Mode)");
        info!("📊 Target: {} for arbitrage detection", TARGET_HEDGE_SUM);
        
        // Check API health first
        if !self.check_api_health().await {
            warn!("⚠️ API health check failed, continuing anyway...");
        }
        
        self.state = StrategyState::Scanning;
        let mut scan_count = 0u64;
        
        loop {
            scan_count += 1;
            
            // Fetch markets from API
            match self.api.get_markets().await {
                Ok(markets) => {
                    if scan_count % 60 == 0 {
                        info!("📊 Scanning {} markets (scan #{})", markets.len(), scan_count);
                    }
                    
                    // Update tracker and check for arbitrage
                    for market in &markets {
                        let yes_price = market.yes_price();
                        let no_price = market.no_price();
                        
                        self.tracker.update(
                            &market.condition_id,
                            yes_price,
                            no_price,
                        );
                        
                        // Check if this market has arbitrage
                        if market.has_arbitrage(TARGET_HEDGE_SUM) {
                            let combined = market.combined_cost().unwrap_or(1.0);
                            let profit = 1.0 - combined - (combined * 0.02); // After 2% fee
                            
                            info!(
                                "🎯 ARBITRAGE OPPORTUNITY: {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Profit: ${:.4}/share",
                                market.condition_id,
                                yes_price.unwrap_or(0.0),
                                no_price.unwrap_or(0.0),
                                combined,
                                profit
                            );
                            
                            let _ = self.events.send(StrategyEvent::ArbitrageDetected {
                                market: market.condition_id.clone(),
                                yes_ask: yes_price.unwrap_or(0.0),
                                no_ask: no_price.unwrap_or(0.0),
                                combined,
                                profit,
                            });
                        }
                    }
                }
                Err(e) => {
                    warn!("API error: {}", e);
                    let _ = self.events.send(StrategyEvent::ApiError(e.to_string()));
                }
            }
            
            // Poll every second
            sleep(Duration::from_secs(1)).await;
        }
    }
    
    /// Check if API is healthy
    async fn check_api_health(&self) -> bool {
        match self.api.health_check().await {
            Ok(true) => {
                info!("✅ Polymarket API is healthy");
                true
            }
            Ok(false) => {
                warn!("⚠️ Polymarket API returned unhealthy");
                false
            }
            Err(e) => {
                warn!("⚠️ Could not reach Polymarket API: {}", e);
                false
            }
        }
    }
    
    /// Get current state
    pub fn state(&self) -> &StrategyState {
        &self.state
    }
}

/// Async logger for strategy events
pub async fn start_event_logger(mut rx: mpsc::UnboundedReceiver<StrategyEvent>) {
    info!("📝 Strategy event logger started");
    
    while let Some(event) = rx.recv().await {
        match event {
            StrategyEvent::ArbitrageDetected { market, yes_ask, no_ask, combined, profit } => {
                println!(
                    "ARB_DETECTED {} YES={:.4} NO={:.4} COMBINED={:.4} PROFIT={:.4}",
                    market, yes_ask, no_ask, combined, profit
                );
            }
            StrategyEvent::StateChanged(state) => {
                debug!("STATE_CHANGE {:?}", state);
            }
            StrategyEvent::ApiError(msg) => {
                eprintln!("API_ERROR {}", msg);
            }
        }
    }
}
