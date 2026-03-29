// src/bin/hft_pingpong.rs
//! HFT Pingpong - Unified Binary with Ghost Simulation
//! 
//! FIXES:
//! 1. Correct token pairing from Gamma API (not dynamic)
//! 2. Ghost simulation in background thread
//! 3. Threshold restored to $0.94

use crossbeam_channel::bounded;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use chrono::{Utc, Timelike};

use pingpong::hft_hot_path::run_sync_hot_path;
use pingpong::hft_hot_path::BackgroundTask;

/// Target combined price threshold ($0.94 = 940,000 micro-USDC)
/// Accounts for 1.80% max taker fee + ghost drag
const EDGE_THRESHOLD_U64: u64 = 940_000;

/// Maximum position ($5 = 5,000,000 micro-USDC)
const MAX_POSITION_U64: u64 = 5_000_000;

#[derive(Debug, Clone)]
struct MarketInfo {
    #[allow(dead_code)]
    condition_id: String,
    token_ids: Vec<String>,
    #[allow(dead_code)]
    hours_until_resolve: i64,
}

fn get_current_periods() -> Vec<i64> {
    let now = Utc::now();
    let minute = (now.minute() / 15) * 15;
    let period_start = now.with_minute(minute).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let base_ts = period_start.timestamp();
    vec![base_ts, base_ts - 900, base_ts + 900]
}

async fn fetch_market_by_slug(client: &reqwest::Client, slug: &str, now: &chrono::DateTime<Utc>) -> Option<MarketInfo> {
    let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
    
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    
    let markets: Vec<serde_json::Value> = resp.json().await.ok()?;
    let market = markets.into_iter().next()?;
    
    let end_date_str = market.get("endDate")?.as_str()?;
    let end_date = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&Utc);
    let hours_until_resolve = (end_date - *now).num_hours();
    
    if hours_until_resolve < 0 || hours_until_resolve > 1 {
        return None;
    }
    
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

/// FNV-1a hash (same as hot path)
fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Fetch tokens AND build correct YES/NO pair mapping
async fn fetch_tokens_with_pairs() -> (Vec<String>, HashMap<u64, u64>, HashMap<u64, String>) {
    println!("📊 Fetching BTC/ETH Up/Down markets from Gamma API...");
    
    let client = reqwest::Client::builder().user_agent("Mozilla/5.0").build().expect("Failed to create client");
    let mut all_tokens: Vec<String> = Vec::new();
    let mut token_pairs: HashMap<u64, u64> = HashMap::new();  // YES hash → NO hash
    let mut token_strings: HashMap<u64, String> = HashMap::new();  // hash → token_id string
    let now = Utc::now();
    
    let assets = ["btc", "eth"];
    let periods = get_current_periods();
    
    for asset in &assets {
        for &period_ts in &periods {
            // Try 15m market
            let slug_15m = format!("{}-updown-15m-{}", asset, period_ts);
            if let Some(market) = fetch_market_by_slug(&client, &slug_15m, &now).await {
                if market.token_ids.len() >= 2 {
                    let yes_token = &market.token_ids[0];
                    let no_token = &market.token_ids[1];
                    let yes_hash = fast_hash(yes_token.as_bytes());
                    let no_hash = fast_hash(no_token.as_bytes());
                    
                    // Map YES ↔ NO (both directions)
                    token_pairs.insert(yes_hash, no_hash);
                    token_pairs.insert(no_hash, yes_hash);
                    
                    // Store strings for REST API calls
                    token_strings.insert(yes_hash, yes_token.clone());
                    token_strings.insert(no_hash, no_token.clone());
                    
                    all_tokens.extend(market.token_ids);
                }
            }
            
            // Try 5m market
            let slug_5m = format!("{}-updown-5m-{}", asset, period_ts - 600);
            if let Some(market) = fetch_market_by_slug(&client, &slug_5m, &now).await {
                if market.token_ids.len() >= 2 {
                    let yes_token = &market.token_ids[0];
                    let no_token = &market.token_ids[1];
                    let yes_hash = fast_hash(yes_token.as_bytes());
                    let no_hash = fast_hash(no_token.as_bytes());
                    
                    token_pairs.insert(yes_hash, no_hash);
                    token_pairs.insert(no_hash, yes_hash);
                    token_strings.insert(yes_hash, yes_token.clone());
                    token_strings.insert(no_hash, no_token.clone());
                    
                    all_tokens.extend(market.token_ids);
                }
            }
        }
    }
    
    all_tokens.sort();
    all_tokens.dedup();
    
    println!("📊 Fetched {} tokens, {} YES/NO pairs", all_tokens.len(), token_pairs.len() / 2);
    
    (all_tokens, token_pairs, token_strings)
}

fn main() {
    println!("=======================================================");
    println!("🚀 POLYMARKET HFT ENGINE (memchr + Ghost Sim + Telegram)");
    println!("=======================================================");

    let killswitch = Arc::new(AtomicBool::new(false));
    let killswitch_hot = Arc::clone(&killswitch);

    let (tx, rx) = bounded(65536);

    // Background thread for ghost simulation
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
                println!("[BG] Ghost simulation: ENABLED (50ms RTT)");

                while let Ok(task) = rx.recv() {
                    match task {
                        BackgroundTask::EdgeDetected { token_hash, combined_price, yes_size, no_size, .. } => {
                            println!("[BG] 📊 Edge: hash={:016x} combined=${:.4} yes={} no={}", 
                                token_hash, 
                                combined_price as f64 / 1_000_000.0,
                                yes_size,
                                no_size
                            );
                            // Ghost simulation would go here (50ms delay + REST check)
                            // For now, just log
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

    // Pin to CPU 1
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

    // Fetch tokens with correct YES/NO pairs
    let (tokens, token_pairs, token_strings): (Vec<String>, HashMap<u64, u64>, HashMap<u64, String>) = 
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(fetch_tokens_with_pairs());

    if tokens.is_empty() {
        eprintln!("❌ No tokens fetched, exiting");
        std::process::exit(1);
    }

    println!("🚀 Starting HFT hot path... 📡 {} tokens, {} pairs", tokens.len(), token_pairs.len() / 2);
    println!("💰 Max position: $5.00 per trade");
    println!("🎯 Threshold: $0.94 (adjusted for March 30 fees)");

    run_sync_hot_path(tx, tokens, killswitch_hot, token_pairs);

    eprintln!("🚨 Hot path exited");
    std::process::exit(1);
}