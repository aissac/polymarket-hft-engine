//! Metrics-based logging - Zero disk I/O hot path
//! 
//! Instead of logging every edge (500/sec), aggregate in memory
//! and emit a 1-second heartbeat summary.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Global metrics (shared with hot path)
pub static EDGE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static PAIR_CHECKS: AtomicU64 = AtomicU64::new(0);
pub static MSG_COUNT: AtomicU64 = AtomicU64::new(0);
pub static DEEPEST_EDGE: AtomicU64 = AtomicU64::new(0); // In micro-dollars

/// Log heartbeat every second
pub fn start_heartbeat_thread() {
    std::thread::spawn(|| {
        let mut last_edges = 0u64;
        let mut last_checks = 0u64;
        let mut last_msgs = 0u64;
        let start = Instant::now();
        
        loop {
            std::thread::sleep(Duration::from_secs(1));
            
            let edges = EDGE_COUNT.load(Ordering::Relaxed);
            let checks = PAIR_CHECKS.load(Ordering::Relaxed);
            let msgs = MSG_COUNT.load(Ordering::Relaxed);
            let deepest = DEEPEST_EDGE.load(Ordering::Relaxed);
            
            let edges_per_sec = edges.saturating_sub(last_edges);
            let checks_per_sec = checks.saturating_sub(last_checks);
            let msgs_per_sec = msgs.saturating_sub(last_msgs);
            
            let deepest_cents = deepest as f64 / 10_000.0;
            
            let elapsed = start.elapsed().as_secs();
            
            println!("[HB] {}s | msg/s: {} | checks/s: {} | edges/s: {} | deepest: ${:.4}", 
                elapsed,
                format_number(msgs_per_sec),
                format_number(checks_per_sec),
                edges_per_sec,
                deepest_cents
            );
            
            last_edges = edges;
            last_checks = checks;
            last_msgs = msgs;
        }
    });
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
