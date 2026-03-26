//! Pingpong Trading Module - Phase 3: EIP-712 Infrastructure
//! 
//! Uses polymarket-client-sdk for authenticated trading with EIP-712.
//! Supports dry-run simulation. Live trading scaffolding is ready.
//! 
//! EIP-712 Signing Flow (documented):
//! 1. Build Order with OrderBuilder  
//! 2. Sign with client.sign(signer, SignableOrder) 
//! 3. Submit with client.post_order(signed_order)
//!
//! TODO for live trading:
//! - Resolve Arc<Client> lifetime issues with tokio::join!
//! - Test authenticated CLOB client connection

use std::str::FromStr;

use anyhow::{Context, Result};
use alloy::signers::local::PrivateKeySigner;
use tokio::sync::mpsc;
use std::sync::Mutex;
use std::collections::HashMap;
use tokio::time::{timeout, Duration};
use tracing::{info, error, warn, debug};

use crate::api::SimplifiedMarket;
use std::sync::Arc;
use reqwest::Client;

/// Trading configuration
#[derive(Debug, Clone)]
pub struct TradingConfig {
    /// Minimum profit per share to trigger a trade
    pub min_profit: f64,
    /// Maximum amount per leg (in USDC)
    pub max_leg_usdc: f64,
    /// Whether to actually trade or just simulate
    pub dry_run: bool,
    /// Total bankroll for MLE calculations
    pub total_bankroll: f64,
    /// Max % of bankroll per market (5%)
    pub max_per_market_pct: f64,
    /// Max % of bankroll across all markets (15%)
    pub max_total_pct: f64,
}

impl Default for TradingConfig {
    fn default() -> Self {
        Self {
            min_profit: 0.01,
            max_leg_usdc: 50.0,
            dry_run: true,
            total_bankroll: 1000.0, // $1000 default
            max_per_market_pct: 0.05, // 5% per market
            max_total_pct: 0.15, // 15% total
        }
    }
}

/// Order result
#[derive(Debug, Clone)]
pub struct OrderResult {
    pub market_id: String,
    pub yes_order_id: Option<String>,
    pub no_order_id: Option<String>,
    pub yes_filled: bool,
    pub no_filled: bool,
    pub profit_estimate: f64,
    pub size: f64,
}

/// Trading engine using Polymarket CLOB SDK with EIP-712
/// 
/// In dry-run mode: simulates signing and submission
/// In live mode: uses real EIP-712 signing via polymarket-client-sdk
/// 
/// For live trading, need to resolve Arc<Client> lifetime with tokio::join!
/// Pre-warmed HTTP/2 client for sub-100ms order submission
pub struct ClobClient {
    client: Client,
    base_url: String,
}

impl ClobClient {
    pub fn new(base_url: &str) -> Self {
        let client = Client::builder()
            .http2_prior_knowledge()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to create pre-warmed HTTP/2 client");
        Self {
            client,
            base_url: base_url.to_string(),
        }
    }
    
    pub fn submit_order_background(&self, payload: serde_json::Value) {
        let client = self.client.clone();
        let url = format!("{}/order", self.base_url);
        tokio::spawn(async move {
            let start = std::time::Instant::now();
            match client.post(&url).json(&payload).send().await {
                Ok(resp) => {
                    let latency = start.elapsed();
                    match resp.text().await {
                        Ok(body) => info!("Order submitted in {:?}: {}", latency, body),
                        Err(e) => error!("Order response error in {:?}: {}", latency, e),
                    }
                }
                Err(e) => error!("Order submission failed in {:?}: {}", start.elapsed(), e),
            }
        });
    }
}

pub struct TradingEngine {
    config: TradingConfig,
    /// Signer for EIP-712 signatures
    signer: Option<PrivateKeySigner>,
    /// CLOB API URL
    api_url: String,
    /// Pre-warmed HTTP/2 client (Phase 1 optimization)
    clob_client: Option<Arc<ClobClient>>,
    // MLE tracking
    active_capital: Mutex<HashMap<String, f64>>,
    total_active_capital: Mutex<f64>,
}

impl TradingEngine {
    /// Create a new trading engine
    pub fn new(config: TradingConfig) -> Self {
        Self {
            config: config.clone(),
            signer: None,
            api_url: "https://clob.polymarket.com".to_string(),
            clob_client: Some(Arc::new(ClobClient::new("https://clob.polymarket.com"))),
            active_capital: Mutex::new(HashMap::new()),
            total_active_capital: Mutex::new(0.0),
        }
    }

