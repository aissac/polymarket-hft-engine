//! Pingpong - Main Entry Point
//! 
//! Phase 4.0: Gabagool Strategy - Expensive-side skew + directional betting
//! Based on RD Olivaw's proven PingPong strategy

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
mod hot_switchover;
mod pnl;
mod gabagool_strategy;

use api::PolyClient;
use orderbook::OrderBookTracker;
use strategy::{PingpongStrategy, StrategyEvent};
use trading::{TradingEngine, TradingConfig, start_trading_loop, ArbitrageSignal};
use websocket::OrderBookUpdate;
use hot_switchover::{run_hot_switchover_manager, AppState};
use pnl::{PnlTracker, create_trade_result};
use gabagool_strategy::{GabagoolStrategy, GabagoolConfig, TradingSignal};

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
    info!("   PINGPONG v0.4.0 - Gabagool Strategy");
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
        info!("📡 Using Hot Switchover WebSocket (primary + backup)");
    } else {
        info!("🌐 Using REST API (slower polling mode)");
    }
    
    // Get private key from env
    let private_key = env::var("POLYMARKET_PRIVATE_KEY")
        .unwrap_or_else(|_| {
            warn!("POLYMARKET_PRIVATE_KEY not set - running in read-only mode");
            String::new()
        });
    
    // Initialize PnL tracker
    let pnl_tracker = PnlTracker::new(Some("/tmp/pnl_trades.jsonl"));
    info!("📊 PnL Tracker initialized");
    
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
        // Phase 3.6: Hot Switchover WebSocket mode with PnL tracking
        run_websocket_mode(api.clone(), strategy_tx, pnl_tracker).await?;
    } else {
        // Phase 2: REST polling mode
        run_rest_mode(api.clone(), strategy_tx).await?;
    }
    
    Ok(())
}

/// REST polling mode (Phase 2)
async fn run_rest_mode(
    api: Arc<PolyClient>,
    strategy_tx: mpsc::UnboundedSender<StrategyEvent>,
) -> anyhow::Result<()> {
    use orderbook::OrderBookTracker;
    
    let tracker = Arc::new(OrderBookTracker::new());
    info!("✅ OrderBookTracker initialized");
    
    let mut strategy = PingpongStrategy::new(
        tracker,
        api.clone(),
        strategy_tx,
    );
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

/// Hot Switchover WebSocket mode (Phase 3.6)
async fn run_websocket_mode(
    api: Arc<PolyClient>,
    strategy_tx: mpsc::UnboundedSender<StrategyEvent>,
    pnl_tracker: PnlTracker,
) -> anyhow::Result<()> {
    info!("🚀 Starting Hot Switchover WebSocket mode...");
    
    // First, get initial market list via REST
    let markets = api.get_markets().await?;
    info!("📊 Fetched {} markets from REST API", markets.len());
    
    // Collect all YES/NO token IDs
    let mut tokens: Vec<String> = Vec::new();
    
    for market in &markets {
        if let Some(yes_id) = market.yes_token_id() {
            tokens.push(yes_id);
        }
        if let Some(no_id) = market.no_token_id() {
            tokens.push(no_id);
        }
    }
    
    // Create shared application state
    let state = Arc::new(AppState::new());
    
    // Create channel for director to send updates
    let (update_tx, mut update_rx) = mpsc::unbounded_channel::<OrderBookUpdate>();
    
    // Run Hot Switchover Manager (spawns primary + backup WS)
    let tokens_for_stream = tokens.clone();
    tokio::spawn(run_hot_switchover_manager(tokens_for_stream, state.clone(), update_tx));
    
    info!("");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   🎯 PINGPONG REAL-TIME ARBITRAGE DETECTION");
    info!("   🛡️  Hot Switchover: Primary + Backup Active");
    info!("   📊 Gabagool Strategy v1.0 (expensive-side skew + directional)");
    info!("═══════════════════════════════════════════════════════════════");
    info!("   Monitoring {} markets in real-time", markets.len());
    info!("   Min edge: 0.1% | Skew: 3x when price > $0.55");
    info!("   Directional confidence: 70%+ | Position limit: 150 shares");
    info!("");
    
    // Initialize Gabagool strategy
    let gabagool = GabagoolStrategy::new(GabagoolConfig::default());
    
    // Process updates from the Trading Director
    let mut scan_count = 0u64;
    
    loop {
        match update_rx.recv().await {
            Some(update) => {
                // Update the orderbook tracker with this price
                // The update contains BOTH yes_price and no_price in the OrderBookUpdate
                // We store them and compute combined when both exist
                pnl_tracker.record_update(&update.condition_id, update.price, update.size);
                
                scan_count += 1;
                
                // Evaluate with Gabagool strategy
                // Use the combined price from tracker if available
                let combined = pnl_tracker.get_combined(&update.condition_id);
                
                // Only process if we have BOTH yes and no prices
                if let Some(combined) = combined {
                    let edge = 1.0 - combined - (combined * 0.02);
                    
                    // Sweet spot filter: only combined $0.70-$0.90, edge 10-30%
                    let in_sweet_spot = combined >= 0.70 && combined <= 0.90;
                    let good_edge = edge >= 0.10 && edge <= 0.30;
                    
                    // Log periodically
                    if scan_count % 100 == 0 {
                        info!(
                            "📋 Scan #{:06} | {} | Combined: ${:.4} | Edge: {:.1}%",
                            scan_count,
                            &update.condition_id[..8.min(update.condition_id.len())],
                            combined,
                            edge * 100.0
                        );
                    }
                    
                    // Handle if in sweet spot
                    if in_sweet_spot && good_edge {
                        let yes_price = pnl_tracker.get_yes_price(&update.condition_id).unwrap_or(0.0);
                        let no_price = pnl_tracker.get_no_price(&update.condition_id).unwrap_or(0.0);
                        let signal = gabagool.evaluate_prices(yes_price, no_price);
                        
                        info!(
                            "🎯 SWEET SPOT ARB! | Combined: ${:.4} | Edge: {:.1}%",
                            combined,
                            edge * 100.0
                        );
                        pnl_tracker.record_arb_opportunity();
                        
                        let trade = create_trade_result(
                            &update.token_id,
                            &update.condition_id,
                            yes_price,
                            no_price,
                            100.0,
                        );
                        pnl_tracker.record_trade(&trade);
                    }
                }
            }
            None => break,
        }
    }
    
    Ok(())
}
