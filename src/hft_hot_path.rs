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

use crate::state::{TokenBookState, fast_hash, parse_fixed_6};
use crate::websocket_reader::WebSocketReader;
use crate::market_rollover::{build_subscribe_message, build_unsubscribe_message};

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
/// 
/// Uses WebSocketReader directly (not generic R: Read) to allow
/// sending subscription messages without blocking.
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
        orderbook.entry(fast_hash(token.as_bytes()))
            .or_insert_with(TokenBookState::new);
    }
    
    let asset_pattern = memmem::Finder::new(b"\"asset_id\":\"");
    let price_changes_pattern = memmem::Finder::new(b"\"price_changes\"");
    let bids_pattern = memmem::Finder::new(b"\"bids\"");
    let asks_pattern = memmem::Finder::new(b"\"asks\"");
    let side_pattern = memmem::Finder::new(b"\"side\":\"");
    let price_pattern = memmem::Finder::new(b"\"price\":\"");
    let size_pattern = memmem::Finder::new(b"\"size\":\"");
    
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut total_bytes = 0;
    let mut messages = 0u64;
    let start = Instant::now();
    let mut debug_printed = false;
    let mut last_rollover_check = Instant::now();
    
    println!("[HFT] 🔥 Starting hot path with rollover support");
    println!("[HFT] Token pairs: {} pairs", token_pairs.len());
    println!("[HFT] 🔄 Rollover channel active");
    
    loop {
        if killswitch.load(Ordering::Relaxed) {
            println!("[HFT] Killswitch triggered, shutting down");
            break;
        }
        
        // Check for rollover commands (non-blocking)
        if last_rollover_check.elapsed().as_secs() >= 1 {
            loop {
                match rollover_rx.try_recv() {
                    Ok(RolloverCommand::Subscribe { tokens }) => {
                        if !tokens.is_empty() {
                            let msg = build_subscribe_message(&tokens);
                            match ws_stream.send(msg) {
                                Ok(_) => {
                                    println!("[ROLLOVER] 📡 Subscribed to {} tokens", tokens.len());
                                    // Add to orderbook
                                    for token in &tokens {
                                        orderbook.entry(fast_hash(token.as_bytes()))
                                            .or_insert_with(TokenBookState::new);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[ROLLOVER] ❌ Subscribe failed: {}", e);
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
                                    // Remove from orderbook to free memory
                                    for token in &tokens {
                                        orderbook.remove(&fast_hash(token.as_bytes()));
                                    }
                                }
                                Err(e) => {
                                    eprintln!("[ROLLOVER] ❌ Unsubscribe failed: {}", e);
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
        
        let is_book = bids_pattern.find(bytes).is_some() && asks_pattern.find(bytes).is_some();
        let is_price_changes = price_changes_pattern.find(bytes).is_some();
        
        let mut search_start = 0;
        
        while let Some(asset_idx) = asset_pattern.find(&bytes[search_start..]) {
            let token_start = search_start + asset_idx + 12;
            
            if let Some(token_end) = memchr(b'"', &bytes[token_start..]) {
                let token_bytes = &bytes[token_start..token_start + token_end];
                let token_hash = fast_hash(token_bytes);
                
                let price_search_start = token_start + token_end + 1;
                if let Some(price_idx) = price_pattern.find(&bytes[price_search_start..]) {
                    let price_val_start = price_search_start + price_idx + 9;
                    
                    if let Some(price_end) = memchr(b'"', &bytes[price_val_start..]) {
                        let price = parse_fixed_6(&bytes[price_val_start..price_val_start + price_end]);
                        
                        let size_search_start = price_val_start + price_end + 1;
                        if let Some(size_idx) = size_pattern.find(&bytes[size_search_start..]) {
                            let size_start = size_search_start + size_idx + 8;
                            
                            if let Some(size_end) = memchr(b'"', &bytes[size_start..]) {
                                let size = parse_fixed_6(&bytes[size_start..size_start + size_end]);
                                
                                let is_bid = if is_book {
                                    let bids_idx = bids_pattern.find(bytes).unwrap_or(usize::MAX);
                                    let asks_idx = asks_pattern.find(bytes).unwrap_or(usize::MAX);
                                    let current_pos = search_start + asset_idx;
                                    if bids_idx < asks_idx {
                                        current_pos > bids_idx && current_pos < asks_idx
                                    } else {
                                        current_pos > bids_idx
                                    }
                                } else if is_price_changes {
                                    let side_search_start = token_start + token_end + 1;
                                    if let Some(side_idx) = side_pattern.find(&bytes[side_search_start..]) {
                                        let side_val_start = side_search_start + side_idx + 8;
                                        if let Some(side_end) = memchr(b'"', &bytes[side_val_start..]) {
                                            let side_bytes = &bytes[side_val_start..side_val_start + side_end];
                                            side_bytes == b"BUY" || side_bytes == b"buy"
                                        } else { false }
                                    } else { false }
                                } else {
                                    false
                                };
                                
                                if let Some(state) = orderbook.get_mut(&token_hash) {
                                    if is_bid {
                                        state.update_bid(price, size);
                                    } else {
                                        state.update_ask(price, size);
                                    }
                                }
                                
                                if let Some(&complement_hash) = token_pairs.get(&token_hash) {
                                    if let (Some(yes_state), Some(no_state)) = 
                                        (orderbook.get(&token_hash), orderbook.get(&complement_hash)) {
                                        
                                        if let (Some((yes_ask_price, yes_ask_size)), 
                                                Some((no_ask_price, no_ask_size))) = 
                                            (yes_state.get_best_ask(), no_state.get_best_ask()) {
                                            
                                            let combined_ask = yes_ask_price * 10_000 + no_ask_price * 10_000;
                                            
                                            if combined_ask <= EDGE_THRESHOLD_U64 
                                                && combined_ask >= MIN_VALID_COMBINED_U64 {
                                                
                                                let ec = edge_counter.fetch_add(1, Ordering::Relaxed);
                                                if ec < 10 || ec % 100 == 0 {
                                                    println!("[EDGE] 🎯 Combined ASK = ${:.4}", 
                                                        combined_ask as f64 / 1_000_000.0);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                
                search_start = token_start + 1;
            } else {
                break;
            }
        }
        
        messages += 1;
        total_bytes = 0;
        
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