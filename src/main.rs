//! Gabagool Bot - Main Entry Point
//! 
//! Phase 1: WebSocket + Orderbook tracking (READ-ONLY)
//! This proves the data pipeline before any real trading.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

mod orderbook;
mod websocket;
mod strategy;

use orderbook::OrderBookTracker;
use websocket::PolymarketWsClient;
use strategy::{GabagoolStrategy, start_event_logger, StrategyEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();
    
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set tracing subscriber");
    
    info!("═══════════════════════════════════════════════════════════════");
    info!("   GABAGOOL BOT v0.1.0 - Phase 1: WebSocket + Orderbook");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    info!("⚠️  PHASE 1 MODE: READ-ONLY - No trading, just monitoring");
    info!("");
    info!("Target: ${:.2} combined cost for arbitrage (includes 2% fee buffer)", 0.95);
    info!("");
    
    // Initialize shared state
    let tracker = Arc::new(OrderBookTracker::new());
    info!("✅ OrderBookTracker initialized");
    
    // Create event channel for strategy → logger
    let (event_tx, event_rx) = mpsc::unbounded_channel::<StrategyEvent>();
    info!("✅ Event channel created");
    
    // Initialize WebSocket client
    let ws_client = PolymarketWsClient::new(tracker.clone());
    info!("✅ WebSocket client created");
    
    // Start event logger (async, runs in background)
    let _logger_handle = tokio::spawn(start_event_logger(event_rx));
    
    // Start WebSocket connection
    info!("");
    info!("🔌 Connecting to Polymarket CLOB WebSocket...");
    if let Err(e) = ws_client.connect().await {
        warn!("WebSocket connection failed: {}. Running in fallback mode.", e);
    } else {
        info!("✅ WebSocket connected!");
    }
    
    // Start strategy engine
    let mut strategy = GabagoolStrategy::new(tracker.clone(), event_tx);
    info!("✅ Strategy engine initialized");
    
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 GABAGOOL SCANNING FOR ARBITRAGE OPPORTUNITIES");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    
    // Run strategy (this blocks forever)
    strategy.run().await;
    
    Ok(())
}
