// src/bin/hft_pingpong.rs
//! HFT Pingpong - Unified Binary (Hot Path + Background Thread)

use crossbeam_channel::bounded;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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
    println!("🚀 POLYMARKET HFT ENGINE (memchr + Killswitch + Telegram)");
    println!("=======================================================");

    // Killswitch: AtomicBool for 1ns check in hot path
    let killswitch = Arc::new(AtomicBool::new(false));
    let killswitch_hot = Arc::clone(&killswitch);

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
                println!("[BG] Tokio runtime started");
                println!("[BG] Killswitch: ARMED (-3% drawdown will halt)");

                while let Ok(task) = rx.recv() {
                    match task {
                        BackgroundTask::EdgeDetected { token_hash, combined_price, yes_size, no_size, .. } => {
                            // Ghost simulation would go here
                            println!("📊 Edge: hash={:016x} combined=${:.4} yes={} no={}", 
                                token_hash, 
                                combined_price as f64 / 1_000_000.0,
                                yes_size,
                                no_size
                            );
                        }
                        BackgroundTask::LatencyStats { min_ns, max_ns, avg_ns, p99_ns, sample_count } => {
                            println!(
                                "[HFT] 🔥 avg={:.2}µs min={:.2}µs max={:.2}µs p99={:.2}µs | {} samples",
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
    #[cfg(target_os = "linux")]
    {
        use std::mem::size_of;
        let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::CPU_SET(1, &mut cpu_set);
            if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &cpu_set) == 0 {
                println!("🔒 Pinned to CPU 1 (on {})", libc::sched_getcpu());
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

    println!("🚀 Starting HFT hot path... 📡 {} tokens", tokens.len());
    println!("💰 Max position: $5.00 per trade");

    // 5. Run Hot Path with Killswitch
    run_sync_hot_path(tx, tokens, killswitch_hot);

    eprintln!("🚨 Hot path exited");
    std::process::exit(1);
}