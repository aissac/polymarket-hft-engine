//! OrderBook Tracker for Pingpong Strategy - Phase 2: DashMap for Ghost Detection
//! 
//! Thread-safe orderbook tracker using DashMap for concurrent access.
//! Enables real-time ghost liquidity detection during 500ms taker queue.

use dashmap::DashMap;
use std::sync::Arc;

/// Market price data with depth tracking
#[derive(Debug, Clone)]
pub struct MarketPrices {
    pub condition_id: String,
    pub yes_best_bid: Option<f64>,
    pub yes_best_ask: Option<f64>,
    pub no_best_bid: Option<f64>,
    pub no_best_ask: Option<f64>,
    pub yes_depth: f64,  // Size at best ask
    pub no_depth: f64,   // Size at best ask
    pub last_update: i64,
    /// Track depth changes during 500ms queue (ghost detection)
    pub queue_start_yes_depth: Option<f64>,
    pub queue_start_no_depth: Option<f64>,
}

impl MarketPrices {
    pub fn new(condition_id: String) -> Self {
        Self {
            condition_id,
            yes_best_bid: None,
            yes_best_ask: None,
            no_best_bid: None,
            no_best_ask: None,
            yes_depth: 0.0,
            no_depth: 0.0,
            last_update: 0,
            queue_start_yes_depth: None,
            queue_start_no_depth: None,
        }
    }

    pub fn combined_cost(&self) -> Option<f64> {
        match (self.yes_best_ask, self.no_best_ask) {
            (Some(yes), Some(no)) => Some(yes + no),
            _ => None,
        }
    }
    
    pub fn has_arbitrage(&self, target: f64) -> bool {
        self.combined_cost()
            .map(|c| c < target)
            .unwrap_or(false)
    }

    /// Check if liquidity vanished during queue (ghost detection)
    /// Check if liquidity vanished during queue, accounting for own fills
    /// True_Ghost = Initial_Depth - Final_Depth - Our_Filled_Shares
    pub fn liquidity_vanished_during_queue_with_fill(&self, filled_size: f64) -> bool {
        match (self.queue_start_yes_depth, self.queue_start_no_depth) {
            (Some(start_yes), Some(start_no)) => {
                // Calculate true ghost volume: what vanished beyond our own fill
                // We filled 'filled_size' shares, so expect depth to drop by at least that much
                // If depth dropped MORE than filled_size, that's true ghost liquidity
                
                // For YES side: expect depth to drop by filled_size (we bought YES)
                let yes_ghost_volume = start_yes - self.yes_depth - filled_size;
                // For NO side: expect depth to drop by filled_size (we bought NO)
                let no_ghost_volume = start_no - self.no_depth - filled_size;
                
                // Ghost detected if maker pulled MORE than our fill
                // Threshold: >50% of original depth vanished beyond our fill
                let yes_ghost = yes_ghost_volume > start_yes * 0.5;
                let no_ghost = no_ghost_volume > start_no * 0.5;
                let ghost_detected = yes_ghost || no_ghost;
                
                // Always log the comparison for debugging
                tracing::info!(
                    "🔍 GHOST CHECK: {} | YES: {:.2} -> {:.2} (filled {:.0}, ghost {:.2}) | NO: {:.2} -> {:.2} (filled {:.0}, ghost {:.2}) | ghost={}",
                    &self.condition_id[..8.min(self.condition_id.len())],
                    start_yes, self.yes_depth, filled_size, yes_ghost_volume,
                    start_no, self.no_depth, filled_size, no_ghost_volume,
                    ghost_detected
                );
                
                if ghost_detected {
                    tracing::warn!(
                        "👻 GHOST DETECTED: {} | YES ghost: {:.2} | NO ghost: {:.2} | filled: {:.0}",
                        &self.condition_id[..8.min(self.condition_id.len())],
                        yes_ghost_volume, no_ghost_volume, filled_size
                    );
                }
                
                ghost_detected
            }
            _ => false
        }
    }
    
    /// Legacy method for backward compatibility
    pub fn liquidity_vanished_during_queue(&self) -> bool {
        match (self.queue_start_yes_depth, self.queue_start_no_depth) {
            (Some(start_yes), Some(start_no)) => {
                // If depth dropped by >50%, liquidity vanished (maker pulled quote)
                let yes_vanished = self.yes_depth < start_yes * 0.5;
                let no_vanished = self.no_depth < start_no * 0.5;
                let ghost_detected = yes_vanished || no_vanished;
                
                // Always log the comparison for debugging
                tracing::info!(
                    "🔍 GHOST CHECK: {} | YES: {:.2} vs {:.2} ({}%) | NO: {:.2} vs {:.2} ({}%) | ghost={}",
                    &self.condition_id[..8.min(self.condition_id.len())],
                    start_yes, self.yes_depth,
                    if start_yes > 0.0 { ((self.yes_depth / start_yes) * 100.0) as i32 } else { 0 },
                    start_no, self.no_depth,
                    if start_no > 0.0 { ((self.no_depth / start_no) * 100.0) as i32 } else { 0 },
                    ghost_detected
                );
                
                if ghost_detected {
                    tracing::warn!(
                        "👻 GHOST DETECTED: {} | YES: {:.2} -> {:.2} | NO: {:.2} -> {:.2}",
                        &self.condition_id[..8.min(self.condition_id.len())],
                        start_yes, self.yes_depth,
                        start_no, self.no_depth
                    );
                }
                
                ghost_detected
            }
            _ => false,
        }
    }

