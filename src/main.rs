//! Pingpong - Main Entry Point
//! 
//! Phase 1: REST API + Orderbook tracking (READ-ONLY)
//! Proves the data pipeline before real trading.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

mod orderbook;
mod api;
mod strategy;

use orderbook::OrderBookTracker;
use api::PolyClient;
use strategy::{PingpongStrategy, start_event_logger, StrategyEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set tracing subscriber");
    
    info!("═══════════════════════════════════════════════════════════════");
    info!("   GABAGOOL BOT v0.1.0 - Phase 1: REST API + Orderbook");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    info!("⚠️  PHASE 1 MODE: READ-ONLY - No trading, just monitoring");
    info!("");
    info!("Target: ${:.2} combined cost for arbitrage (includes 2% fee buffer)", 0.95);
    info!("");
    
    // Initialize shared state
    let tracker = Arc::new(OrderBookTracker::new());
    info!("✅ OrderBookTracker initialized");
    
    // Initialize API client
    let api = Arc::new(PolyClient::new());
    info!("✅ Polymarket API client initialized");
    
    // Create event channel
    let (event_tx, event_rx) = mpsc::unbounded_channel::<StrategyEvent>();
    info!("✅ Event channel created");
    
    // Start event logger
    let _logger_handle = tokio::spawn(start_event_logger(event_rx));
    
    // Start strategy engine
    let mut strategy = PingpongStrategy::new(tracker.clone(), api.clone(), event_tx);
    info!("✅ Strategy engine initialized");
    
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 GABAGOOL SCANNING FOR ARBITRAGE OPPORTUNITIES");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    
    // Run strategy (blocks forever)
    strategy.run().await;
    
    Ok(())
}
