//! Pingpong - Main Entry Point
//! 
//! Phase 3: WebSocket streaming + Concurrent Order Execution
//! Real-time orderbook via WebSocket + tokio::join! for leg execution.

use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;

mod orderbook;
mod api;
mod strategy;
mod trading;
mod websocket;

use api::PolyClient;
use strategy::{PingpongStrategy, StrategyEvent};
use trading::{TradingEngine, TradingConfig, start_trading_loop, ArbitrageSignal};
use websocket::{WSClient, OrderBookUpdate};

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
    info!("   PINGPONG v0.3.0 - WebSocket + Concurrent Execution");
    info!("═══════════════════════════════════════════════════════════════");
    
    // Parse command line args
    let args: Vec<String> = env::args().collect();
    let dry_run = !args.contains(&"--live".to_string());
    let use_websocket = args.contains(&"--ws".to_string());
    
    if dry_run {
        info!("⚠️  MODE: DRY RUN (paper trading, no real orders)");
    } else {
        info!("🔴  MODE: LIVE TRADING");
        info!("⚠️  Real money will be risked!");
    }
    
    if use_websocket {
        info!("📡 Using WebSocket for real-time orderbook streaming");
    } else {
        info!("🌐 Using REST API (slower polling mode)");
    }
    
    // Get private key from env
    let private_key = env::var("POLYMARKET_PRIVATE_KEY")
        .unwrap_or_else(|_| {
            warn!("POLYMARKET_PRIVATE_KEY not set - running in read-only mode");
            String::new()
        });
    
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
    
    if use_websocket {
        // Phase 3: WebSocket streaming mode
        run_websocket_mode(api.clone(), strategy_tx, trading_tx).await?;
    } else {
        // Phase 2: REST polling mode
        run_rest_mode(api.clone(), strategy_tx, trading_tx).await?;
    }
    
    Ok(())
}

/// REST polling mode (Phase 2)
async fn run_rest_mode(
    api: Arc<PolyClient>,
    strategy_tx: mpsc::UnboundedSender<StrategyEvent>,
    trading_tx: mpsc::UnboundedSender<ArbitrageSignal>,
) -> anyhow::Result<()> {
    use orderbook::OrderBookTracker;
    
    let tracker = Arc::new(OrderBookTracker::new());
    info!("✅ OrderBookTracker initialized");
    
    let mut strategy = PingpongStrategy::new(
        tracker,
        api.clone(),
        strategy_tx,
    ).with_trading_channel(trading_tx);
    info!("✅ Strategy engine initialized");
    
    // Check API health
    let api_clone = api.clone();
    if !api_clone.health_check().await? {
        warn!("⚠️ Polymarket API health check failed");
    }
    
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 PINGPONG SCANNING FOR ARBITRAGE OPPORTUNITIES");
    info!("═══════════════════════════════════════════════════════════════");
    info!("");
    
    strategy.run().await;
    
    Ok(())
}

/// WebSocket streaming mode (Phase 3)
async fn run_websocket_mode(
    api: Arc<PolyClient>,
    strategy_tx: mpsc::UnboundedSender<StrategyEvent>,
    _trading_tx: mpsc::UnboundedSender<ArbitrageSignal>,
) -> anyhow::Result<()> {
    info!("🚀 Starting WebSocket streaming mode...");
    
    // First, get initial market list via REST
    let markets = api.get_markets().await?;
    info!("📊 Fetched {} markets from REST API", markets.len());
    
    // Collect all YES/NO token IDs
    let mut tokens: Vec<String> = Vec::new();
    let mut condition_ids: Vec<String> = Vec::new();
    
    for market in &markets {
        tokens.push(market.yes_token_id());
        tokens.push(market.no_token_id());
        condition_ids.push(market.condition_id.clone());
    }
    
    info!("🔌 Connecting to Polymarket WebSocket...");
    
    // Create WebSocket client
    let ws_client = Arc::new(WSClient::new());
    
    // Create channel for updates
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<OrderBookUpdate>();
    
    // Spawn WebSocket streaming task
    let tokens_for_stream = tokens.clone();
    let ws_client_clone = ws_client.clone();
    
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = ws_client_clone.stream(tokens_for_stream, move |update| {
            let _ = update_tx.send(update);
        }).await {
            tracing::error!("WebSocket error: {}", e);
        }
    });
    
    info!("✅ WebSocket connected and streaming");
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 PINGPONG REAL-TIME ARBITRAGE DETECTION");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   Monitoring {} markets in real-time", condition_ids.len());
    info!("");
    
    // Process WebSocket updates
    let mut scan_count = 0u64;
    let target_hedge = 1.05; // Combined cost threshold
    
    loop {
        match update_rx.recv().await {
            Some(update) => {
                scan_count += 1;
                
                // Check for arbitrage on this specific market
                if let Some((yes_ask, no_ask)) = ws_client.get_best_prices(&update.condition_id) {
                    let combined = yes_ask + no_ask;
                    
                    // Log periodically
                    if scan_count % 100 == 0 {
                        info!(
                            "📋 {} | YES: ${:.4} | NO: ${:.4} | COMBINED: ${:.4}",
                            &update.condition_id[..8.min(update.condition_id.len())],
                            yes_ask,
                            no_ask,
                            combined
                        );
                    }
                    
                    // Check for arbitrage opportunity
                    if combined < target_hedge {
                        let profit = 1.0 - combined - (combined * 0.02);
                        info!(
                            "🎯 ARBITRAGE OPPORTUNITY! {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Profit: ${:.4}/share",
                            &update.condition_id[..8.min(update.condition_id.len())],
                            yes_ask,
                            no_ask,
                            combined,
                            profit
                        );
                        
                        // Send event
                        let _ = strategy_tx.send(StrategyEvent::ArbitrageDetected {
                            market: update.condition_id.clone(),
                            yes_ask,
                            no_ask,
                            combined,
                            profit,
                        });
                    }
                }
            }
            None => {
                warn!("WebSocket update channel closed");
                break;
            }
        }
    }
    
    ws_handle.abort();
    
    Ok(())
}
