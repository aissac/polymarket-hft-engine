// src/bin/hft_pingpong.rs
//! HFT Pingpong - Unified Binary with Ghost Simulation

use crossbeam_channel::bounded;
use std::thread;
use chrono::{Utc, Timelike};

use pingpong::hft_hot_path::run_sync_hot_path;
use pingpong::hft_hot_path::BackgroundTask;
use pingpong::hft_hot_path::GhostStatus;
use rand::Rng;

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
    if !resp.status().is_success() { return None; }
    
    let markets: Vec<serde_json::Value> = resp.json().await.ok()?;
    let market = markets.into_iter().next()?;
    
    let end_date_str = market.get("endDate")?.as_str()?;
    let end_date = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&Utc);
    let hours_until_resolve = (end_date - *now).num_hours();
    
    if hours_until_resolve < 0 || hours_until_resolve > 1 { return None; }
    
    let token_ids_str = market.get("clobTokenIds")?.as_str()?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_str).ok()?;
    if token_ids.len() < 2 || token_ids[0].is_empty() { return None; }
    
    Some(MarketInfo {
        condition_id: market.get("conditionId")?.as_str()?.to_string(),
        token_ids: token_ids.into_iter().take(2).collect(),
        hours_until_resolve,
    })
}

async fn fetch_tokens() -> Vec<String> {
    println!("📊 Fetching BTC/ETH Up/Down markets...");
    let client = reqwest::Client::new();
    let mut all_tokens: Vec<String> = Vec::new();
    let now = Utc::now();
    let assets = ["btc", "eth"];
    
    for asset in &assets {
        for &period_ts in &get_current_periods() {
            let slug_15m = format!("{}-updown-15m-{}", asset, period_ts);
            if let Some(m) = fetch_market_by_slug(&client, &slug_15m, &now).await {
                all_tokens.extend(m.token_ids);
            }
            let slug_5m = format!("{}-updown-5m-{}", asset, period_ts - 600);
            if let Some(m) = fetch_market_by_slug(&client, &slug_5m, &now).await {
                all_tokens.extend(m.token_ids);
            }
        }
    }
    all_tokens.sort();
    all_tokens.dedup();
    println!("📊 Fetched {} tokens", all_tokens.len());
    all_tokens
}

#[derive(Debug, Clone)]
struct MarketInfo {
    #[allow(dead_code)]
    condition_id: String,
    token_ids: Vec<String>,
    #[allow(dead_code)]
    hours_until_resolve: i64,
}

async fn check_liquidity(_client: &reqwest::Client, _token_hash: u64) -> GhostStatus {
    let rng = rand::thread_rng().gen_range(0..100);
    if rng < 62 { GhostStatus::Ghosted }
    else if rng < 97 { GhostStatus::Executable }
    else { GhostStatus::Partial }
}

fn main() {
    println!("=======================================================");
    println!("🚀 POLYMARKET HFT ENGINE (Unified + Ghost Sim)");
    println!("=======================================================");

    let (tx, rx) = bounded(65536);

    let _bg = thread::Builder::new().name("bg".into()).spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async {
            println!("[BG] Tokio runtime started, ghost sim enabled");
            let http = reqwest::Client::new();
            while let Ok(task) = rx.recv() {
                match task {
                    BackgroundTask::EdgeDetected { token_hash, combined_price, yes_size, no_size, .. } => {
                        let c = http.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                            let s = check_liquidity(&c, token_hash).await;
                            println!("👻 GHOST: {:016x} ${:.4} {:?} y={} n={}", token_hash, combined_price as f64/1e6, s, yes_size, no_size);
                        });
                    }
                    BackgroundTask::LatencyStats { min_ns, max_ns, avg_ns, p99_ns, sample_count } => {
                        println!("[HFT] 🔥 avg={:.2}µs min={:.2}µs max={:.2}µs p99={:.2}µs | {} samples", avg_ns as f64 / 1000.0, min_ns as f64 / 1000.0, max_ns as f64 / 1000.0, p99_ns as f64 / 1000.0, sample_count);
                    }
                }
            }
        });
    }).unwrap();

    #[cfg(target_os = "linux")] {
        use std::mem::size_of;
        let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::CPU_SET(1, &mut cpu_set);
            if libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &cpu_set) == 0 {
                println!("🔒 Pinned to CPU 1 (on {})", libc::sched_getcpu());
            }
        }
    }

    let tokens = tokio::runtime::Runtime::new().unwrap().block_on(fetch_tokens());
    if tokens.is_empty() { eprintln!("❌ No tokens"); std::process::exit(1); }

    println!("🚀 Starting HFT hot path... 📡 {} tokens", tokens.len());
    run_sync_hot_path(tx, tokens);
    eprintln!("🚨 Hot path exited");
    std::process::exit(1);
}
