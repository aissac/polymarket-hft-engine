// src/bin/hft_pingpong.rs
//! HFT Pingpong - Sub-microsecond Polymarket Orderbook Processor
//!
//! Architecture: One-Way State Propagation

use crossbeam_channel::bounded;
use std::thread;

// Use module from lib
use pingpong::hft_hot_path::run_sync_hot_path;

/// Fetch tokens from Gamma API
async fn fetch_tokens() -> Vec<String> {
    println!("📊 Fetching BTC/ETH Up/Down markets from REST API...");
    vec![
        "96408543298904617446523822137153069418764739261787888217644515210922136845491".to_string(),
        "59954835907681619931244536249752663855967744102320616217752569180734806747405".to_string(),
    ]
}

fn main() {
    println!("🚀 INITIALIZING POLYMARKET HFT ENGINE");

    let (tx, rx) = bounded(65536);

    let _bg_handle = thread::Builder::new()
        .name("background-dispatcher".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            rt.block_on(async move {
                while let Ok(_task) = rx.recv() {}
            });
        })
        .expect("Failed to spawn background thread");

    if let Some(core_ids) = core_affinity::get_core_ids() {
        for core in core_ids {
            if core.id == 1 {
                core_affinity::set_for_current(core);
                break;
            }
        }
    }

    let tokens: Vec<String> = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fetch_tokens());

    println!("🚀 Starting HFT hot path...");
    run_sync_hot_path(tx, tokens);
}
