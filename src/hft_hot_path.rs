// src/hft_hot_path.rs
//! HFT Hot Path - Zero-Allocation Raw Byte Scanner with memchr
//!
//! Uses memchr to scan WebSocket messages without parsing full JSON DOM.
//! Expected latency: ~50-100ns per message

use crossbeam_channel::Sender;
use std::time::Duration;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Target combined price for arbitrage (94 cents = $0.94)
const EDGE_THRESHOLD_U64: u64 = 940_000;

/// Maximum position size ($5 = 5_000_000 micro-USDC)
const MAX_POSITION_U64: u64 = 5_000_000;

/// Background task for crossbeam channel
#[derive(Debug, Clone)]
pub enum BackgroundTask {
    EdgeDetected {
        token_hash: u64,
        combined_price: u64,
        timestamp_nanos: u64,
        yes_size: u64,
        no_size: u64,
    },
    LatencyStats {
        min_ns: u64,
        max_ns: u64,
        avg_ns: u64,
        p99_ns: u64,
        sample_count: u64,
    },
}

/// Ghost simulation status
#[derive(Debug, Clone, PartialEq)]
pub enum GhostStatus {
    Executable,
    Partial,
    Ghosted,
}

/// Run the synchronous hot path with memchr byte scanning
pub fn run_sync_hot_path(tx: Sender<BackgroundTask>, tokens: Vec<String>, killswitch: Arc<AtomicBool>) {
    use tungstenite::{connect, Message};
    use memchr::memmem;
    
    println!("[HFT] Using memchr zero-allocation scanner (target: <1µs)");
    
    // Connect to Polymarket CLOB WebSocket
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

    // Subscribe using correct format
    let subscribe_msg = serde_json::json!({
        "type": "market",
        "operation": "subscribe",
        "markets": [],
        "assets_ids": tokens,
        "initial_dump": true
    });
    
    let msg_str = serde_json::to_string(&subscribe_msg).expect("Failed to serialize subscription");
    let _ = socket.write_message(Message::Text(msg_str.into()));
    
    println!("📡 Subscribed to {} tokens", tokens.len());
    println!("[HFT] 🔥 Starting MEMCHR hot path (50-100ns target)...");

    // Local orderbook: token_hash -> (yes_price, no_price, yes_size, no_size)
    let mut orderbook: HashMap<u64, (u64, u64, u64, u64)> = HashMap::with_capacity(128);
    
    // Warmup counter
    let mut warmup_count = 0;
    
    // Latency tracking
    let mut latency_samples: Vec<u64> = Vec::with_capacity(8192);
    let mut last_stat_time = std::time::Instant::now();
    
    // Pre-compiled patterns for memchr
    let asset_pattern = memmem::Finder::new(b"\"asset_id\":\"");
    let price_changes_pattern = memmem::Finder::new(b"\"price_changes\":[{");
    let asks_pattern = memmem::Finder::new(b"\"asks\":[{");
    let price_pattern = memmem::Finder::new(b"\"price\":\"");
    let size_pattern = memmem::Finder::new(b"\"size\":\"");

    // Main busy-poll loop
    loop {
        // Killswitch check (1ns atomic read)
        if killswitch.load(Ordering::Relaxed) {
            println!("[HFT] 🚨 KILLSWITCH ENGAGED - draining socket");
            let _ = socket.read();
            continue;
        }
        
        let msg = match socket.read() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[HFT] WS Read Error: {:?}", e);
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };

        if let Message::Text(text) = msg {
            let start_tsc = minstant::Instant::now();
            let bytes = text.as_bytes();
            let len = bytes.len();

            // Skip tiny messages (pongs, heartbeats)
            if len < 100 {
                continue;
            }

            // Warmup period
            if warmup_count < 50 {
                warmup_count += 1;
                if warmup_count == 50 {
                    println!("[HFT] ✅ Warmed up after 50 messages");
                }
            }

            // ============================================
            // ZERO-ALLOCATION RAW BYTE SCANNING WITH MEMCHR
            // ============================================
            
            // 1. Find asset_id (Polymarket tokens are 66 chars)
            if let Some(asset_idx) = asset_pattern.find(bytes) {
                let token_start = asset_idx + 12; // Length of "asset_id":"
                
                if token_start + 66 <= len {
                    // Extract 66-char token ID for hashing
                    let token_bytes = &bytes[token_start..token_start + 66];
                    let token_hash = fast_hash(token_bytes);
                    
                    // 2. Determine message format
                    let is_price_changes = price_changes_pattern.find(bytes).is_some();
                    let is_asks = asks_pattern.find(bytes).is_some();
                    
                    if is_price_changes || is_asks {
                        // 3. Find price and size using memchr
                        if let Some(price_idx) = price_pattern.find(bytes) {
                            let price_start = price_idx + 9; // Length of "price":"
                            
                            // Find closing quote for price
                            if let Some(price_end) = memchr::memchr(b'"', &bytes[price_start..]) {
                                let price_bytes = &bytes[price_start..price_start + price_end];
                                let price = parse_fixed_6(price_bytes);
                                
                                // Find size
                                let size_search_start = price_start + price_end + 1;
                                if let Some(size_idx) = size_pattern.find(&bytes[size_search_start..]) {
                                    let size_start = size_search_start + size_idx + 8; // Length of "size":"
                                    
                                    if let Some(size_end) = memchr::memchr(b'"', &bytes[size_start..]) {
                                        let size_bytes = &bytes[size_start..size_start + size_end];
                                        let size = parse_fixed_6(size_bytes);
                                        
                                        // 4. Update orderbook
                                        orderbook.entry(token_hash)
                                            .and_modify(|(p, _, s, _)| { *p = price; *s = size; })
                                            .or_insert((price, 0, size, 0));
                                    }
                                }
                            }
                        }

                        // 5. Edge detection with $5 max position cap
                        let complement_hash = token_hash ^ 1;
                        if let Some((yes_price, _, yes_size, _)) = orderbook.get(&token_hash) {
                            if let Some((c_yes_price, _, c_yes_size, _)) = orderbook.get(&complement_hash) {
                                let combined = yes_price + c_yes_price;
                                
                                if combined <= EDGE_THRESHOLD_U64 && *yes_size > 0 && *c_yes_size > 0 {
                                    // Apply $5 max position cap
                                    let capped_yes = std::cmp::min(*yes_size, MAX_POSITION_U64);
                                    let capped_no = std::cmp::min(*c_yes_size, MAX_POSITION_U64);
                                    
                                    let _ = tx.try_send(BackgroundTask::EdgeDetected {
                                        token_hash,
                                        combined_price: combined,
                                        timestamp_nanos: start_tsc.elapsed().as_nanos() as u64,
                                        yes_size: capped_yes,
                                        no_size: capped_no,
                                    });
                                }
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
                    let sample_count = sorted.len();

                    let _ = tx.try_send(BackgroundTask::LatencyStats {
                        min_ns: min,
                        max_ns: max,
                        avg_ns: avg,
                        p99_ns: p99,
                        sample_count: sample_count as u64,
                    });
                }

                latency_samples.clear();
                last_stat_time = std::time::Instant::now();
            }
        }
    }
}

/// FNV-1a fast hash for token IDs
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
/// Converts "0.67" to 670000 (6 decimal places)
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

    // Normalize to exactly 6 decimal places
    while fraction_digits < 6 {
        val *= 10;
        fraction_digits += 1;
    }

    val
}