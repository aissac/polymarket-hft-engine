//! Pingpong Production Module
//! 
//! Production-grade features for live trading:
//! 1. Liquidity checks - verify orderbook depth before trading
//! 2. Gas optimization - estimate and batch transactions
//! 3. Gnosis Safe - gasless meta-transactions (Signature Type 2)
//! 4. Circuit breakers - rate limiting and risk management
//! 5. Order expiration - TTL for posted orders

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tracing::{info, warn, debug};

/// Orderbook level for depth checking
#[derive(Debug, Clone)]
pub struct OrderBookLevel {
    pub price: f64,
    pub size: f64,
}

/// Liquidity requirements for a trade
#[derive(Debug, Clone)]
pub struct LiquidityCheck {
    /// Minimum orderbook depth at best bid/ask
    pub min_depth: f64,
    /// Maximum slippage tolerance (e.g., 0.01 = 1%)
    pub max_slippage: f64,
    /// Maximum slippage in absolute dollars
    pub max_slippage_usdc: f64,
}

impl Default for LiquidityCheck {
    fn default() -> Self {
        Self {
            min_depth: 100.0,         // At least 100 shares at target price
            max_slippage: 0.05,       // 5% max slippage
            max_slippage_usdc: 1.0,   // $1 max slippage in dollar terms
        }
    }
}

/// Circuit breaker configuration
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Maximum trades per minute
    pub max_trades_per_minute: u32,
    /// Maximum concurrent trades
    pub max_concurrent_trades: u32,
    /// Maximum daily loss before stopping
    pub max_daily_loss_usdc: f64,
    /// Maximum position size per trade
    pub max_position_size: f64,
    /// Cooldown period after circuit break (seconds)
    pub cooldown_seconds: u64,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self {
            max_trades_per_minute: 20,
            max_concurrent_trades: 5,
            max_daily_loss_usdc: 100.0,
            max_position_size: 150.0,
            cooldown_seconds: 60,
        }
    }
}

/// Gas estimation
#[derive(Debug, Clone)]
pub struct GasEstimate {
    /// Estimated gas units
    pub gas_units: u64,
    /// Gas price in gwei
    pub gas_price_gwei: f64,
    /// Estimated cost in USDC
    pub cost_usdc: f64,
}

impl Default for GasEstimate {
    fn default() -> Self {
        // Default Polygon gas estimates
        Self {
            gas_units: 200_000,        // Order placement
            gas_price_gwei: 30.0,     // ~30 gwei typical
            cost_usdc: 0.01,           // ~$0.01 at 30 gwei, 200k gas, MATIC=$0.80
        }
    }
}

/// Gnosis Safe configuration for gasless trading
#[derive(Debug, Clone)]
pub struct GnosisSafeConfig {
    /// Whether to use Gnosis Safe for gasless transactions
    pub enabled: bool,
    /// Safe contract address
    pub safe_address: Option<String>,
    /// Whether to use EIP-1271 signature
    pub use_eip_1271: bool,
}

impl Default for GnosisSafeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            safe_address: None,
            use_eip_1271: true,
        }
    }
}

/// Production trading guard - wraps trading with all safety checks
pub struct ProductionGuard {
    circuit_breaker: CircuitBreaker,
    liquidity_check: LiquidityCheck,
    gas_estimate: GasEstimate,
    gnosis_safe: GnosisSafeConfig,
    
    // Runtime state
    trades_last_minute: Arc<RwLock<Vec<Instant>>>,
    concurrent_trades: Arc<RwLock<u32>>,
    daily_loss: Arc<RwLock<f64>>,
    last_circuit_break: Arc<RwLock<Option<Instant>>>,
    total_trades: Arc<RwLock<u64>>,
    profitable_trades: Arc<RwLock<u64>>,
}

impl ProductionGuard {
    pub fn new() -> Self {
        Self {
            circuit_breaker: CircuitBreaker::default(),
            liquidity_check: LiquidityCheck::default(),
            gas_estimate: GasEstimate::default(),
            gnosis_safe: GnosisSafeConfig::default(),
            trades_last_minute: Arc::new(RwLock::new(Vec::new())),
            concurrent_trades: Arc::new(RwLock::new(0)),
            daily_loss: Arc::new(RwLock::new(0.0)),
            last_circuit_break: Arc::new(RwLock::new(None)),
            total_trades: Arc::new(RwLock::new(0)),
            profitable_trades: Arc::new(RwLock::new(0)),
        }
    }
    
    /// Configure circuit breaker
    pub fn with_circuit_breaker(mut self, cb: CircuitBreaker) -> Self {
        self.circuit_breaker = cb;
        self
    }
    
    /// Configure liquidity check
    pub fn with_liquidity_check(mut self, lc: LiquidityCheck) -> Self {
        self.liquidity_check = lc;
        self
    }
    
    /// Configure Gnosis Safe for gasless trading
    pub fn with_gnosis_safe(mut self, config: GnosisSafeConfig) -> Self {
        self.gnosis_safe = config;
        self
    }
    
