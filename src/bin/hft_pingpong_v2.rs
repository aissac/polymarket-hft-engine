//! HFT Pingpong - Integrated Binary with NotebookLM Fixes
//! 
//! Architecture:
//! - Hot path: sync tungstenite, CPU pinned, memchr parser
//! - Background: tokio runtime, crossbeam bridge via spawn_blocking
//! - Token maps: hash → token_id → condition_id
//! - Execution: Maker (GTC) + Taker (FAK) routing

use crossbeam_channel::bounded;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use chrono::{Utc, Timelike};
use reqwest::Client;

use pingpong::hft_hot_path::run_sync_hot_path;
use pingpong::hft_hot_path::BackgroundTask;
use pingpong::token_map::{hash_token, build_maps};
use pingpong::background_wiring::process_edge;
use pingpong::execution::{build_hft_client, pre_warm_connections};
use pingpong::signing::init_signer;
use pingpong::condition_map::MARKET_SLUGS;

/// Target combined price threshold ($0.94 = 940,000 micro-USDC)
const EDGE_THRESHOLD_U64: u64 = 940_000;
const MIN_VALID_COMBINED_U64: u64 = 900_000;
const MAX_POSITION_U64: u64 = 5_000_000;

fn main() {
    println!("=======================================================");
    println!("🚀 POLYMARKET HFT ENGINE (NotebookLM Integration)");
    println!("=======================================================");

    let killswitch = Arc::new(AtomicBool::new(false));
    let killswitch_hot = Arc::clone(&killswitch);

    // ============================================================
    // 1. LOAD ENVIRONMENT VARIABLES
    // ============================================================
    dotenvy::dotenv().ok();
    
    let api_key = std::env::var("POLYMARKET_API_KEY").unwrap_or_else(|_| "mock_api_key".to_string());
    let api_secret = std::env::var("POLYMARKET_API_SECRET").unwrap_or_else(|_| "mock_secret".to_string());
    let api_passphrase = std::env::var("POLYMARKET_PASSPHRASE").unwrap_or_else(|_| "mock_passphrase".to_string());
    let private_key = std::env::var("POLYMARKET_PRIVATE_KEY").unwrap_or_else(|_| "0000000000000000000000000000000000000000000000000000000000000000".to_string());
    let signer_address = std::env::var("POLYMARKET_SAFE_ADDRESS").unwrap_or_else(|_| "0x0000000000000000000000000000000000000000".to_string());
    let dry_run = std::env::var("DRY_RUN").unwrap_or_else(|_| "true".to_string()).to_lowercase() == "true";

    println!("📋 Config: DRY_RUN={}, API_KEY={}, Signer={}", dry_run, &api_key[..8], &signer_address[..10]);

    // ============================================================
    // 2. BUILD HTTP/2 CLIENT (NotebookLM Fix #2)
    // ============================================================
    let http_client = Arc::new(build_hft_client());

    // ============================================================
    // 3. PRE-WARM CONNECTIONS (NotebookLM Fix #4)
    // ============================================================
    let temp_rt = tokio::runtime::Runtime::new().unwrap();
    temp_rt.block_on(pre_warm_connections(&http_client));

    // ============================================================
    // 4. BUILD TOKEN MAPS (NotebookLM Integration)
    // ============================================================
    println!("🔨 Building token maps from Gamma API...");
    let (hash_to_id, id_to_condition) = temp_rt.block_on(async {
        build_maps(&http_client, MARKET_SLUGS).await
    });
    
    let hash_to_id_arc = Arc::new(hash_to_id);
    let id_to_condition_arc = Arc::new(id_to_condition);

    // ============================================================
    // 5. INITIALIZE SIGNER
    // ============================================================
    let signer = Arc::new(init_signer(&private_key).expect("Failed to initialize signer"));
    println!("✅ Signer initialized: {:?}", signer.address());

    // ============================================================
    // 6. FETCH TOKENS WITH CORRECT YES/NO PAIRS
    // ============================================================
    println!("📊 Fetching token list from Gamma API...");
    let (all_tokens, token_pairs, _token_strings) = temp_rt.block_on(async {
        fetch_tokens_with_pairs(&http_client, MARKET_SLUGS).await
    });
    
    println!("📊 Fetched {} tokens, {} YES/NO pairs", all_tokens.len(), token_pairs.len() / 2);

    // ============================================================
    // 7. CREATE CHANNELS
    // ============================================================
    let (opportunity_tx, opportunity_rx) = bounded::<BackgroundTask>(1024);

    // ============================================================
    // 8. SPAWN BACKGROUND THREAD (NotebookLM Bridge)
    // ============================================================
    let client_bg = Arc::clone(&http_client);
    let hash_map_bg = Arc::clone(&hash_to_id_arc);
    let condition_map_bg = Arc::clone(&id_to_condition_arc);
    let signer_bg = Arc::clone(&signer);
    let api_key_bg = Arc::new(api_key.clone());
    let api_secret_bg = Arc::new(api_secret.clone());
    let api_passphrase_bg = Arc::new(api_passphrase.clone());
    let signer_address_bg = Arc::new(signer_address.clone());

    thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        
        rt.block_on(async {
            // Bridge: crossbeam receiver → tokio async tasks
            while let Ok(task) = opportunity_rx.recv() {
                        // Clone Arcs for this iteration
                        let api_key_iter = Arc::clone(&api_key_bg);
                        let api_secret_iter = Arc::clone(&api_secret_bg);
                        let api_passphrase_iter = Arc::clone(&api_passphrase_bg);
                        let signer_address_iter = Arc::clone(&signer_address_bg);
                match task {
                    BackgroundTask::EdgeDetected { 
                        token_hash, 
                        complement_hash,
                        combined_price, 
                        yes_size, 
                        no_size,
                        .. 
                    } => {
                        // Clone Arcs for the async task
                        let client_task = Arc::clone(&client_bg);
                        let hash_map_task = Arc::clone(&hash_map_bg);
                        let condition_map_task = Arc::clone(&condition_map_bg);
                        let signer_task = Arc::clone(&signer_bg);

                        // Spawn async task for execution
                        tokio::spawn(async move {
                            // Use iteration clones
                            let api_key_task = Arc::clone(&api_key_iter);
                            let api_secret_task = Arc::clone(&api_secret_iter);
                            let api_passphrase_task = Arc::clone(&api_passphrase_iter);
                            let signer_address_task = Arc::clone(&signer_address_iter);
                            process_edge(
                                token_hash,
                                complement_hash,
                                yes_size,
                                no_size,
                                combined_price,
                                &hash_map_task,
                                &condition_map_task,
                                &client_task,
                                &signer_task,
                                &api_key_task,
                                &api_secret_task,
                                &api_passphrase_task,
                                &signer_address_task,
                                dry_run,
                            ).await;
                        });
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
    });

    println!("✅ Background execution thread spawned");

    // ============================================================
    // 9. RUN HOT PATH (CPU PINNED, UNCHANGED)
    // ============================================================
    println!("🔥 Starting hot path (memchr parser, target: <1µs)...");
    run_sync_hot_path(opportunity_tx, all_tokens, killswitch_hot, token_pairs);
}

/// Fetch tokens and build YES/NO pairs from Gamma API
async fn fetch_tokens_with_pairs(
    client: &Client,
    market_slugs: &[&str],
) -> (Vec<String>, HashMap<u64, u64>, Vec<String>) {
    use serde_json::Value;
    
    let mut all_tokens = Vec::new();
    let mut token_pairs = HashMap::new();
    let mut token_strings = Vec::new();

    for slug in market_slugs {
        let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
        
        match client.get(&url).send().await {
            Ok(resp) => {
                match resp.json::<Value>().await {
                    Ok(json) => {
                        if let Some(events) = json.as_array() {
                            for event in events {
                                if let Some(markets) = event["markets"].as_array() {
                                    for market in markets {
                                        if let Some(tokens) = market["tokens"].as_array() {
                                            let mut yes_token = None;
                                            let mut no_token = None;
                                            
                                            for token in tokens {
                                                let token_id = token["token_id"].as_str().unwrap_or("").to_string();
                                                let outcome = token["outcome"].as_str().unwrap_or("");
                                                
                                                if !token_id.is_empty() {
                                                    all_tokens.push(token_id.clone());
                                                    token_strings.push(token_id.clone());
                                                    
                                                    if outcome.contains("Yes") || outcome.contains("YES") {
                                                        yes_token = Some(token_id.clone());
                                                    } else if outcome.contains("No") || outcome.contains("NO") {
                                                        no_token = Some(token_id.clone());
                                                    }
                                                }
                                            }
                                            
                                            if let (Some(yes), Some(no)) = (yes_token, no_token) {
                                                let yes_hash = hash_token(&yes);
                                                let no_hash = hash_token(&no);
                                                token_pairs.insert(yes_hash, no_hash);
                                                token_pairs.insert(no_hash, yes_hash);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("⚠️ Failed to parse JSON for {}: {}", slug, e),
                }
            }
            Err(e) => eprintln!("⚠️ Failed to fetch {}: {}", slug, e),
        }
    }
    
    all_tokens.sort();
    all_tokens.dedup();
    
    (all_tokens, token_pairs, token_strings)
}