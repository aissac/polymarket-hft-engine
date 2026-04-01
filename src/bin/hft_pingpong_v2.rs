//! HFT Pingpong - Integrated Binary with Rollover Support
//! 
//! Architecture:
//! - Hot path: sync tungstenite, CPU pinned, memchr parser, rollover channel
//! - Background: tokio runtime, crossbeam bridge via spawn_blocking
//! - Rollover: background thread checks every 60s for market transitions
//! - Token maps: hash → token_id → condition_id
//! - Execution: Maker (GTC) + Taker (FAK) routing

use crossbeam_channel::bounded;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::collections::HashMap;
use std::time::Duration;
use chrono::{Utc, Timelike};
use reqwest::Client;

use pingpong::hft_hot_path::{run_sync_hot_path, BackgroundTask, RolloverCommand};
use pingpong::market_rollover::run_rollover_thread;
use pingpong::websocket_reader::connect_to_polymarket;
use pingpong::condition_map::build_maps;
// use pingpong::token_map::hash_token;
use pingpong::background_wiring::process_edge;
use pingpong::execution::{build_hft_client, pre_warm_connections};
use pingpong::signing::init_signer;

/// Target combined price threshold ($0.94 = 940,000 micro-USDC)
const EDGE_THRESHOLD_U64: u64 = 940_000;
const MIN_VALID_COMBINED_U64: u64 = 900_000;
const MAX_POSITION_U64: u64 = 5_000_000;

