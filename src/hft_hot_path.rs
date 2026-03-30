//! HFT Hot Path - With Rollover Support
//!
//! Sub-microsecond orderbook parsing with dynamic market subscriptions

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::io::Read;
use std::time::Instant;
use memchr::memchr;
use memchr::memmem;
use crossbeam_channel::{Sender, Receiver, TryRecvError};

use crate::state::{TokenBookState, parse_fixed_6};
use crate::websocket_reader::WebSocketReader;
use crate::market_rollover::{build_subscribe_message, build_unsubscribe_message};

/// Hash token bytes consistently with hash_token (convert to str first)
#[inline]
fn fast_hash_token(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    let token_str = std::str::from_utf8(bytes).unwrap_or("");
    token_str.hash(&mut hasher);
    hasher.finish()
}

const EDGE_THRESHOLD_U64: u64 = 980_000;
const MIN_VALID_COMBINED_U64: u64 = 900_000;
const MAX_POSITION_U64: u64 = 5_000_000;

/// Task sent to background execution thread
pub enum BackgroundTask {
    EdgeDetected {
        yes_token_hash: u64,
        no_token_hash: u64,
        yes_best_bid: u64,
        yes_best_ask: u64,
        yes_ask_size: u64,
        no_best_bid: u64,
        no_best_ask: u64,
        no_ask_size: u64,
        combined_ask: u64,
        timestamp_nanos: u64,
    },
    LatencyStats {
        min_ns: u64,
        max_ns: u64,
        avg_ns: u64,
        p99_ns: u64,
        sample_count: u64,
    },
}

/// Rollover command from background thread
pub enum RolloverCommand {
    Subscribe { tokens: Vec<String> },
    Unsubscribe { tokens: Vec<String> },
}

