//! Pingpong Strategy Engine
//! 
//! Phase 2: Read-only monitoring + Trading execution
//! Monitors for arbitrage and signals the trading engine.
//! 
//! Strategy: Focus on realistic, fillable opportunities
//! - Sweet spot: Combined $0.70-$0.90 (less competition from HFT)
//! - Minimum edge: 10% (filter out noise)
//! - Maximum edge: 30% (extremely rare = likely stale)
//! - Avoid >50% edges (usually stale/unfillable data)

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{info, debug, warn};

use crate::orderbook::OrderBookTracker;
use crate::api::{PolyClient, SimplifiedMarket};
use crate::trading::ArbitrageSignal;

/// Target combined cost - only trade in the "sweet spot"
/// Combined $0.70-$0.90 = less competition, more fillable
const SWEET_SPOT_MIN: f64 = 0.70;
const SWEET_SPOT_MAX: f64 = 0.90;

/// Minimum profit margin (after 2% fee) to consider a trade
const MIN_EDGE: f64 = 0.10; // 10% minimum profit

/// Maximum edge to consider (anything higher is likely stale)
const MAX_EDGE: f64 = 0.30; // 30% max (beyond this = suspicious)

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
    /// Optional channel to send arbitrage signals to trading engine
    trading_tx: Option<mpsc::UnboundedSender<ArbitrageSignal>>,
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
            trading_tx: None,
        }
    }
    
    /// With trading channel - enables order execution
    pub fn with_trading_channel(
        mut self,
        tx: mpsc::UnboundedSender<ArbitrageSignal>,
    ) -> Self {
        self.trading_tx = Some(tx);
        self
    }
    
    pub fn set_trading_channel(&mut self, tx: Option<mpsc::UnboundedSender<ArbitrageSignal>>) {
        self.trading_tx = tx;
    }
    
    /// Main strategy loop (Phase 2: monitoring + trading)
    pub async fn run(&mut self) {
        info!("🚀 Pingpong Strategy starting (SWEET SPOT MODE)");
        info!("📊 Sweet spot: ${:.0}-${:.0} combined (edge: {:.0}%-{:.0}%)", 
              SWEET_SPOT_MIN * 100.0, SWEET_SPOT_MAX * 100.0, MIN_EDGE * 100.0, MAX_EDGE * 100.0);
        info!("📡 Trading mode: {}", 
              if self.trading_tx.is_some() { "ENABLED" } else { "READ-ONLY" });
        
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
                    // Log scan periodically
                    if scan_count % 20 == 0 {
                        info!("📊 Scan #{}: {} markets | Sweet spot ARBs this cycle: looking...", 
                              scan_count, markets.len());
                    }
                    
                    // Update tracker and check for arbitrage
                    for market in &markets {
                        let yes_price = market.yes_price();
                        let no_price = market.no_price();
                        let combined = yes_price.zip(no_price).map(|(y, n)| y + n);
                        
                        self.tracker.update(
                            &market.condition_id,
                            yes_price,
                            no_price,
                        );
                        
                        // Check if this market has arbitrage in the SWEET SPOT
                        // Only trade combined $0.70-$0.90 (realistic, fillable)
                        if let Some(combined_val) = combined {
                            // Calculate edge after 2% fee
                            let edge = 1.0 - combined_val - (combined_val * 0.02);
                            
                            // Check if in sweet spot (70-90 cents combined)
                            let in_sweet_spot = combined_val >= SWEET_SPOT_MIN && combined_val <= SWEET_SPOT_MAX;
                            
                            // Only trade if in sweet spot AND has reasonable edge (10-30%)
                            if in_sweet_spot && edge >= MIN_EDGE && edge <= MAX_EDGE {
                                info!(
                                    "🎯 SWEET SPOT ARB: {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Edge: {:.1}%",
                                    &market.condition_id[..8.min(market.condition_id.len())],
                                    yes_price.unwrap_or(0.0),
                                    no_price.unwrap_or(0.0),
                                    combined_val,
                                    edge * 100.0
                                );
                                
                                // Send event for logging
                                let _ = self.events.send(StrategyEvent::ArbitrageDetected {
                                    market: market.condition_id.clone(),
                                    yes_ask: yes_price.unwrap_or(0.0),
                                    no_ask: no_price.unwrap_or(0.0),
                                    combined: combined_val,
                                    profit: edge,
                                });
                                
                                // Send to trading engine if channel is available
                                if let Some(ref tx) = self.trading_tx {
                                    let signal = ArbitrageSignal {
                                        market: market.clone(),
                                        profit: edge,
                                        size: 100.0,
                                        yes_depth: 200.0,
                                        no_depth: 200.0,
                                    };
                                    if let Err(e) = tx.send(signal) {
                                        warn!("Failed to send trading signal: {}", e);
                                    }
                                }
                            }
                            
                            // Log high-edge opportunities (stale) occasionally
                            if edge > MAX_EDGE && scan_count % 50 == 0 {
                                debug!(
                                    "⚠️ High edge (stale?): {} | Combined: ${:.4} | Edge: {:.1}%",
                                    &market.condition_id[..8.min(market.condition_id.len())],
                                    combined_val,
                                    edge * 100.0
                                );
                            }
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
