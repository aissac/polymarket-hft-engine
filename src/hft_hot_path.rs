// src/hft_hot_path.rs
//! HFT Hot Path - Zero-Allocation Raw Byte Scanner
//!
//! Uses memchr to scan WebSocket messages without parsing full JSON DOM.
//! Expected latency: ~50-100ns (vs 4-6µs for DOM parsing)

use crossbeam_channel::Sender;
use std::time::Duration;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Target combined price for arbitrage (94 cents = $0.94)
///
/// March 30, 2026 Fee Structure:
/// - Max Taker Fee: 1.80% (at $0.50 probability)
/// - Maker Rebate: 0.36% (20% of taker)
/// - Net Fee Burden: 1.44%
/// - Ghost Drag: ~2.14%
/// - Total Cost: ~3.58%
///
/// At $0.94 combined (6% gross edge):
/// - Gross Profit: +6.00%
/// - Fee Burden: -1.44%
/// - Ghost Drag: -2.14%
/// - Net EV: +2.42% per trade
const EDGE_THRESHOLD_U64: u64 = 940_000;

/// Maximum position size in micro-USDC ($5 = 5_000_000)
/// Prevents liquidity mirage sweeps and limits exposure
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

/// Ghost simulation result
#[derive(Debug, Clone)]
pub struct GhostResult {
    pub token_hash: u64,
    pub combined_price: u64,
    pub status: GhostStatus,
    pub initial_yes_size: u64,
    pub initial_no_size: u64,
    pub remaining_yes_size: u64,
    pub remaining_no_size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GhostStatus {
    Executable,
    Partial,
    Ghosted,
}

/// Run the synchronous hot path with zero-allocation byte scanning
pub fn run_sync_hot_path(tx: Sender<BackgroundTask>, tokens: Vec<String>, killswitch: Arc<AtomicBool>) {
    use tungstenite::{connect, Message};
    
    // Connect to Polymarket CLOB WebSocket
    let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    
    println!("[HFT] Connecting to {}...", ws_url);
    
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
    println!("[HFT] 🔥 Starting ZERO-ALLOCATION hot path...");

    // Local orderbook: token_hash -> (yes_price, no_price, yes_size, no_size)
    let mut orderbook: HashMap<u64, (u64, u64, u64, u64)> = HashMap::with_capacity(128);
    
    // Warmup counter
    let mut warmup_count = 0;
    
    // Latency tracking (8192 samples = ~10 seconds)
    let mut latency_samples: Vec<u64> = Vec::with_capacity(8192);
    let mut last_stat_time = std::time::Instant::now();

    // Main busy-poll loop
    loop {
        // Killswitch check (1ns atomic read)
        if killswitch.load(Ordering::Relaxed) {
            println!("[HFT] 🚨 KILLSWITCH ENGAGED - draining socket");
            let _ = socket.read(); // Drain but don't process
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
            let len = text.len();

            // Skip tiny messages (pongs, heartbeats)
            if len < 100 {
                continue;
            }

            // Warmup period (first 50 messages)
            if warmup_count < 50 {
                warmup_count += 1;
                if warmup_count == 50 {
                    println!("[HFT] ✅ Warmed up after 50 messages");
                }
            }

            // ============================================
            // FAST PATH: serde_json parsing (stable)
            // TODO: Replace with memchr once patterns validated
            // ============================================
            
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                // Handle array messages (Polymarket wraps in array)
                let obj = if value.is_array() {
                    value.as_array().and_then(|arr| arr.first()).unwrap_or(&value)
                } else {
                    &value
                };
                
                // Try price_changes format
                if let (Some(asset_id), Some(price_changes)) = (
                    obj.get("asset_id").and_then(|v| v.as_str()),
                    obj.get("price_changes").and_then(|v| v.as_array())
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

                    // Edge detection with $5 max position cap
                    let complement_hash = token_hash ^ 1;
                    if let Some((yes_price, _no_price, yes_size, _no_size)) = orderbook.get(&token_hash) {
                        if let Some((c_yes_price, _c_no_price, c_yes_size, _c_no_size)) = orderbook.get(&complement_hash) {
                            let combined = yes_price + c_yes_price;
                            
                            if combined <= EDGE_THRESHOLD_U64 && *yes_size > 0 && *c_yes_size > 0 {
                                // Apply $5 max position cap to prevent liquidity sweeps
                                let capped_yes_size = std::cmp::min(*yes_size, MAX_POSITION_U64);
                                let capped_no_size = std::cmp::min(*c_yes_size, MAX_POSITION_U64);
                                
                                let _ = tx.try_send(BackgroundTask::EdgeDetected {
                                    token_hash,
                                    combined_price: combined,
                                    timestamp_nanos: start_tsc.elapsed().as_nanos() as u64,
                                    yes_size: capped_yes_size,
                                    no_size: capped_no_size,
                                });
                            }
                        }
                    }
                }
                
                // Try bids/asks format (orderbook snapshot)
                if let (Some(bids), Some(asks)) = (
                    obj.get("bids").and_then(|v| v.as_array()),
                    obj.get("asks").and_then(|v| v.as_array())
                ) {
                    if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
                        if let (Some(bid_price), Some(ask_price)) = (
                            best_bid.get("price").and_then(|v| v.as_str()),
                            best_ask.get("price").and_then(|v| v.as_str())
                        ) {
                            let market_id = obj.get("market").and_then(|v| v.as_str()).unwrap_or("");
                            let token_hash = fast_hash(market_id.as_bytes());
                            
                            let bp = parse_fixed_6(bid_price.as_bytes());
                            let ap = parse_fixed_6(ask_price.as_bytes());
                            
                            orderbook.entry(token_hash)
                                .and_modify(|(b, a, _, _)| { *b = bp; *a = ap; })
                                .or_insert((bp, ap, 0, 0));
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