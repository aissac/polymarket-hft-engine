// src/hft_hot_path.rs
//! HFT Hot Path - Sync Tungstenite WebSocket Processing

use crossbeam_channel::Sender;
use std::time::Duration;
use std::collections::HashMap;

/// Target combined price for arbitrage (95 cents)
const EDGE_THRESHOLD_U64: u64 = 950_000;

/// Background task for crossbeam channel
#[derive(Debug, Clone)]
pub enum BackgroundTask {
    EdgeDetected {
        token_hash: u64,
        combined_price: u64,
        timestamp_nanos: u64,
    },
}

/// Run the synchronous hot path
pub fn run_sync_hot_path(tx: Sender<BackgroundTask>, tokens: Vec<String>) {
    use tungstenite::{connect, Message};
    
    // Connect to Polymarket WebSocket
    let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    
    let (mut socket, response) = match connect(ws_url) {
        Ok((s, r)) => (s, r),
        Err(e) => {
            eprintln!("❌ Failed to connect: {:?}", e);
            std::process::exit(1);
        }
    };
    
    println!("✅ Primary WebSocket connected (sync)");
    println!("HTTP status: {}", response.status());

    // Subscribe to tokens
    let sub_msg = serde_json::json!({
        "type": "subscribe",
        "assets_ids": tokens
    });
    
    let _ = socket.write_message(Message::Text(sub_msg.to_string()));
    println!("📡 Subscribed to {} tokens", tokens.len());
    println!("[HFT] 🔥 Starting UNIFIED busy-poll loop...");

    // Local orderbook
    let mut orderbook: HashMap<u64, (u64, u64, u64, u64)> = HashMap::with_capacity(128);
    
    // Latency tracking
    let mut latency_samples: Vec<u64> = Vec::with_capacity(4096);
    let mut last_stat_time = std::time::Instant::now();

    // Main loop
    loop {
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("WS Read Error: {:?}", e);
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        if let Message::Text(text) = msg {
            let start_tsc = minstant::Instant::now();
            let len = text.len();

            // Skip pong messages
            if len < 10 || text.starts_with("pong") {
                continue;
            }

            // Parse JSON
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                // Extract market data
                if let (Some(asset_id), Some(price_changes)) = (
                    value.get("asset_id").and_then(|v| v.as_str()),
                    value.get("price_changes").and_then(|v| v.as_array())
                ) {
                    let token_hash = fast_hash(asset_id.as_bytes());
                    
                    for change in price_changes {
                        if let (Some(price_str), Some(size_str)) = (
                            change.get("price").and_then(|v| v.as_str()),
                            change.get("size").and_then(|v| v.as_str())
                        ) {
                            let price = parse_fixed_6(price_str.as_bytes());
                            let size = parse_fixed_6(size_str.as_bytes());
                            
                            orderbook.entry(token_hash)
                                .or_insert((price, 0, size, 0));
                        }
                    }

                    // Edge detection
                    let complement_hash = token_hash ^ 1;
                    if let Some((yes_price, _, yes_size, _)) = orderbook.get(&token_hash) {
                        if let Some((c_yes_price, _, c_yes_size, _)) = orderbook.get(&complement_hash) {
                            let combined = yes_price + c_yes_price;
                            
                            if combined <= EDGE_THRESHOLD_U64 && *yes_size > 0 && *c_yes_size > 0 {
                                let _ = tx.try_send(BackgroundTask::EdgeDetected {
                                    token_hash,
                                    combined_price: combined,
                                    timestamp_nanos: start_tsc.elapsed().as_nanos() as u64,
                                });
                            }
                        }
                    }
                }
            }

            // Track latency
            let elapsed_nanos = start_tsc.elapsed().as_nanos() as u64;
            latency_samples.push(elapsed_nanos);

            // 5-second stats
            if last_stat_time.elapsed() >= Duration::from_secs(5) {
                if !latency_samples.is_empty() {
                    let mut sorted: Vec<u64> = latency_samples.clone();
                    sorted.sort_unstable();
                    
                    let min = sorted[0];
                    let max = sorted[sorted.len() - 1];
                    let sum: u64 = sorted.iter().sum();
                    let avg = sum / sorted.len() as u64;
                    let p99_idx = ((sorted.len() as f64) * 0.99) as usize;
                    let p99 = sorted[p99_idx.min(sorted.len() - 1)];

                    println!(
                        "[HFT] 🔥 5s STATS | avg={:.2}µs min={:.2}µs max={:.2}µs p99={:.2}µs | {} samples",
                        avg as f64 / 1000.0,
                        min as f64 / 1000.0,
                        max as f64 / 1000.0,
                        p99 as f64 / 1000.0,
                        sorted.len()
                    );
                }

                latency_samples.clear();
                last_stat_time = std::time::Instant::now();
            }
        }
    }
}

/// FNV-1a fast hash
#[inline(always)]
fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Zero-allocation fixed-point parser
#[inline(always)]
fn parse_fixed_6(bytes: &[u8]) -> u64 {
    let mut val: u64 = 0;
    let mut fraction_digits = 0;
    let mut in_fraction = false;

    for &b in bytes {
        if b == b'.' {
            in_fraction = true;
        } else if b.is_ascii_digit() {
            val = val * 10 + (b - b'0') as u64;
            if in_fraction {
                fraction_digits += 1;
                if fraction_digits == 6 {
                    break;
                }
            }
        }
    }

    while fraction_digits < 6 {
        val *= 10;
        fraction_digits += 1;
    }

    val
}
