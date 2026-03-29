//! Ghost liquidity simulator - measures real competition by simulating order execution
//! After detecting an opportunity, wait 50ms then check if liquidity still exists

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ghost simulation result
#[derive(Debug, Clone)]
pub enum GhostResult {
    Ghosted,    // Depth went to 0 (liquidity vanished)
    Executable, // Depth remained (real opportunity)
    Partial,    // Some depth remained (partial fill possible)
}

/// Track pending ghost simulations
#[derive(Debug, Clone)]
pub struct PendingGhost {
    pub condition_id: String,
    pub side: String,
    pub price: f64,
    pub initial_depth: f64,
    pub order_size: f64,
    pub detected_at: Instant,
}

/// Ghost simulator state
pub struct GhostSimulator {
    pending: Mutex<HashMap<String, PendingGhost>>,
    stats: Mutex<GhostStats>,
}

#[derive(Debug, Clone, Default)]
pub struct GhostStats {
    pub ghosted: u64,
    pub executable: u64,
    pub partial: u64,
}

impl GhostSimulator {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            stats: Mutex::new(GhostStats::default()),
        }
    }

    /// Record an opportunity for ghost simulation
    pub fn track_opportunity(
        &self,
        condition_id: &str,
        side: &str,
        price: f64,
        depth: f64,
        order_size: f64,
    ) {
        let key = format!("{}-{}", condition_id, side);
        let pending = PendingGhost {
            condition_id: condition_id.to_string(),
            side: side.to_string(),
            price,
            initial_depth: depth,
            order_size,
            detected_at: Instant::now(),
        };
        
        let mut p = self.pending.lock().unwrap();
        p.insert(key, pending);
    }

    /// Check if a pending simulation is ready (50ms RTT simulation)
    pub fn check_ghost(&self, condition_id: &str, side: &str, current_depth: f64) -> Option<GhostResult> {
        let key = format!("{}-{}", condition_id, side);
        let mut p = self.pending.lock().unwrap();
        
        if let Some(pending) = p.remove(&key) {
            // Simulate 50ms network RTT
            let elapsed = pending.detected_at.elapsed();
            if elapsed < Duration::from_millis(50) {
                // Not enough time passed, put it back
                p.insert(key, pending);
                return None;
            }
            
            let result = if current_depth == 0.0 {
                GhostResult::Ghosted
            } else if current_depth >= pending.order_size {
                GhostResult::Executable
            } else {
                GhostResult::Partial
            };
            
            // Update stats
            let mut stats = self.stats.lock().unwrap();
            match &result {
                GhostResult::Ghosted => stats.ghosted += 1,
                GhostResult::Executable => stats.executable += 1,
                GhostResult::Partial => stats.partial += 1,
            }
            
            Some(result)
        } else {
            None
        }
    }

    /// Get current ghost statistics
    pub fn get_stats(&self) -> GhostStats {
        self.stats.lock().unwrap().clone()
    }

    /// Log ghost result
    pub fn log_result(&self, result: &GhostResult, condition_id: &str, side: &str, price: f64, initial_depth: f64, final_depth: f64, order_size: f64) {
        match result {
            GhostResult::Ghosted => {
                tracing::info!(
                    "👻 GHOSTED: {} | {} @ ${:.4} | Depth: {:.0} -> 0 | Order: {:.0}",
                    &condition_id[..8.min(condition_id.len())],
                    side,
                    price,
                    initial_depth,
                    order_size
                );
            }
            GhostResult::Executable => {
                tracing::info!(
                    "✅ EXECUTABLE: {} | {} @ ${:.4} | Depth: {:.0} -> {:.0} | Order: {:.0}",
                    &condition_id[..8.min(condition_id.len())],
                    side,
                    price,
                    initial_depth,
                    final_depth,
                    order_size
                );
            }
            GhostResult::Partial => {
                tracing::info!(
                    "⚠️ PARTIAL: {} | {} @ ${:.4} | Depth: {:.0} -> {:.0} | Order: {:.0}",
                    &condition_id[..8.min(condition_id.len())],
                    side,
                    price,
                    initial_depth,
                    final_depth,
                    order_size
                );
            }
        }
    }
}