    /// Record depth at start of 500ms queue
    pub fn mark_queue_start(&mut self) {
        self.queue_start_yes_depth = Some(self.yes_depth);
        self.queue_start_no_depth = Some(self.no_depth);
        tracing::info!(
            "📊 QUEUE START: {} | YES depth: {:.2} -> saved | NO depth: {:.2} -> saved",
            &self.condition_id[..8.min(self.condition_id.len())],
            self.yes_depth,
            self.no_depth
        );
    }

    /// Clear queue markers after execution
    pub fn clear_queue_markers(&mut self) {
        self.queue_start_yes_depth = None;
        self.queue_start_no_depth = None;
    }
}

/// Thread-safe orderbook tracker using DashMap (lock-free concurrent access)
pub struct OrderBookTracker {
    markets: Arc<DashMap<String, MarketPrices>>,
}

impl OrderBookTracker {
    pub fn new() -> Self {
        Self {
            markets: Arc::new(DashMap::new()),
        }
    }
    
    /// Initialize a market with depths from ARB detection (before WebSocket updates arrive)
    pub fn init_market(&self, condition_id: &str, yes_depth: f64, no_depth: f64) {
        let mut market = self.markets
            .entry(condition_id.to_string())
            .or_insert_with(|| MarketPrices::new(condition_id.to_string()));
        market.yes_depth = yes_depth;
        market.no_depth = no_depth;
    }
    
    /// Update prices for a market
    /// 
    /// IMPORTANT: Only updates the depth for the side that changed.
    /// WebSocket sends separate updates for YES and NO sides.
    /// We must preserve the other side's depth to avoid overwriting to 0.
    pub fn update(&self, condition_id: &str, yes_ask: Option<f64>, no_ask: Option<f64>, yes_depth: f64, no_depth: f64) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        
        let mut market = self.markets
            .entry(condition_id.to_string())
            .or_insert_with(|| MarketPrices::new(condition_id.to_string()));
        
        // Update prices if provided
        if yes_ask.is_some() {
            market.yes_best_ask = yes_ask;
        }
        if no_ask.is_some() {
            market.no_best_ask = no_ask;
        }
        
        // CRITICAL: Only update depth for the side that has valid data
        // WebSocket sends separate updates for YES and NO sides
        // If yes_depth > 0, this is a YES update - preserve NO depth
        // If no_depth > 0, this is a NO update - preserve YES depth
        if yes_depth > 0.0 {
            market.yes_depth = yes_depth;
            // Keep existing no_depth (don't overwrite to 0)
        }
        if no_depth > 0.0 {
            market.no_depth = no_depth;
            // Keep existing yes_depth (don't overwrite to 0)
        }
        
        market.last_update = timestamp;
        
        // Log accumulated depths
        tracing::debug!(
            "📈 TRACKER UPDATE: {} | YES={:.2} NO={:.2} (after: YES={:.2} NO={:.2})",
            &condition_id[..8.min(condition_id.len())],
            yes_depth, no_depth,
            market.yes_depth, market.no_depth
        );
    }

    /// Get market prices (read-only, lock-free)
    pub fn get(&self, condition_id: &str) -> Option<MarketPrices> {
        self.markets.get(condition_id).map(|r| r.clone())
    }

    /// Mark queue start for ghost detection
    pub fn mark_queue_start(&self, condition_id: &str) {
        // First check if market exists and log its current depths
        if let Some(existing) = self.markets.get(condition_id) {
            tracing::info!(
                "📊 BEFORE QUEUE START: {} | existing YES={:.2} NO={:.2}",
                &condition_id[..8.min(condition_id.len())],
                existing.yes_depth, existing.no_depth
            );
        } else {
            tracing::warn!("📊 QUEUE START: {} | NO EXISTING ENTRY", &condition_id[..8.min(condition_id.len())]);
        }
        
        // RACE CONDITION FIX: Create market entry if it doesn't exist
        // ARB detection can happen before the first WebSocket update arrives
        let mut market = self.markets
            .entry(condition_id.to_string())
            .or_insert_with(|| MarketPrices::new(condition_id.to_string()));
        
        market.mark_queue_start();
    }

    /// Check if liquidity vanished during queue
    pub fn check_ghost_liquidity(&self, condition_id: &str) -> bool {
        self.markets
            .get(condition_id)
            .map(|r| r.liquidity_vanished_during_queue())
            .unwrap_or(false)
    }
    
    /// Check if liquidity vanished during queue, accounting for own fills
    pub fn check_ghost_liquidity_with_fill(&self, condition_id: &str, filled_size: f64) -> bool {
        self.markets
            .get(condition_id)
            .map(|r| r.liquidity_vanished_during_queue_with_fill(filled_size))
            .unwrap_or(false)
    }

    /// Clear queue markers after execution
    pub fn clear_queue_markers(&self, condition_id: &str) {
        if let Some(mut market) = self.markets.get_mut(condition_id) {
            market.clear_queue_markers();
        }
    }

    /// Get all markets (for reporting)
    pub fn get_all_markets(&self) -> Vec<MarketPrices> {
        self.markets.iter().map(|r| r.clone()).collect()
    }

    /// Count active markets
    pub fn market_count(&self) -> usize {
        self.markets.len()
    }
    
    /// Get best asks for a market (for adverse selection tracking)
    pub fn get_best_asks(&self, condition_id: &str) -> (f64, f64) {
        match self.markets.get(condition_id) {
            Some(market) => (market.yes_best_ask.unwrap_or(0.0), market.no_best_ask.unwrap_or(0.0)),
            None => (0.0, 0.0)
        }
    }
}

impl Default for OrderBookTracker {
    fn default() -> Self {
        Self::new()
    }
}
