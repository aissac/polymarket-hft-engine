//! Pingpong - Main Entry Point
//! 
//! Phase 4.0: Gabagool Strategy - Expensive-side skew + directional betting
//! Based on RD Olivaw's proven PingPong strategy

use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn, debug, Level, error};
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
mod merger;

use api::{PolyClient, SimplifiedMarket};
use websocket::TokenData;
use orderbook::OrderBookTracker;
use strategy::{PingpongStrategy, StrategyEvent};
use trading::{TradingEngine, TradingConfig, start_trading_loop, ArbitrageSignal};
use websocket::OrderBookUpdate;
use hot_switchover::{run_hot_switchover_manager, AppState};
use pnl::{PnlTracker, create_trade_result};
use gabagool_strategy::{GabagoolStrategy, GabagoolConfig, TradingSignal};
use merger::PolyMerger;

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
        total_bankroll: 200.0, // $1000 bankroll
        max_per_market_pct: 0.05, // 5% per market
        max_total_pct: 0.15, // 15% total
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
    let trading_tx = trading_tx.clone(); // Clone for use in websocket mode
    let _trading_handle = tokio::spawn(start_trading_loop(engine, trading_rx));
    
    // Start poly merger for capital recycling
    let api_key = env::var("POLYMARKET_API_KEY").unwrap_or_default();
    let wallet = env::var("POLYMARKET_WALLET").unwrap_or_default();
    if !api_key.is_empty() && !wallet.is_empty() {
        let merger = PolyMerger::new(api_key, wallet);
        tokio::spawn(merger.run_loop());
        info!("🔄 Poly Merger started (async capital recycling)");
    } else {
        warn!("⚠️  POLYMARKET_API_KEY or POLYMARKET_WALLET not set - merger disabled");
    }
    
    if use_websocket {
        // Phase 3.6: Hot Switchover WebSocket mode with PnL tracking
        run_websocket_mode(api.clone(), strategy_tx, pnl_tracker, trading_tx).await?;
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
    trading_tx: mpsc::UnboundedSender<ArbitrageSignal>,
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
    
    // 5-min rolling capital cap tracker
    let mut capital_history: Vec<(std::time::Instant, f64)> = Vec::new();
    const CAPITAL_WINDOW_SECS: u64 = 300; // 5 minutes
    const MAX_CAPITAL_PER_WINDOW: f64 = 75.0; // $250 max per 5-min window
    
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
                    // Profit per share = $0.98 (net after 2% fee) - combined cost
                    let profit_per_share = 0.98 - combined;
                    // Edge % = profit / combined cost
                    let edge_pct = (profit_per_share / combined) * 100.0;
                    // Keep edge for backward compatibility
                    let edge = profit_per_share;
                    
                    // Sweet spot filter: only combined $0.70-$0.90, edge 10-30%
                    // Lag compensation: During high-volatility periods (price swings),
                        // require higher edge to survive 500ms sequencer delay
                        // Markets with high price velocity will move against us during wait
                        // So we demand more edge as a buffer
                        let price_spread = (yes_price - no_price).abs();
                        let is_high_vol = price_spread > 0.40 || combined < 0.75 || combined > 0.88;
                        
                        // Dynamic thresholds: tighter during volatility, normal otherwise
                        let (min_edge, max_combined) = if is_high_vol {
                            (0.15, 0.88) // Need 15%+ edge, combined ≤ $0.88
                        } else {
                            (0.10, 0.90) // Normal: 10%+ edge, combined ≤ $0.90
                        };
                        
                        let in_sweet_spot = combined >= 0.70 && combined <= max_combined;
                        let good_edge = edge >= min_edge && edge <= 0.35; // widened to capture real edges
                    
                    // Log periodically (every 5 seconds)
                    if last_log_time.elapsed().as_secs() > 5 {
                        info!(
                            "📋 {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Profit: ${:.4}/sh | Edge: {:.1}%",
                            &condition_id[..8.min(condition_id.len())],
                            yes_price,
                            no_price,
                            combined,
                            profit_per_share,
                            edge_pct,
                        );
                    }
                    
                    // Handle if in sweet spot
                    if in_sweet_spot && good_edge {
                        // Get actual top-of-book prices AND depths for dynamic sizing
                        // Get actual top-of-book sizes from orderbook tracker
                        let (yes_depth, no_depth): (f64, f64) = {
                            let ob = state.orderbook.lock();
                            if let Some(update) = ob.get(&condition_id) {
                                // Use the tracked size, but we need both YES and NO
                                // Since we only track one size per condition, use it for both
                                let size = update.size;
                                (size, size)
                            } else {
                                continue; // No orderbook data yet
                            }
                        };

                        // Dynamic sizing: use BOTH edges' available liquidity (IOC protection)
                        // Size to the limiting side to avoid sweeping into bad prices
                        let min_depth = yes_depth.min(no_depth);
                        
                        // Skip if liquidity < 10 shares (filter ghost liquidity)
                        if min_depth < 10.0 {
                            continue;
                        }
                        
                        // ═══════════════════════════════════════════════════════
                        // RISK MANAGEMENT CHECKS (NotebookLM recommendations)
                        // ═══════════════════════════════════════════════════════
                        
                        // 1. VELOCITY LOCKOUT - Don't trade if price moving too fast
                        if state.check_velocity_lockout(&condition_id) {
                            debug!("⏸️  Skipping due to velocity lockout: {}", &condition_id[..8.min(condition_id.len())]);
                            continue;
                        }
                        
                        // 2. VOLATILITY FILTER - Only trade in moving markets
                        if state.check_volatility(&condition_id) {
                            continue;
                        }
                        
                        // 3. INVENTORY SKEW - Don't get too imbalanced
                        if state.check_inventory_skew() {
                            info!("⚠️  Skipping trade due to inventory imbalance");
                            continue;
                        }
                        
                        // 4. DRAWDOWN LIMIT - Stop if losing too much today
                        if state.check_drawdown_limit() {
                            warn!("🚨 DRAWDOWN LIMIT REACHED - Trading paused for safety");
                            state.halt_trading();
                            continue;
                        }

                        // Dynamic sizing: edge buckets, but capped by ACTUAL top-of-book depth
                        // This ensures we only trade what the orderbook can provide at profitable prices
                        let base_shares = if edge >= 0.25 {
                            50  // 25%+ edge: was 100, reduced for better fill rate
                        } else if edge >= 0.20 {
                            40  // 20-25% edge: was 75
                        } else if edge >= 0.15 {
                            30  // 15-20% edge: was 50
                        } else {
                            20  // 10-15% edge: was 30
                        };
                        
                        // Cap by actual available depth (IOC protection - don't sweep the book)
                        let max_depth_shares = (min_depth * 0.80) as i32; // Use 80% of available to leave headroom
                        let shares = base_shares.min(max_depth_shares).max(10);
                        
                        // HARD CAP: 0 max per trade (notebooklm recommendation)
                        let max_cap_shares = ((50.0 / combined) as i32).max(10);
                        let shares = shares.min(max_cap_shares);
                        let shares_f = shares as f64;
                        let shares_f = shares as f64;
                        
                        info!(
                            "🎯 SWEET SPOT ARB! | {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Edge: {:.1}% | Size: {:.0}",
                            &condition_id[..8.min(condition_id.len())],
                            yes_price,
                            no_price,
                            combined,
                            edge * 100.0,
                            shares_f
                        );
                        
                        // Calculate profit for minimum trade check
                        // Scale minimum based on shares: require at least $0.10 per share
                        let min_profit = shares_f * 0.10;
                        let gross_profit = shares_f - combined * shares_f - (combined * shares_f * 0.02);
                        let net_profit = gross_profit - 0.003;
                        
                        // Only execute if net profit >= minimum threshold
                        if net_profit < min_profit {
                            // Below minimum trade value - skip
                            info!("BLOCKED: net_profit ${:.4} < min_profit ${:.4} for {} | Edge: {:.1}% | Size: {:.0}", net_profit, min_profit, &condition_id[..8.min(condition_id.len())], edge * 100.0, shares_f);
                            continue;
                        }
                        
                        // Send to trading engine for real execution
                        pnl_tracker.record_arb_opportunity();
                        pnl_tracker.record_fill_attempt();
                        
                        // Check 5-min rolling capital cap
                        let now = std::time::Instant::now();
                        let trade_capital = combined * shares_f; // Dynamic capital based on shares
                        
                        // Remove entries older than 5 minutes
                        capital_history.retain(|(time, _)| now.duration_since(*time).as_secs() < CAPITAL_WINDOW_SECS);
                        
                        // Sum capital deployed in window
                        let current_capital: f64 = capital_history.iter().map(|(_, c)| c).sum();
                        
                        if current_capital + trade_capital > MAX_CAPITAL_PER_WINDOW {
                            // Cap hit - skip this trade
                            info!("BLOCKED: CAPITAL WINDOW ${:.2}+${:.2}>${:.2} for {}", current_capital, trade_capital, MAX_CAPITAL_PER_WINDOW, &condition_id[..8.min(condition_id.len())]);
                            continue;
                        }
                        
                        // Record this trade's capital
                        capital_history.push((now, trade_capital));
                        
                        // Create token data for YES and NO
                        // Get NO token from state mapping
                        let no_token_id = state.token_side.lock()
                            .iter()
                            .find(|(_, (cond, is_yes))| cond == &condition_id && !is_yes)
                            .map(|(token_id, _)| token_id.clone())
                            .unwrap_or_else(|| update.token_id.clone());
                        let yes_token = TokenData {
                            token_id: update.token_id.clone(),
                            outcome: "yes".to_string(),
                            price: Some(yes_price),
                        };
                        let no_token = TokenData {
                            token_id: no_token_id.clone(),
                            outcome: "no".to_string(),
                            price: Some(no_price),
                        };
                        
                        // Create simplified market for trading signal
                        let market = SimplifiedMarket {
                            condition_id: condition_id.clone(),
                            question: String::new(),
                            tokens: vec![yes_token, no_token],
                            active: true,
                            closed: false,
                            hours_until_resolve: 0,
                            slug: String::new(),
                        };
                        
                        // Send to trading engine
                        let profit_estimate = shares_f - combined * shares_f - (combined * shares_f * 0.02) - 0.003;
                        let signal = ArbitrageSignal {
                            market,
                            profit: profit_estimate,
                            size: shares_f,
                            yes_depth,
                            no_depth,
                        };
                        
                        if trading_tx.send(signal).is_err() {
                            error!("Failed to send trading signal - channel closed");
                        } else {
                            info!("📤 Trade signal sent to engine (profit est: ${:.2})", profit_estimate);
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
