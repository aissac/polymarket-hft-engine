// src/bin/hft_pingpong.rs
//! HFT Pingpong - Unified Binary (Hot Path + Background Thread)

use crossbeam_channel::bounded;
use std::thread;
use chrono::{Utc, Timelike};

use pingpong::hft_hot_path::run_sync_hot_path;
use pingpong::hft_hot_path::BackgroundTask;

/// Simplified market info
#[derive(Debug, Clone)]
struct MarketInfo {
    condition_id: String,
    token_ids: Vec<String>,
    hours_until_resolve: i64,
}

/// Get current 15-minute periods for market slugs
fn get_current_periods() -> Vec<i64> {
    let now = Utc::now();
    let minute = (now.minute() / 15) * 15;
    let period_start = now.with_minute(minute).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let base_ts = period_start.timestamp();
    vec![base_ts, base_ts - 900, base_ts + 900]
}

/// Fetch a single market by slug (same logic as old binary)
async fn fetch_market_by_slug(client: &reqwest::Client, slug: &str, now: &chrono::DateTime<Utc>) -> Option<MarketInfo> {
    let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
    
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    
    let markets: Vec<serde_json::Value> = resp.json().await.ok()?;
    let market = markets.into_iter().next()?;
    
    // Parse end date
    let end_date_str = market.get("endDate")?.as_str()?;
    let end_date = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&Utc);
    let hours_until_resolve = (end_date - *now).num_hours();
    
    // Skip if already resolved or more than 1 hour away
    if hours_until_resolve < 0 || hours_until_resolve > 1 {
        return None;
    }
    
    // Get token IDs from clobTokenIds (JSON string)
    let token_ids_str = market.get("clobTokenIds")?.as_str()?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_str).ok()?;
    
    if token_ids.len() < 2 || token_ids[0].is_empty() {
        return None;
    }
    
    Some(MarketInfo {
        condition_id: market.get("conditionId")?.as_str()?.to_string(),
        token_ids: token_ids.into_iter().take(2).collect(),
        hours_until_resolve,
    })
}

/// Fetch tokens from Gamma API
async fn fetch_tokens() -> Vec<String> {
    println!("📊 Fetching BTC/ETH Up/Down markets from REST API...");
    
    let client = reqwest::Client::new();
    let mut all_tokens: Vec<String> = Vec::new();
    let now = Utc::now();
    
    let assets = ["btc", "eth"];
    let periods = get_current_periods();
    
    for asset in &assets {
        for &period_ts in &periods {
            // Try 15m market
            let slug_15m = format!("{}-updown-15m-{}", asset, period_ts);
            if let Some(market) = fetch_market_by_slug(&client, &slug_15m, &now).await {
                all_tokens.extend(market.token_ids);
            }
            
            // Try 5m market
            let slug_5m = format!("{}-updown-5m-{}", asset, period_ts - 600);
            if let Some(market) = fetch_market_by_slug(&client, &slug_5m, &now).await {
                all_tokens.extend(market.token_ids);
            }
        }
    }
    
    // Deduplicate
    all_tokens.sort();
    all_tokens.dedup();
    
    println!("📊 Fetched {} tokens from {} markets", all_tokens.len(), all_tokens.len() / 2);
    all_tokens
}

fn main() {
    println!("=======================================================");
    println!("🚀 INITIALIZING POLYMARKET HFT ENGINE (Unified)");
    println!("=======================================================");

    // 1. Create the lock-free bridge
    let (tx, rx) = bounded(65536);

    // 2. Spawn Background Thread (Ghost Simulation + Telegram)
    let _bg_handle = thread::Builder::new()
        .name("background-dispatcher".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            rt.block_on(async move {
                println!("[BG] Background Tokio runtime started");

                while let Ok(task) = rx.recv() {
                    match task {
                        BackgroundTask::EdgeDetected { token_hash, combined_price, .. } => {
                            println!("[BG] Edge: hash={:016x} combined=${:.4}", 
                                token_hash, 
                                combined_price as f64 / 1_000_000.0
                            );
                        }
                        BackgroundTask::LatencyStats { min_ns, max_ns, avg_ns, p99_ns, sample_count } => {
                            println!(
                                "[HFT] 🔥 5s STATS | avg={:.2}µs min={:.2}µs max={:.2}µs p99={:.2}µs | {} samples",
                                avg_ns as f64 / 1000.0,
                                min_ns as f64 / 1000.0,
                                max_ns as f64 / 1000.0,
                                p99_ns as f64 / 1000.0,
                                sample_count
                            );
                        }
                    }
                }
            });
        })
        .expect("Failed to spawn background thread");

    // 3. Pin Hot Path to isolated CPU core
    if let Some(core_ids) = core_affinity::get_core_ids() {
        for core in core_ids {
            if core.id == 1 {
                if core_affinity::set_for_current(core) {
                    println!("🔒 [HFT] Pinned to core {}", core.id);
                }
                break;
            }
        }
    }

    // 4. Get tokens from Gamma API
    let tokens: Vec<String> = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fetch_tokens());

    if tokens.is_empty() {
        eprintln!("❌ No tokens fetched, exiting");
        std::process::exit(1);
    }

    println!("🚀 Starting HFT hot path...");
    println!("📡 Subscribing to {} tokens", tokens.len());

    // 5. Run Hot Path
    run_sync_hot_path(tx, tokens);

    eprintln!("🚨 [HFT] Hot path exited");
    std::process::exit(1);
}