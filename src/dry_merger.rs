use tracing::{info, debug};
/// Dry-run merger: simulates capital recycling without API calls
pub struct DryMerger {
    positions: std::collections::HashMap<String, (u64, u64)>,
}

impl DryMerger {
    pub fn new() -> Self {
        Self {
            positions: std::collections::HashMap::new(),
        }
    }

    /// Record a trade's shares for later merging
    pub fn record_trade(&mut self, condition_id: String, yes_shares: u64, no_shares: u64) {
        let entry = self.positions.entry(condition_id).or_insert((0, 0));
        entry.0 += yes_shares;
        entry.1 += no_shares;
        info!("📝 DRY MERGER: Recorded {} YES={} NO={}", 
              &entry.0.to_string()[..8.min(8)], yes_shares, no_shares);
    }

    /// Get mergeable positions and clear them (simulating merge)
    pub fn get_mergeable(&mut self) -> Vec<(String, u64, f64)> {
        let mut result = Vec::new();
        let keys: Vec<_> = self.positions.keys().cloned().collect();
        
        for cid in keys {
            if let Some((yes, no)) = self.positions.get(&cid) {
                let merged = (*yes).min(*no);
                if merged > 0 {
                    let released = merged as f64 * 0.70;
                    result.push((cid.clone(), merged, released));
                    // Keep the larger side
                    if *yes > *no {
                        self.positions.insert(cid.clone(), (yes - merged, 0));
                    } else {
                        self.positions.insert(cid.clone(), (0, no - merged));
                    }
                }
            }
        }
        result
    }

    /// Run the dry merger loop - fires every 30s
    pub async fn run_loop(mut self) {
        use std::time::Duration;
        use tokio::time::interval;
        
        const MERGE_INTERVAL_SECS: u64 = 30;
        info!("🔄 DRY MERGER: Starting (simulates capital recycling every {}s)", MERGE_INTERVAL_SECS);
        
        let mut timer = interval(Duration::from_secs(MERGE_INTERVAL_SECS));
        
        loop {
            timer.tick().await;
            
            let mergeable = self.get_mergeable();
            if mergeable.is_empty() {
                tracing::debug!("DRY MERGER: No positions to merge");
                continue;
            }
            
            let total_released: f64 = mergeable.iter().map(|(_, _, r)| r).sum();
            
            for (cid, shares, released) in mergeable {
                info!("🔄 DRY MERGE | {} | {} shares (~${:.2})", 
                      &cid[..8.min(cid.len())], shares, released);
            }
            
            info!("💰 DRY MERGER: Released ~${:.2} capital this cycle (projected)", total_released);
        }
    }
}