fn main() {
    println!("=======================================================");
    println!("🚀 POLYMARKET HFT ENGINE (Rollover Enabled)");
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
    // 2. BUILD HTTP/2 CLIENT
    // ============================================================
    let http_client = Arc::new(build_hft_client());

    // ============================================================
    // 3. PRE-WARM CONNECTIONS
    // ============================================================
    let temp_rt = tokio::runtime::Runtime::new().unwrap();
    temp_rt.block_on(pre_warm_connections(&http_client));

    // ============================================================
    // 4. BUILD TOKEN MAPS (ACTIVE MARKETS ONLY)
    // ============================================================
    println!("🔨 Building token maps from Gamma API...");
    let (hash_to_id, id_to_condition, complement_map) = temp_rt.block_on(async {
        build_maps(&http_client).await
    });
    
    if hash_to_id.is_empty() {
        panic!("CRITICAL: 0 tokens fetched from Gamma API. Halting to prevent WebSocket spam.");
    }
    
    let hash_to_id_arc = Arc::new(hash_to_id);
    let id_to_condition_arc = Arc::new(id_to_condition);
    let pair_count = complement_map.len() / 2;
    let complement_map_arc = Arc::new(complement_map.clone());
    
    // Build token list for WebSocket subscription
    let all_tokens: Vec<String> = hash_to_id_arc.values().cloned().collect();
    println!("📊 Fetched {} tokens, {} YES/NO pairs", all_tokens.len(), pair_count);
    
    // Debug: print all tokens we're subscribing to
    println!("[SUBSCRIBE] {} tokens (first 5 only)", all_tokens.len());
    for (i, token) in all_tokens.iter().take(5).enumerate() {
        println!("[SUBSCRIBE]   #{} len={} hash={:x} token={}", 
            i, token.len(), pingpong::token_map::hash_token(token), token);
    }

    // ============================================================
    // 5. INITIALIZE SIGNER
    // ============================================================
    let signer = Arc::new(init_signer(&private_key).expect("Failed to initialize signer"));
    println!(" Signer initialized: {:?}", signer.address());

    // ============================================================
    // 6. CREATE CHANNELS
    // ============================================================
    let (opportunity_tx, opportunity_rx) = bounded::<BackgroundTask>(1024);
    let (rollover_tx, rollover_rx) = bounded::<RolloverCommand>(64);
    // Start JSONL logger thread
    let (log_tx, log_rx) = crossbeam_channel::bounded(4096);
    let _logger_handle = pingpong::jsonl_logger::JsonlLogger::start(log_rx);
    println!("JSONL logger started");

    // ============================================================
    // 7. SPAWN ROLLOVER CHECKER THREAD
    // ============================================================
    let rollover_client = Arc::clone(&http_client);
    
    // Spawn rollover thread (blocking, uses reqwest blocking client)
    let rollover_client = Arc::clone(&http_client);
    let rollover_tx_clone = rollover_tx.clone();
    
    std::thread::spawn(move || {
        // Create a tokio runtime for the async function
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            pingpong::market_rollover::run_rollover_thread(rollover_client, rollover_tx_clone).await;
        });
    });
    
    println!(" Rollover thread spawned (15s polling)");

    // ============================================================
    // 8. SPAWN BACKGROUND EXECUTION THREAD
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
            while let Ok(task) = opportunity_rx.recv() {
                let api_key_iter = Arc::clone(&api_key_bg);
                let api_secret_iter = Arc::clone(&api_secret_bg);
                let api_passphrase_iter = Arc::clone(&api_passphrase_bg);
                let signer_address_iter = Arc::clone(&signer_address_bg);
                
                match task {
                    BackgroundTask::EdgeDetected { 
                        yes_token_hash, 
                        no_token_hash,
                        combined_ask, 
                        yes_ask_size, 
                        no_ask_size,
                        .. 
                    } => {
                        let client_task = Arc::clone(&client_bg);
                        let hash_map_task = Arc::clone(&hash_map_bg);
                        let condition_map_task = Arc::clone(&condition_map_bg);
                        let signer_task = Arc::clone(&signer_bg);

                        tokio::spawn(async move {
                            let api_key_task = Arc::clone(&api_key_iter);
                            let api_secret_task = Arc::clone(&api_secret_iter);
                            let api_passphrase_task = Arc::clone(&api_passphrase_iter);
                            let signer_address_task = Arc::clone(&signer_address_iter);
                            
                            process_edge(
                                yes_token_hash,
                                no_token_hash,
                                yes_ask_size,
                                no_ask_size,
                                combined_ask,
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

    println!(" Background execution thread spawned");
    // Start heartbeat logging (1/sec summary)
    pingpong::hft_metrics::start_heartbeat_thread();

    // ============================================================
    // 9. AUTO-RECONNECT LOOP (NotebookLM Step 1)
    // ============================================================
    println!("🔄 Auto-reconnect enabled (5s delay, 30s TCP timeout)");
    
    let mut reconnect_count: u64 = 0;
    
    loop {
        // Wait before reconnecting (skip on first iteration)
        if reconnect_count > 0 {
            let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
            println!("🔄 [{}] Reconnect attempt #{} in 5 seconds...", timestamp, reconnect_count);
            std::thread::sleep(Duration::from_secs(5));
        }
        reconnect_count += 1;
        
        // Build bi-directional map (needed for each reconnect)
        let mut bidi_pairs: std::collections::HashMap<u64, (u64, u64)> = std::collections::HashMap::new();
        for (&yes_hash, &no_hash) in complement_map.iter() {
            bidi_pairs.insert(yes_hash, (yes_hash, no_hash));
            bidi_pairs.insert(no_hash, (yes_hash, no_hash));
        }
        
        println!("🔥 Starting hot path (memchr parser, target: <1µs)...");
        println!("🔄 Rollover channel active - markets will transition seamlessly");
        println!("🔗 [NETWORK] Establishing TCP Connection (attempt #{})...", reconnect_count);
        
        // Connect to WebSocket (this now has 30s TCP timeout built-in)
        let ws_stream = connect_to_polymarket(all_tokens.clone());
        
        println!("✅ [NETWORK] WebSocket connected");
        
        // Run hot path - blocks until TCP timeout (30s) or error
        run_sync_hot_path(
            ws_stream,
            opportunity_tx.clone(),
            all_tokens.clone(),
            killswitch_hot.clone(),
            bidi_pairs,
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            rollover_rx.clone(),
            log_tx.clone(),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
            Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        
        // Hot path exited - connection lost, will reconnect
        let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        println!("⚠️ [{}] Hot path exited - connection lost", timestamp);
        println!("🔄 Will reconnect in 5 seconds...");
    }
}