/// Run the hot path with rollover support
pub fn run_sync_hot_path(
    mut ws_stream: WebSocketReader,
    opportunity_tx: Sender<BackgroundTask>,
    all_tokens: Vec<String>,
    killswitch: Arc<AtomicBool>,
    token_pairs: HashMap<u64, u64>,
    edge_counter: Arc<AtomicU64>,
    rollover_rx: Receiver<RolloverCommand>,
) {
    let mut orderbook: HashMap<u64, TokenBookState> = HashMap::new();
    
    for token in &all_tokens {
        orderbook.entry(fast_hash_token(token.as_bytes()))
            .or_insert_with(TokenBookState::new);
    }
    
    let price_pattern = memmem::Finder::new(b"\"price\":\"");
    let size_pattern = memmem::Finder::new(b"\"size\":\"");
    let asset_pattern = memmem::Finder::new(b"\"asset_id\":\"");
    let side_pattern = memmem::Finder::new(b"\"side\":\"");
    let bids_start_pattern = memmem::Finder::new(b"\"bids\":[");
    let asks_start_pattern = memmem::Finder::new(b"\"asks\":[");
    
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total_bytes = 0;
    let mut messages = 0u64;
    let mut pair_checks = 0u64;
    let start = Instant::now();
    let mut debug_printed = false;
    let mut last_rollover_check = Instant::now();
    
    println!("[HFT] 🔥 Starting hot path with rollover support");
    println!("[HFT] Token pairs: {} pairs", token_pairs.len());
    println!("[HFT] 🔄 Rollover channel active");
    
    // Debug: print ALL token hashes we expect
    println!("[HFT] Expected token hashes:");
    for (hash, token) in token_pairs.iter() {
        println!("[HFT]   hash={:x} -> complement={:x}", hash, token);
    }

    loop {
        if killswitch.load(Ordering::Relaxed) {
            println!("[HFT] Killswitch triggered, exiting");
            break;
        }
        
        // Check for rollover commands
        if last_rollover_check.elapsed().as_millis() > 100 {
            loop {
                match rollover_rx.try_recv() {
                    Ok(RolloverCommand::Subscribe { tokens }) => {
                        if !tokens.is_empty() {
                            let msg = build_subscribe_message(&tokens);
                            match ws_stream.send(msg) {
                                Ok(_) => {
                                    println!("[ROLLOVER] 📡 Subscribed to {} tokens", tokens.len());
                                    for token in &tokens {
                                        orderbook.entry(fast_hash_token(token.as_bytes()))
                                            .or_insert_with(TokenBookState::new);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[ROLLOVER] ⚠️ Subscribe failed: {}", e);
                                }
                            }
                        }
                    }
                    Ok(RolloverCommand::Unsubscribe { tokens }) => {
                        if !tokens.is_empty() {
                            let msg = build_unsubscribe_message(&tokens);
                            match ws_stream.send(msg) {
                                Ok(_) => {
                                    println!("[ROLLOVER] 🗑️ Unsubscribed from {} tokens", tokens.len());
                                    for token in &tokens {
                                        orderbook.remove(&fast_hash_token(token.as_bytes()));
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[ROLLOVER] ⚠️ Unsubscribe failed: {}", e);
                                }
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        eprintln!("[ROLLOVER] ⚠️ Channel disconnected");
                        break;
                    }
                }
            }
            last_rollover_check = Instant::now();
        }
        
        // Read WebSocket message
        let n = match ws_stream.read(&mut buffer[total_bytes..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        total_bytes += n;
        
        let bytes = &buffer[..total_bytes];
        
        // Stateful parsing for batched WebSocket messages
        // Message structure: [{"asset_id":"...", "bids":[{price,size}...], "asks":[...]}, {...}]
        let mut current_token_hash: Option<u64> = None;
        let mut is_bid = false;
        let mut in_array = false;
        let mut parse_debug_count = 0u64;
        
        let mut pos = 0;
        while pos < bytes.len() {
            let remaining = &bytes[pos..];
            
            // Check for asset_id at TOP LEVEL (not inside bids/asks)
            if remaining.starts_with(b"\"asset_id\":\"") && !in_array {
                let token_start = pos + 12;
                if let Some(token_end) = memchr(b'"', &bytes[token_start..]) {
                    current_token_hash = Some(fast_hash_token(&bytes[token_start..token_start + token_end]));
                    if messages <= 3 && parse_debug_count < 5 {
                        parse_debug_count += 1;
                        println!("[PARSE] Found asset_id, hash={:x}", current_token_hash.unwrap());
                    }
                    pos = token_start + token_end + 1;
                    continue;
                }
            }
            
            // Check for bids array start
            if remaining.starts_with(b"\"bids\":[") {
                is_bid = true;
                in_array = true;
                pos += 9;
                continue;
            }
            
            // Check for asks array start
            if remaining.starts_with(b"\"asks\":[") {
                is_bid = false;
                in_array = true;
                pos += 9;
                continue;
            }
            
            // Check for end of arrays
            if remaining.starts_with(b"]") && in_array {
                in_array = false;
                pos += 1;
                continue;
            }
            
            // Check for price (only inside arrays)
            if in_array && remaining.starts_with(b"\"price\":\"") {
                let price_start = pos + 9;
                if let Some(price_end) = memchr(b'"', &bytes[price_start..]) {
                    let price = parse_fixed_6(&bytes[price_start..price_start + price_end]);
                    
                    // Find size after price
                    let size_search = price_start + price_end + 1;
                    if let Some(size_idx) = size_pattern.find(&bytes[size_search..]) {
                        let size_start_inner = size_search + size_idx + 8;
                        if let Some(size_end) = memchr(b'"', &bytes[size_start_inner..]) {
                            let size = parse_fixed_6(&bytes[size_start_inner..size_start_inner + size_end]);
                            
                            // Apply to current token
                            if let Some(token_hash) = current_token_hash {
                                if let Some(state) = orderbook.get_mut(&token_hash) {
                                    if is_bid {
                                        state.update_bid(price, size);
                                        if messages <= 3 && parse_debug_count < 10 {
                                            println!("[BID] token={:x} price={:.2}¢ size={:.2}", 
                                                token_hash, price as f64 / 10_000.0, size as f64 / 1_000_000.0);
                                        }
                                    } else {
                                        state.update_ask(price, size);
                                        if messages <= 3 && parse_debug_count < 10 {
                                            println!("[ASK] token={:x} price={:.2}¢ size={:.2}", 
                                                token_hash, price as f64 / 10_000.0, size as f64 / 1_000_000.0);
                                        }
                                    }
                                }
                            }
                            pos = size_start_inner + size_end + 1;
                            continue;
                        }
                    }
                    pos = price_start + price_end + 1;
                    continue;
                }
            }
            
            pos += 1;
        }
        
        // Edge detection: check complement pairs
        for (&token_hash, &complement_hash) in token_pairs.iter() {
            if let (Some(yes_state), Some(no_state)) = 
                (orderbook.get(&token_hash), orderbook.get(&complement_hash)) {
                
                if let (Some((yes_ask_price, yes_ask_size)), 
                        Some((no_ask_price, no_ask_size))) = 
                    (yes_state.get_best_ask(), no_state.get_best_ask()) {
                    
                    let combined_ask = yes_ask_price * 10_000 + no_ask_price * 10_000;
                    
                    pair_checks += 1;
                    if pair_checks <= 10 || pair_checks % 100 == 0 {
                        println!("[DEBUG] Combined ASK = ${:.4} (YES={:.2}¢, NO={:.2}¢) | pair_checks={}", 
                            combined_ask as f64 / 1_000_000.0,
                            yes_ask_price as f64 / 10_000.0,
                            no_ask_price as f64 / 10_000.0,
                            pair_checks);
                    }
                    
                    if combined_ask <= EDGE_THRESHOLD_U64 && combined_ask >= MIN_VALID_COMBINED_U64 {
                        let ec = edge_counter.fetch_add(1, Ordering::Relaxed);
                        if ec < 10 || ec % 100 == 0 {
                            println!("[EDGE] 🎯 Combined ASK = ${:.4}", combined_ask as f64 / 1_000_000.0);
                        }
                        
                        let capped_yes_size = std::cmp::min(yes_ask_size, MAX_POSITION_U64);
                        let capped_no_size = std::cmp::min(no_ask_size, MAX_POSITION_U64);
                        
                        let (yes_bid, no_bid) = (
                            yes_state.get_best_bid().map(|(p, _)| p).unwrap_or(0),
                            no_state.get_best_bid().map(|(p, _)| p).unwrap_or(0)
                        );
                        
                        let _ = opportunity_tx.try_send(BackgroundTask::EdgeDetected {
                            yes_token_hash: token_hash,
                            no_token_hash: complement_hash,
                            yes_best_bid: yes_bid,
                            yes_best_ask: yes_ask_price,
                            yes_ask_size: capped_yes_size,
                            no_best_bid: no_bid,
                            no_best_ask: no_ask_price,
                            no_ask_size: capped_no_size,
                            combined_ask,
                            timestamp_nanos: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_nanos() as u64,
                        });
                    }
                }
            }
        }
        
        messages += 1;
        total_bytes = 0;
        
        if messages % 100 == 0 {
            println!("[HFT] Received {} messages, pair_checks={}", messages, pair_checks);
        }
        
        if messages == 50 && !debug_printed {
            debug_printed = true;
            println!("[HFT] ✅ Warmed up after {} messages", messages);
            
            let mut tokens_with_both = 0;
            let mut tokens_with_bid_only = 0;
            let mut tokens_with_ask_only = 0;
            
            for state in orderbook.values() {
                let has_bid = state.get_best_bid().is_some();
                let has_ask = state.get_best_ask().is_some();
                
                if has_bid && has_ask {
                    tokens_with_both += 1;
                } else if has_bid {
                    tokens_with_bid_only += 1;
                } else if has_ask {
                    tokens_with_ask_only += 1;
                }
            }
            
            println!("[HFT] Orderbook state: {} tokens", orderbook.len());
            println!("[HFT]   {} with both bid+ask", tokens_with_both);
            println!("[HFT]   {} with bid only", tokens_with_bid_only);
            println!("[HFT]   {} with ask only", tokens_with_ask_only);
        }
    }
    
    let elapsed = start.elapsed();
    println!("[HFT] Processed {} messages in {:?}", messages, elapsed);
}