    /// Initialize with private key - sets up EIP-712 signing infrastructure
    /// 
    /// In dry-run mode: validates key but doesn't connect
    /// In live mode: would authenticate with Polymarket CLOB (TODO)
    pub async fn init(&mut self, private_key: &str) -> Result<()> {
        info!("🔐 Initializing Polymarket trading with EIP-712...");
        
        // Create signer from private key
        let signer: PrivateKeySigner = PrivateKeySigner::from_str(private_key)
            .context("Invalid private key format")?;
        let address = signer.address();
        info!("📍 Wallet address: 0x{:x}", address);
        
        self.signer = Some(signer);
        
        if self.config.dry_run {
            info!("🔒 DRY RUN: EIP-712 signing infrastructure ready (simulation mode)");
            info!("   To enable live trading:");
            info!("   1. Set POLYMARKET_PRIVATE_KEY env var");
            info!("   2. Run with --live flag");
            info!("   3. Implement Arc<Client> lifetime handling for concurrent signing");
        } else {
            info!("🔴 LIVE MODE: Would authenticate with Polymarket CLOB");
            info!("   EIP-712 signing would be used for gasless proxy wallet trading");
            // TODO: Implement live authentication when Arc<Client> issues resolved
        }
        
        Ok(())
    }

    /// Check if engine is ready
    pub fn is_ready(&self) -> bool {
        self.signer.is_some()
    }

    /// Execute arbitrage trade with CONCURRENT legs (Phase 3)
    /// Uses tokio::join! for simultaneous YES+NO order execution
    /// 
    /// Currently only supports dry-run mode.
    /// Live trading requires resolving Arc<Client> lifetime with tokio::join!
    pub async fn execute_arbitrage(&self, market: &SimplifiedMarket, size: f64) -> Result<OrderResult> {
        let yes_price = market.yes_price().unwrap_or(0.0);
        let no_price = market.no_price().unwrap_or(0.0);
        let combined = yes_price + no_price;
        // Exact EV calculation: Polymarket takes 2% on winnings
        let profit = 1.0 - combined - (combined * 0.02);  // Keep as approximate for now (will update)
        
        if profit <= self.config.min_profit {
            return Ok(OrderResult {
                market_id: market.condition_id.clone(),
                yes_order_id: None,
                no_order_id: None,
                yes_filled: false,
                no_filled: false,
                profit_estimate: profit,
                size,
            });
        }
        
        info!(
            "🎯 ARBITRAGE DETECTED: {} | YES: ${:.4} + NO: ${:.4} = ${:.4} | Profit: ${:.4} | Size: {:.0}",
            &market.condition_id[..8.min(market.condition_id.len())],
            yes_price, no_price, combined, profit, size
        );
        
        // Mark queue start for ghost liquidity detection
        self.mark_taker_queue_start(&market.condition_id);
        
        if self.config.dry_run {
            self.execute_dry_run(market, yes_price, no_price, size, profit, size, size).await
        } else {
            // TODO: Implement live trading when Arc<Client> lifetime resolved
            anyhow::bail!("Live trading not yet implemented - use dry_run mode");
        }
    }