    /// Check if trading is allowed (all circuit breakers pass)
    pub fn can_trade(&self) -> Result<String, String> {
        let now = Instant::now();
        
        // Check circuit break cooldown
        if let Some(last_break) = *self.last_circuit_break.read() {
            let elapsed = now.duration_since(last_break);
            if elapsed < Duration::from_secs(self.circuit_breaker.cooldown_seconds) {
                let remaining = self.circuit_breaker.cooldown_seconds - elapsed.as_secs();
                return Err(format!("Circuit break active, {}s remaining", remaining));
            }
        }
        
        // Check trades per minute
        {
            let mut trades = self.trades_last_minute.write();
            let one_min_ago = now - Duration::from_secs(60);
            trades.retain(|t| *t > one_min_ago);
            if trades.len() >= self.circuit_breaker.max_trades_per_minute as usize {
                return Err(format!("Rate limit: {} trades/min exceeded", self.circuit_breaker.max_trades_per_minute));
            }
        }
        
        // Check concurrent trades
        if *self.concurrent_trades.read() >= self.circuit_breaker.max_concurrent_trades {
            return Err("Max concurrent trades reached".to_string());
        }
        
        // Check daily loss
        if *self.daily_loss.read() >= self.circuit_breaker.max_daily_loss_usdc {
            return Err("Daily loss limit reached".to_string());
        }
        
        Ok("OK".to_string())
    }
    
    /// Record a new trade
    pub fn record_trade(&self, profit: f64) {
        // Record timestamp
        self.trades_last_minute.write().push(Instant::now());
        
        // Update concurrent count
        *self.concurrent_trades.write() += 1;
        
        // Update daily loss if negative
        if profit < 0.0 {
            *self.daily_loss.write() += profit.abs();
        }
        
        // Update totals
        *self.total_trades.write() += 1;
        if profit > 0.0 {
            *self.profitable_trades.write() += 1;
        }
        
        debug!("Trade recorded: profit=${:.4}, concurrent={}, 1min_window={}", 
            profit,
            *self.concurrent_trades.read(),
            self.trades_last_minute.read().len()
        );
    }
    
    /// Release a concurrent trade slot
    pub fn release_trade(&self) {
        *self.concurrent_trades.write() = self.concurrent_trades.read().saturating_sub(1);
    }
    
    /// Trigger circuit break
    pub fn circuit_break(&self, reason: &str) {
        warn!("⚠️ CIRCUIT BREAK: {}", reason);
        *self.last_circuit_break.write() = Some(Instant::now());
    }
    
    /// Check if orderbook has sufficient liquidity
    pub fn check_liquidity(
        &self,
        best_bid: f64,
        best_ask: f64,
        bid_depth: f64,  // Total size at best bid
        ask_depth: f64,  // Total size at best ask
        target_price: f64,
        target_size: f64,
    ) -> Result<LiquidityCheck, String> {
        // Check minimum depth at target price
        let depth_at_price = if target_price < best_ask {
            ask_depth
        } else {
            bid_depth
        };
        
        if depth_at_price < self.liquidity_check.min_depth {
            return Err(format!("Insufficient liquidity: {:.0} < {:.0} required", 
                depth_at_price, self.liquidity_check.min_depth));
        }
        
        // Check slippage
        let slippage = if target_price < best_ask {
            best_ask - target_price
        } else {
            target_price - best_bid
        };
        
        let slippage_pct = slippage / target_price;
        if slippage_pct > self.liquidity_check.max_slippage {
            return Err(format!("Slippage too high: {:.2}% > {:.2}% max", 
                slippage_pct * 100.0, self.liquidity_check.max_slippage * 100.0));
        }
        
        let slippage_usdc = slippage * target_size;
        if slippage_usdc > self.liquidity_check.max_slippage_usdc {
            return Err(format!("Slippage cost too high: ${:.2} > ${:.2} max", 
                slippage_usdc, self.liquidity_check.max_slippage_usdc));
        }
        
        Ok(self.liquidity_check.clone())
    }
    
    /// Estimate gas cost for a trade
    pub fn estimate_gas(&self, trade_type: TradeType) -> GasEstimate {
        match trade_type {
            TradeType::MakerPost => GasEstimate {
                gas_units: 150_000,    // Cancel + post
                gas_price_gwei: self.gas_estimate.gas_price_gwei,
                cost_usdc: 0.008,
            },
            TradeType::TakerFill => GasEstimate {
                gas_units: 200_000,    // Direct fill
                gas_price_gwei: self.gas_estimate.gas_price_gwei,
                cost_usdc: 0.012,
            },
            TradeType::CancelReplace => GasEstimate {
                gas_units: 100_000,    // Just cancel
                gas_price_gwei: self.gas_estimate.gas_price_gwei,
                cost_usdc: 0.005,
            },
        }
    }
    
