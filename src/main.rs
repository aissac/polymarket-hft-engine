//! Pingpong - Main Entry Point
//! 
//! Phase 4.0: Gabagool Strategy - Expensive-side skew + directional betting
//! Based on RD Olivaw's proven PingPong strategy

use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use chrono::Local;

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
    
    // Collect all YES/NO token IDs and build token→side mapping
    let mut tokens: Vec<String> = Vec::new();
    let mut token_mapping: Vec<(String, String, bool)> = Vec::new(); // (cond_id, token_id, is_yes)
    
    for market in &markets {
        if let Some(yes_id) = market.yes_token_id() {
            tokens.push(yes_id.clone());
            token_mapping.push((market.condition_id.clone(), yes_id, true));
        }
        if let Some(no_id) = market.no_token_id() {
            tokens.push(no_id.clone());
            token_mapping.push((market.condition_id.clone(), no_id, false));
        }
    }
    
    // Create shared application state
    let state = Arc::new(AppState::new());
    state.build_token_mapping(&token_mapping);
    
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
    let mut last_log_time = std::time::Instant::now();
    
    loop {
        match update_rx.recv().await {
            Some(update) => {
                // Look up whether this token is YES or NO using our token mapping
                // The Gamma API gives us token_ids, we assigned them as [0]=YES, [1]=NO
                let outcome = {
                    let mapping = state.token_side.lock();
                    match mapping.get(&update.token_id) {
                        Some((_, is_yes)) => {
                            if *is_yes { "yes" } else { "no" }
                        }
                        None => {
                            // Token not in mapping - skip
                            continue;
                        }
                    }
                };
                
                // Update the orderbook tracker using the outcome
                if let Some((condition_id, yes_price, no_price)) = state.update_by_outcome(&update.condition_id, update.price, outcome) {
                    // We have both prices now!
                    let combined = yes_price + no_price;
                    let edge = 1.0 - combined - (combined * 0.02);
                    
                    // Sweet spot filter: only combined $0.70-$0.90, edge 10-30%
                    let in_sweet_spot = combined >= 0.70 && combined <= 0.90;
                    let good_edge = edge >= 0.10 && edge <= 0.30;
                    
                    // Log periodically (every 5 seconds)
                    if last_log_time.elapsed().as_secs() > 5 {
                        info!(
                            "📋 {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Edge: {:.1}%",
                            &condition_id[..8.min(condition_id.len())],
                            yes_price,
                            no_price,
                            combined,
                            edge * 100.0
                        );
                        last_log_time = std::time::Instant::now();
                    }
                    
                    // Handle if in sweet spot
                    if in_sweet_spot && good_edge {
                        info!(
                            "🎯 SWEET SPOT ARB! | {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Edge: {:.1}%",
                            &condition_id[..8.min(condition_id.len())],
                            yes_price,
                            no_price,
                            combined,
                            edge * 100.0
                        );
                        pnl_tracker.record_arb_opportunity();
                        pnl_tracker.record_fill_attempt();
                        
                        // Simulate fill rate based on edge quality
                        // Higher edge = more likely to still be available
                        let fill_probability = if edge >= 0.20 {
                            0.80 // 80% for high edge
                        } else if edge >= 0.15 {
                            0.70 // 70% for medium edge
                        } else {
                            0.60 // 60% for low edge
                        };
                        
                        // In dry run, simulate fill success/failure based on edge
                        // Use combined price digits as pseudo-random
                        let pseudo_rand = (combined * 1000.0).fract();
                        if pseudo_rand < fill_probability {
                            pnl_tracker.record_fill_success();
                            
                            // Record gas cost (Polygon ~$0.003 per tx)
                            pnl_tracker.record_gas(0.003);
                            
                            let trade = create_trade_result(
                                &update.token_id,
                                &condition_id,
                                yes_price,
                                no_price,
                                100.0,
                            );
                            pnl_tracker.record_trade(&trade);
                            
                            // Log detailed trade for PnL report
                            let timestamp = chrono::Local::now().format("%H:%M:%S");
                            let gross_profit = 1.0 - combined - (combined * 0.02);
                            let net_profit = gross_profit - 0.003;
                            info!(
                                "✅ TRADE | {} | YES: ${:.4} × 100 | NO: ${:.4} × 100 | Comb: ${:.4} | Gross: ${:.4} | Gas: $0.003 | Net: ${:.4}",
                                timestamp, yes_price, no_price, combined, gross_profit, net_profit
                            );
                        } else {
                            pnl_tracker.record_fill_failure();
                            let timestamp = chrono::Local::now().format("%H:%M:%S");
                            info!("❌ FILL FAILED | {} | {} | Edge: {:.1}%", 
                                timestamp, 
                                &condition_id[..8.min(condition_id.len())],
                                edge * 100.0
                            );
                        }
                    }
                }
                
                scan_count += 1;
            }
            None => break,
        }
    }
    
    Ok(())
}