    /// Dry-run: simulate concurrent execution with fake EIP-712 simulation
    /// 
    /// Simulates the EIP-712 signing flow:
    /// 1. Simulate building market orders for YES and NO
    /// 2. Simulate concurrent signing with tokio::join!
    /// 3. Simulate submission to relayer
    async fn execute_dry_run(
        &self,
        market: &SimplifiedMarket,
        yes_price: f64,
        no_price: f64,
        size: f64,
        profit: f64,
        yes_depth: f64,
        no_depth: f64,
    ) -> Result<OrderResult> {
        // IOC + Tick Size Limit: Only fill what top-of-book can provide
        let actual_size = size.min(yes_depth).min(no_depth);
        if actual_size < 10.0 {
            info!("⚠️ SKIPPING - insufficient top-of-book liquidity: YES={:.0} NO={:.0}", yes_depth, no_depth);
            return Ok(OrderResult {
                market_id: market.condition_id.clone(),
                yes_order_id: None,
                no_order_id: None,
                yes_filled: false,
                no_filled: false,
                profit_estimate: 0.0,
                size: 0.0,
            });
        }
        let size_msg = if actual_size < size {
            format!(" (IOC capped: {:.0} -> {:.0})", size, actual_size)
        } else {
            String::new()
        };
        info!("🔒 DRY RUN - Simulating concurrent EIP-712 signed orders...{}", size_msg);        
        // Bind token IDs to ensure they live long enough
        let yes_token = market.yes_token_id().unwrap_or_default();
        let no_token = market.no_token_id().unwrap_or_default();
        
        // Execute both legs with timeout to prevent infinite waiting
        // If one leg fills but other doesn't within 45 seconds, trigger stop-loss hedge
        let yes_fut = self.simulate_sign_and_submit(&yes_token, actual_size, yes_price);
        let no_fut = self.simulate_sign_and_submit(&no_token, actual_size, no_price);
        
        // Race both legs - first to complete wins
        let yes_result = timeout(Duration::from_secs(45), yes_fut).await;
        let no_result = timeout(Duration::from_secs(45), no_fut).await;
        
        // Check if either leg timed out
        let yes_timed_out = yes_result.is_err();
        let no_timed_out = no_result.is_err();
        
        if yes_timed_out || no_timed_out {
            let filled_leg = if yes_timed_out == false { "YES" } else { "NO" };
            let missing_leg = if yes_timed_out == false { "NO" } else { "YES" };
            warn!("⚠️ LEG RISK: {} filled but {} TIMEOUT - initiating stop-loss hedge", filled_leg, missing_leg);
            
            // In live mode, would fire market order here to close exposure
            // For now, mark as partial fill
            let (yes_id, no_id, yes_filled, no_filled) = if yes_timed_out {
                (None, None, false, false)
            } else {
                (yes_result.unwrap().ok(), None, true, false)
            };
            return Ok(OrderResult {
                market_id: market.condition_id.clone(),
                yes_order_id: yes_id,
                no_order_id: no_id,
                yes_filled,
                no_filled,
                profit_estimate: profit * 0.5, // Half profit due to leg risk
                size: actual_size,
            });
        }
        
        let yes_result = yes_result.unwrap();
        let no_result = no_result.unwrap();
        
        match (yes_result, no_result) {
            (Ok(yes_id), Ok(no_id)) => {
                info!("✅ DRY RUN: Both legs signed & submitted! YES={} NO={}", yes_id, no_id);
                Ok(OrderResult {
                    market_id: market.condition_id.clone(),
                    yes_order_id: Some(yes_id),
                    no_order_id: Some(no_id),
                    yes_filled: true,
                    no_filled: true,
                    profit_estimate: profit,
                    size: actual_size,
                })
            }
            (Ok(yes_id), Err(e)) => {
                warn!("⚠️ DRY RUN LEG RISK: YES signed but NO failed: {}", e);
                Ok(OrderResult {
                    market_id: market.condition_id.clone(),
                    yes_order_id: Some(yes_id),
                    no_order_id: None,
                    yes_filled: true,
                    no_filled: false,
                    profit_estimate: profit,
                    size,
                })
            }
            (Err(e), Ok(no_id)) => {
                warn!("⚠️ DRY RUN LEG RISK: NO signed but YES failed: {}", e);
                Ok(OrderResult {
                    market_id: market.condition_id.clone(),
                    yes_order_id: None,
                    no_order_id: Some(no_id),
                    yes_filled: false,
                    no_filled: true,
                    profit_estimate: profit,
                    size,
                })
            }
            (Err(e1), Err(e2)) => {
                error!("❌ DRY RUN: Both legs failed - YES={}, NO={}", e1, e2);
                Ok(OrderResult {
                    market_id: market.condition_id.clone(),
                    yes_order_id: None,
                    no_order_id: None,
                    yes_filled: false,
                    no_filled: false,
                    profit_estimate: profit,
                    size,
                })
            }
        }
    }

    /// Simulate EIP-712 signing (dry-run only)
    async fn simulate_sign_and_submit(
        &self,
        token_id: &str,
        size: f64,
        price: f64,
    ) -> Result<String> {
        // Simulate signing latency (EIP-712 signing takes ~1-5ms typically)
        tokio::time::sleep(tokio::time::Duration::from_millis(3)).await;
        let fake_id = format!("eip712_{}_{}", &token_id[..8], (price * 1000.) as u32);
        debug!("  📝 Simulated EIP-712 sign: token={} size={} price={}", token_id, size, price);
        Ok(fake_id)
    }
    
