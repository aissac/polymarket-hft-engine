//! HFT Hot Path - FIXED VERSION with Bid/Ask Tracking
//!
//! CRITICAL FIX: Track both sides of the orderbook
//! - best_bid: Highest BUY order price
//! - best_ask: Lowest SELL order price
//! - Combined ASK = YES Ask + NO Ask (what we PAY to arbitrage)

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use crossbeam_channel::Sender;
use memchr::memchr;
use memchr::memmem;

/// Token orderbook state - tracks both bid and ask
#[derive(Clone, Debug)]
pub struct TokenBookState {
    pub best_bid_price: u64,  // Highest buy price (fixed-point * 1,000,000)
    pub best_bid_size: u64,    // Size at best bid
    pub best_ask_price: u64,   // Lowest sell price (fixed-point * 1,000,000)
    pub best_ask_size: u64,    // Size at best ask
}

impl Default for TokenBookState {
    fn default() -> Self {
        Self {
            best_bid_price: 0,
            best_bid_size: 0,
            best_ask_price: u64::MAX,  // Start high so first ask becomes best
            best_ask_size: 0,
        }
    }
}

/// Fast hash for token IDs
pub fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Parse fixed-point price (multiply by 1,000,000)
fn parse_fixed_6(bytes: &[u8]) -> u64 {
    let mut result: u64 = 0;
    let mut decimal_seen = false;
    let mut decimal_places = 0;
    
    for &b in bytes {
        if b == b'.' {
            decimal_seen = true;
        } else if b >= b'0' && b <= b'9' {
            let digit = (b - b'0') as u64;
            result = result * 10 + digit;
            if decimal_seen {
                decimal_places += 1;
            }
        }
    }
    
    // Adjust for decimal places (we want 6 decimal places)
    while decimal_places < 6 {
        result *= 10;
        decimal_places += 1;
    }
    while decimal_places > 6 {
        result /= 10;
        decimal_places -= 1;
    }
    
    result
}

/// Opportunity to send to background thread
pub struct OpportunitySnapshot {
    pub yes_token: u64,
    pub no_token: u64,
    pub yes_bid: u64,
    pub yes_ask: u64,
    pub no_bid: u64,
    pub no_ask: u64,
    pub combined_ask: u64,  // YES Ask + NO Ask
    pub timestamp_nanos: u64,
}

/// Edge threshold (Combined ASK must be below this)
const EDGE_THRESHOLD_U64: u64 = 980_000;  // $0.98 for DRY_RUN validation
const MIN_VALID_COMBINED_U64: u64 = 900_000;  // $0.90 minimum

/// Run the hot path - parses WebSocket messages and detects edges
pub fn run_sync_hot_path(
    opportunity_tx: Sender<OpportunitySnapshot>,
    all_tokens: Vec<String>,
    killswitch: Arc<AtomicBool>,
    token_pairs: HashMap<u64, u64>,  // YES token hash -> NO token hash
) {
    let mut orderbook: HashMap<u64, TokenBookState> = HashMap::new();
    
    // Pre-populate orderbook with token hashes
    for token in &all_tokens {
        let hash = fast_hash(token.as_bytes());
        orderbook.entry(hash).or_default();
    }
    
    println!("[HFT] 🔥 Starting hot path with BID/ASK tracking...");
    
    // Pattern matching for memchr
    let asset_pattern = memmem::Finder::new(b"\"asset_id\":\"");
    let bids_pattern = memmem::Finder::new(b"\"bids\":[[");
    let asks_pattern = memmem::Finder::new(b"\"asks\":[[");
    let price_pattern = memmem::Finder::new(b"\"price\":\"");
    let size_pattern = memmem::Finder::new(b"\"size\":\"");
    let close_bracket = b']';
    
    // Read from WebSocket (simplified - actual implementation reads from socket)
    let mut buffer = vec![0u8; 1024 * 1024];  // 1MB buffer
    
    while !killswitch.load(Ordering::Relaxed) {
        // In actual implementation, read WebSocket message into buffer
        // For now, this is a placeholder
        
        let bytes = &buffer[..];
        let len = bytes.len();
        
        // Determine if we're in bids or asks section
        let bids_start = bids_pattern.find(bytes);
        let asks_start = asks_pattern.find(bytes);
        
        // Parse tokens
        let mut search_start = 0;
        let mut is_bid = false;
        let mut is_ask = false;
        
        while let Some(asset_idx) = asset_pattern.find(&bytes[search_start..]) {
            let token_start = search_start + asset_idx + 12;
            
            if let Some(token_end) = memchr(b'"', &bytes[token_start..]) {
                let token_bytes = &bytes[token_start..token_start + token_end];
                let token_hash = fast_hash(token_bytes);
                
                // Determine side based on position
                if let Some(bid_pos) = bids_start {
                    if let Some(ask_pos) = asks_start {
                        let current_pos = search_start + asset_idx;
                        is_bid = current_pos > bid_pos && current_pos < ask_pos;
                        is_ask = current_pos > ask_pos;
                    }
                }
                
                // Find price
                let price_search_start = token_start;
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
                                
                                // Update orderbook based on side
                                orderbook.entry(token_hash)
                                    .and_modify(|state| {
                                        if is_bid && size > 0 {
                                            // BID: Highest buy price wins
                                            if state.best_bid_price == 0 || price > state.best_bid_price {
                                                state.best_bid_price = price;
                                                state.best_bid_size = size;
                                            }
                                        } else if is_ask && size > 0 {
                                            // ASK: Lowest sell price wins
                                            if price < state.best_ask_price || state.best_ask_price == u64::MAX {
                                                state.best_ask_price = price;
                                                state.best_ask_size = size;
                                            }
                                        }
                                    })
                                    .or_insert_with(|| {
                                        let mut state = TokenBookState::default();
                                        if is_bid && size > 0 {
                                            state.best_bid_price = price;
                                            state.best_bid_size = size;
                                        } else if is_ask && size > 0 {
                                            state.best_ask_price = price;
                                            state.best_ask_size = size;
                                        }
                                        state
                                    });
                            }
                        }
                    }
                }
                
                // Check for edge after parsing each token
                if let Some(&no_hash) = token_pairs.get(&token_hash) {
                    if let (Some(yes_state), Some(no_state)) = 
                        (orderbook.get(&token_hash), orderbook.get(&no_hash)) {
                        
                        // Calculate TRUE Combined ASK
                        let combined_ask = yes_state.best_ask_price + no_state.best_ask_price;
                        
                        // Check edge threshold
                        if combined_ask <= EDGE_THRESHOLD_U64 && combined_ask >= MIN_VALID_COMBINED_U64 {
                            // Send to background thread
                            let _ = opportunity_tx.send(OpportunitySnapshot {
                                yes_token: token_hash,
                                no_token: no_hash,
                                yes_bid: yes_state.best_bid_price,
                                yes_ask: yes_state.best_ask_price,
                                no_bid: no_state.best_bid_price,
                                no_ask: no_state.best_ask_price,
                                combined_ask,
                                timestamp_nanos: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_nanos() as u64,
                            });
                            
                            println!("[EDGE] 🎯 FOUND! Combined ASK = ${:.4}", 
                                combined_ask as f64 / 1_000_000.0);
                        }
                    }
                }
                
                search_start = token_start + 1;
            }
        }
    }
    
    println!("[HFT] Hot path terminated");
}