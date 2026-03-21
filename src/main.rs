//! Pingpong - Main Entry Point
//! 
//! Phase 2: REST API + Orderbook + Order Execution
//! Detects arbitrage and executes trades via polymarket-client-sdk.

use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

mod orderbook;
mod api;
mod strategy;
mod trading;

use orderbook::OrderBookTracker;
use api::PolyClient;
use strategy::{PingpongStrategy, StrategyEvent};
use trading::{TradingEngine, TradingConfig, start_trading_loop, ArbitrageSignal};

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
    info!("   PINGPONG v0.2.0 - Phase 2: Order Execution");
    info!("═══════════════════════════════════════════════════════════════");
    
    // Parse command line args
    let args: Vec<String> = env::args().collect();
    let dry_run = !args.contains(&"--live".to_string());
    
    if dry_run {
        info!("⚠️  MODE: DRY RUN (paper trading, no real orders)");
    } else {
        info!("🔴  MODE: LIVE TRADING");
        info!("⚠️  Real money will be risked!");
    }
    
    // Get private key from env
    let private_key = env::var("POLYMARKET_PRIVATE_KEY")
        .unwrap_or_else(|_| {
            warn!("POLYMARKET_PRIVATE_KEY not set - running in read-only mode");
            String::new()
        });
    
    // Initialize shared state
    let tracker = Arc::new(OrderBookTracker::new());
    info!("✅ OrderBookTracker initialized");
    
    // Initialize API client
    let api = Arc::new(PolyClient::new());
    info!("✅ Polymarket API client initialized");
    
    // Create event channel for strategy events
    let (strategy_tx, _strategy_rx) = mpsc::unbounded_channel::<StrategyEvent>();
    info!("✅ Event channel created");
    
    // Create channel for trading signals
    let (trading_tx, trading_rx) = mpsc::unbounded_channel::<ArbitrageSignal>();
    
    // Initialize trading engine
    let trading_config = TradingConfig {
        dry_run,
        min_profit: 0.01,
        max_leg_usdc: 50.0,
    };
    let mut engine = TradingEngine::new(trading_config);
    
    // Initialize auth if private key is available
    if !private_key.is_empty() {
        if let Err(e) = engine.init(&private_key).await {
            warn!("⚠️ Authentication failed: {} - continuing in read-only mode", e);
        }
    } else {
        info!("ℹ️  No private key - running in read-only mode");
    }
    
    // Start trading loop
    let _trading_handle = tokio::spawn(start_trading_loop(engine, trading_rx));
    
    // Start strategy engine with trading channel
    let mut strategy = PingpongStrategy::new(
        tracker.clone(), 
        api.clone(), 
        strategy_tx,
    ).with_trading_channel(trading_tx);
    info!("✅ Strategy engine initialized");
    
    // Check API health
    if !api.health_check().await? {
        warn!("⚠️ Polymarket API health check failed");
    }
    
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 PINGPONG SCANNING FOR ARBITRAGE OPPORTUNITIES");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    
    // Run strategy (blocks forever)
    strategy.run().await;
    
    Ok(())
}