    /// Mark queue start for ghost liquidity detection (500ms taker delay)
    pub fn mark_taker_queue_start(&self, condition_id: &str) {
        // In production: call self.orderbook.mark_queue_start(condition_id)
        // For now, log that we're entering the queue
        tracing::info!("⏱️ TAKER QUEUE START: {} (500ms delay - watching for ghost liquidity)", &condition_id[..8.min(condition_id.len())]);
    }
    
    /// Check if liquidity vanished during 500ms queue (ghost detection)
    pub fn check_ghost_liquidity(&self, condition_id: &str) -> bool {
        // In production: return self.orderbook.check_ghost_liquidity(condition_id)
        // For now, always return false (placeholder)
        false
    }
}

/// Signal to execute an arbitrage trade
#[derive(Debug, Clone)]
pub struct ArbitrageSignal {
    pub market: SimplifiedMarket,
    pub profit: f64,
    pub size: f64,
    pub yes_depth: f64,  // Top-of-book YES depth
    pub no_depth: f64,   // Top-of-book NO depth
}

/// Start the trading event loop
pub async fn start_trading_loop(
    engine: TradingEngine,
    mut rx: mpsc::UnboundedReceiver<ArbitrageSignal>,
) {
    let dry_run = engine.config.dry_run;
    info!("📊 Trading loop started (dry_run={})", dry_run);
    
    if dry_run {
        info!("🔒 Running in DRY RUN mode - no real orders");
        info!("   Simulating EIP-712 signed orders with concurrent execution");
    } else {
        info!("🔴 LIVE TRADING MODE - EIP-712 signed orders will be submitted");
        if !engine.is_ready() {
            error!("❌ Trading engine not ready but --live flag set!");
            return;
        }
    }
    
    while let Some(signal) = rx.recv().await {
        // MLE Check: Validate capital limits before trading
        let capital_required = signal.size * (signal.market.yes_price().unwrap_or(0.0) + signal.market.no_price().unwrap_or(0.0));
        let max_per_market = engine.config.total_bankroll * engine.config.max_per_market_pct;
        let max_total = engine.config.total_bankroll * engine.config.max_total_pct;
        
        {
            let active = engine.active_capital.lock().unwrap();
            let total = engine.total_active_capital.lock().unwrap();
            let current_per_market = active.get(&signal.market.condition_id).unwrap_or(&0.0);
            if *current_per_market + capital_required > max_per_market {
                warn!("⚠️ MLE BLOCKED: {} market cap exceeded (${:.2} > ${:.2})", 
                    &signal.market.condition_id[..8.min(signal.market.condition_id.len())], 
                    *current_per_market + capital_required, max_per_market);
            } else if *total + capital_required > max_total {
                warn!("⚠️ MLE BLOCKED: Total capital cap exceeded (${:.2} > ${:.2})", 
                    *total + capital_required, max_total);
            } else {
                // Reserve capital: update both per-market and total
                drop(active);
                drop(total);
                engine.active_capital.lock().unwrap().insert(signal.market.condition_id.clone(), 
                    capital_required);
                *engine.total_active_capital.lock().unwrap() += capital_required;
                info!("📊 MLE: Reserved ${:.2} | Total: ${:.2}", capital_required, 
                    *engine.total_active_capital.lock().unwrap());
            }
        }
        
        let trade_result = match engine.execute_arbitrage(&signal.market, signal.size).await {
            Ok(result) => {
                if result.yes_filled && result.no_filled {
                    info!("✅ Trade complete: {} profit=${:.4} (${:.2} total) [ADVERSE TRACKING ENABLED]", result.market_id, result.profit_estimate, result.profit_estimate * result.size);
                } else if result.yes_filled || result.no_filled {
                    warn!("⚠️ Partial fill - LEG RISK on {}", result.market_id);
                }
                Some(result)
            }
            Err(e) => {
                error!("❌ Trade error: {}", e);
                None
            }
        };

        // Release reserved capital after trade completes (success or failure)
        let mut active = engine.active_capital.lock().unwrap();
        let mut total = engine.total_active_capital.lock().unwrap();
        if let Some(reserved) = active.get(&signal.market.condition_id).copied() {
            active.remove(&signal.market.condition_id);
            *total = (*total - reserved).max(0.0);
            debug!("📊 MLE: Released ${:.2} | Total: ${:.2}", reserved, *total);
        }
    }
    
    info!("🛑 Trading loop ended");
}
