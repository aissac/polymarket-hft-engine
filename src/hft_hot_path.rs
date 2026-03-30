//! HFT Hot Path - Fixed with price_changes array parsing
//!
//! WebSocket message formats:
//! 1. Initial dump: {"event_type":"book","asset_id":"...","bids":[...],"asks":[...]}
//! 2. Updates: {"price_changes":[{"asset_id":"...","side":"BUY/SELL","price":"...","size":"..."}]}

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::io::Read;
use std::time::Instant;
use memchr::memchr;
use memchr::memmem;
use crossbeam_channel::Sender;

use crate::state::{TokenBookState, OpportunitySnapshot, fast_hash, parse_fixed_6};

/// Edge detection constants
const EDGE_THRESHOLD_U64: u64 = 980_000;    // $0.98 for DRY_RUN validation
const MIN_VALID_COMBINED_U64: u64 = 900_000;  // $0.90 minimum
const MAX_POSITION_U64: u64 = 5_000_000;      // $5 max position

/// Background task to send to background thread
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

/// Run the hot path with correct price_changes array parsing
pub fn run_sync_hot_path<R: Read>(
    mut ws_stream: R,
    opportunity_tx: Sender<BackgroundTask>,
    all_tokens: Vec<String>,
    killswitch: Arc<AtomicBool>,
    token_pairs: HashMap<u64, u64>,
    edge_counter: Arc<AtomicU64>,
) {
    // Orderbook: token_hash -> TokenBookState
    let mut orderbook: HashMap<u64, TokenBookState> = HashMap::new();
    
    // Pre-populate with known tokens
    for token in &all_tokens {
        orderbook.entry(fast_hash(token.as_bytes()))
            .or_insert_with(TokenBookState::new);
    }
    
    // Patterns for memchr
    let asset_pattern = memmem::Finder::new(b"\"asset_id\":\"");
    let price_changes_pattern = memmem::Finder::new(b"\"price_changes\"");
    let bids_pattern = memmem::Finder::new(b"\"bids\"");
    let asks_pattern = memmem::Finder::new(b"\"asks\"");
    let side_pattern = memmem::Finder::new(b"\"side\":\"");
    let price_pattern = memmem::Finder::new(b"\"price\":\"");
    let size_pattern = memmem::Finder::new(b"\"size\":\"");
    
    let mut buffer = vec![0u8; 1024 * 1024];  // 1MB buffer
    let mut total_bytes = 0;
    let mut messages = 0u64;
    let start = Instant::now();
    let mut debug_printed = false;
    
    println!("[HFT] 🔥 Starting hot path with price_changes parsing...");
    println!("[HFT] Token pairs: {} pairs", token_pairs.len());
    
    loop {
        if killswitch.load(Ordering::Relaxed) {
            println!("[HFT] Killswitch triggered, shutting down");
            break;
        }
        
        // Read from WebSocket
        let n = match ws_stream.read(&mut buffer[total_bytes..]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        total_bytes += n;
        
        let bytes = &buffer[..total_bytes];
        
        // Determine message type: book snapshot or price_changes
        let is_book = bids_pattern.find(bytes).is_some() && asks_pattern.find(bytes).is_some();
        let is_price_changes = price_changes_pattern.find(bytes).is_some();
        
        // Parse all tokens in this message
        let mut search_start = 0;
        let mut tokens_parsed = 0;
        
        while let Some(asset_idx) = asset_pattern.find(&bytes[search_start..]) {
            let token_start = search_start + asset_idx + 12;
            
            if let Some(token_end) = memchr(b'"', &bytes[token_start..]) {
                let token_bytes = &bytes[token_start..token_start + token_end];
                let token_hash = fast_hash(token_bytes);
                
                // Find price
                let price_search_start = token_start + token_end + 1;
                if let Some(price_idx) = price_pattern.find(&bytes[price_search_start..]) {
                    let price_val_start = price_search_start + price_idx + 9;
                    
                    if let Some(price_end) = memchr(b'"', &bytes[price_val_start..]) {
                        let price = parse_fixed_6(&bytes[price_val_start..price_val_start + price_end]);
                        
                        // Find size
                        let size_search_start = price_val_start + price_end + 1;
                        if let Some(size_idx) = size_pattern.find(&bytes[size_search_start..]) {
                            let size_start = size_search_start + size_idx + 8;
                            
                            if let Some(size_end) = memchr(b'"', &bytes[size_start..]) {
                                let size = parse_fixed_6(&bytes[size_start..size_start + size_end]);
                                
                                // Determine side based on message type
                                println!("[DEBUG] is_book={} is_price_changes={}", is_book, is_price_changes);
                                    let is_bid = if is_book {
                                    // For book snapshots, check if we're in bids or asks section
                                    let bids_idx = bids_pattern.find(bytes).unwrap_or(usize::MAX);
                                    let asks_idx = asks_pattern.find(bytes).unwrap_or(usize::MAX);
                                    let current_pos = search_start + asset_idx;
                                    if bids_idx < asks_idx {
                                        current_pos > bids_idx && current_pos < asks_idx
                                    } else {
                                        current_pos > bids_idx
                                    }
                                } else if is_price_changes {
                                    // For price_changes, look for "side":"BUY" or "side":"SELL"
                                    let side_search_start = token_start + token_end + 1;
                                    if let Some(side_idx) = side_pattern.find(&bytes[side_search_start..]) {
                                        let side_val_start = side_search_start + side_idx + 9;
                                        if let Some(side_end) = memchr(b'"', &bytes[side_val_start..]) {
                                            let side_bytes = &bytes[side_val_start..side_val_start + side_end];
                                            println!("[DEBUG] side_bytes={:?}", std::str::from_utf8(side_bytes));
                                            side_bytes == b"BUY" || side_bytes == b"buy"
                                        } else { false }
                                    } else { false }
                                } else {
                                    false
                                };
                                
                                // Update orderbook based on side
                                // buy/BUY = BID (someone wants to BUY at this price)
                                // sell/SELL = ASK (someone wants to SELL at this price)
                                if let Some(state) = orderbook.get_mut(&token_hash) {
                                    if is_bid {
                                        state.update_bid(price, size);
                                    } else {
                                        state.update_ask(price, size);
                                    }
                                    tokens_parsed += 1;
                                        if tokens_parsed <= 10 {
                                            println!("[DEBUG] token={} is_bid={} price={:.4} size={}", token_hash, is_bid, price as f64 / 1_000_000.0, size);
                                        }
                                }
                                
                                // Check for edge after updating both YES and NO
                                if let Some(&complement_hash) = token_pairs.get(&token_hash) {
                                    if let (Some(yes_state), Some(no_state)) = 
                                        (orderbook.get(&token_hash), orderbook.get(&complement_hash)) {
                                        
                                        // Get best ask prices
                                        if let (Some((yes_ask_price, yes_ask_size)), 
                                                Some((no_ask_price, no_ask_size))) = 
                                            (yes_state.get_best_ask(), no_state.get_best_ask()) {
                                            
                                            // Calculate TRUE Combined ASK
                                            let combined_ask = yes_ask_price * 10_000 + no_ask_price * 10_000;
                                            
                                            // Edge detection: Combined ASK must be below threshold
                                            if combined_ask <= EDGE_THRESHOLD_U64 
                                                && combined_ask >= MIN_VALID_COMBINED_U64 {
                                                
                                                let ec = edge_counter.fetch_add(1, Ordering::Relaxed);
                                                if ec < 10 || ec % 100 == 0 {
                                                    // Get bid prices for Maker strategy
                                                    let yes_bid = yes_state.get_best_bid()
                                                        .map(|(p, _)| p * 10_000)
                                                        .unwrap_or(0);
                                                    let no_bid = no_state.get_best_bid()
                                                        .map(|(p, _)| p * 10_000)
                                                        .unwrap_or(0);
                                                    
                                                    println!("[EDGE] 🎯 FOUND! Combined ASK = ${:.4}", 
                                                        combined_ask as f64 / 1_000_000.0);
                                                    println!("  YES Ask: ${:.4} (size: {}) | Bid: ${:.4}", 
                                                        yes_ask_price as f64 / 100.0,
                                                        yes_ask_size,
                                                        yes_bid as f64 / 1_000_000.0);
                                                    println!("  NO  Ask: ${:.4} (size: {}) | Bid: ${:.4}", 
                                                        no_ask_price as f64 / 100.0,
                                                        no_ask_size,
                                                        no_bid as f64 / 1_000_000.0);
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
            
            // Debug: Print best bid/ask for first token
            if let Some((hash, state)) = orderbook.iter().next() {
                if let Some((bid, bid_size)) = state.get_best_bid() {
                    if let Some((ask, ask_size)) = state.get_best_ask() {
                        println!("[HFT] First token {}: Bid=${:.2} (size: {}) | Ask=${:.2} (size: {})", 
                            hash, bid as f64 / 100.0, bid_size, ask as f64 / 100.0, ask_size);
                    }
                }
            }
        }
    }
    
    let elapsed = start.elapsed();
    println!("[HFT] Processed {} messages in {:?}", messages, elapsed);
}