    /// Check if trade is profitable after gas
    pub fn is_profitable_after_gas(&self, profit_per_share: f64, size: f64, trade_type: TradeType) -> bool {
        let gross_profit = profit_per_share * size;
        let gas = self.estimate_gas(trade_type);
        let net_profit = gross_profit - gas.cost_usdc;
        
        debug!("Profit check: gross=${:.2}, gas=${:.4}, net=${:.2}", 
            gross_profit, gas.cost_usdc, net_profit);
        
        net_profit > 0.01 // At least $0.01 profit after gas
    }
    
    /// Get signature type for Gnosis Safe (EIP-1271)
    pub fn get_signature_type(&self) -> u8 {
        if self.gnosis_safe.enabled && self.gnosis_safe.use_eip_1271 {
            // EIP-1271 signature type for gnosis safe
            // This enables gasless meta-transactions
            2  // EthSign signature type
        } else {
            0  // Standard EIP-712
        }
    }
    
    /// Get trading stats
    pub fn stats(&self) -> TradingStats {
        TradingStats {
            total_trades: *self.total_trades.read(),
            profitable_trades: *self.profitable_trades.read(),
            win_rate: if *self.total_trades.read() > 0 {
                (*self.profitable_trades.read() as f64 / *self.total_trades.read() as f64) * 100.0
            } else {
                0.0
            },
            concurrent_trades: *self.concurrent_trades.read(),
            trades_last_minute: self.trades_last_minute.read().len() as u32,
            daily_loss_usdc: *self.daily_loss.read(),
            circuit_break_active: self.last_circuit_break.read().is_some(),
        }
    }
}

impl Default for ProductionGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Type of trade for gas estimation
#[derive(Debug, Clone, Copy)]
pub enum TradeType {
    /// Post maker orders
    MakerPost,
    /// Take existing liquidity
    TakerFill,
    /// Cancel and replace
    CancelReplace,
}

/// Trading statistics
#[derive(Debug, Clone)]
pub struct TradingStats {
    pub total_trades: u64,
    pub profitable_trades: u64,
    pub win_rate: f64,
    pub concurrent_trades: u32,
    pub trades_last_minute: u32,
    pub daily_loss_usdc: f64,
    pub circuit_break_active: bool,
}

/// Order with expiration for maker orders
#[derive(Debug, Clone)]
pub struct ExpiringOrder {
    pub order_id: String,
    pub created_at: Instant,
    pub ttl_seconds: u64,
}

impl ExpiringOrder {
    pub fn new(order_id: String, ttl_seconds: u64) -> Self {
        Self {
            order_id,
            created_at: Instant::now(),
            ttl_seconds,
        }
    }
    
    pub fn is_expired(&self) -> bool {
        Instant::now().duration_since(self.created_at) > Duration::from_secs(self.ttl_seconds)
    }
}

/// Order manager with expiration tracking
pub struct OrderManager {
    orders: Arc<RwLock<HashMap<String, ExpiringOrder>>>,
    default_ttl_seconds: u64,
}

impl OrderManager {
    pub fn new(default_ttl_seconds: u64) -> Self {
        Self {
            orders: Arc::new(RwLock::new(HashMap::new())),
            default_ttl_seconds,
        }
    }
    
    pub fn add_order(&self, order_id: String) {
        self.orders.write().insert(
            order_id.clone(),
            ExpiringOrder::new(order_id, self.default_ttl_seconds),
        );
    }
    
    pub fn remove_order(&self, order_id: &str) {
        self.orders.write().remove(order_id);
    }
    
    pub fn cleanup_expired(&self) -> Vec<String> {
        let mut orders = self.orders.write();
        let expired: Vec<String> = orders.iter()
            .filter(|(_, o)| o.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        
        for id in &expired {
            orders.remove(id);
        }
        
        expired
    }
    
    pub fn active_order_count(&self) -> usize {
        self.orders.read().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_circuit_breaker() {
        let guard = ProductionGuard::new();
        
        // Should allow trade
        assert!(guard.can_trade().is_ok());
        
        // Record many trades
        for _ in 0..25 {
            guard.record_trade(1.0);
        }
        
        // Should be rate limited
        assert!(guard.can_trade().is_err());
    }
    
    #[test]
    fn test_liquidity_check() {
        let guard = ProductionGuard::new();
        
        // Sufficient liquidity
        let result = guard.check_liquidity(
            0.95,   // best_bid
            0.96,   // best_ask
            500.0,  // bid_depth
            500.0,  // ask_depth
            0.96,   // target_price
            100.0,  // target_size
        );
        assert!(result.is_ok());
        
        // Insufficient depth
        let result = guard.check_liquidity(
            0.95,
            0.96,
            50.0,   // low depth
            50.0,
            0.96,
            100.0,
        );
        assert!(result.is_err());
    }
    
    #[test]
    fn test_order_expiration() {
        let order = ExpiringOrder::new("test123".to_string(), 1);
        assert!(!order.is_expired());
        
        // Manually check after TTL
        std::thread::sleep(Duration::from_secs(2));
        assert!(order.is_expired());
    }
}
