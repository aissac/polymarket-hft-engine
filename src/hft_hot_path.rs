//! HFT Hot Path - Rate Limited + Bi-directional Token Mapping

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, Duration};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use crossbeam_channel::{Sender, Receiver};
use memchr::memchr;
use rustc_hash::FxHashMap;
use tungstenite::Message;
use crate::websocket_reader::WebSocketReader;
use crate::jsonl_logger::LogEvent;

const EVAL_RATE_LIMIT_MS: u64 = 100;
const EDGE_THRESHOLD_U64: u64 = 980_000;
const MIN_VALID_COMBINED_U64: u64 = 900_000;
const TARGET_SHARES: u64 = 100;

pub enum RolloverCommand {
    AddPair {
        yes_hash: u64,
        no_hash: u64,
        ws_sub_payload: String,
    },
    RemovePair {
        yes_hash: u64,
        no_hash: u64,
        ws_unsub_payload: String,
    },
}

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

pub struct EvalTracker {
    last_eval: Instant,
}

impl EvalTracker {
    pub fn new() -> Self {
        Self {
            last_eval: Instant::now() - Duration::from_secs(1),
        }
    }

    pub fn can_evaluate(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_eval).as_millis() as u64 >= EVAL_RATE_LIMIT_MS {
            self.last_eval = now;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug)]
pub struct TokenBookState {
    pub best_bid_price: u64,
    pub best_bid_size: u64,
    pub best_ask_price: u64,
    pub best_ask_size: u64,
}

impl TokenBookState {
    pub fn new() -> Self {
        Self {
            best_bid_price: 0,
            best_bid_size: 0,
            best_ask_price: u64::MAX,
            best_ask_size: 0,
        }
    }

    pub fn update_bid(&mut self, price: u64, size: u64) {
        if price > self.best_bid_price {
            self.best_bid_price = price;
            self.best_bid_size = size;
        }
    }

    pub fn update_ask(&mut self, price: u64, size: u64) {
        if price < self.best_ask_price {
            self.best_ask_price = price;
            self.best_ask_size = size;
        }
    }

    pub fn get_best_bid(&self) -> Option<(u64, u64)> {
        if self.best_bid_price > 0 {
            Some((self.best_bid_price, self.best_bid_size))
        } else {
            None
        }
    }

    pub fn get_best_ask(&self) -> Option<(u64, u64)> {
        if self.best_ask_price < u64::MAX {
            Some((self.best_ask_price, self.best_ask_size))
        } else {
            None
        }
    }
}

pub fn fast_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn parse_fixed_6(bytes: &[u8]) -> u64 {
    let mut result: u64 = 0;
    let mut decimal_seen = false;
    let mut decimal_places = 0;

    for &b in bytes {
        if b == b'.' {
            decimal_seen = true;
            continue;
        }
        if b >= b'0' && b <= b'9' {
            result = result * 10 + (b - b'0') as u64;
            if decimal_seen {
                decimal_places += 1;
            }
        }
    }

    while decimal_places < 6 {
        result *= 10;
        decimal_places += 1;
    }

    result
}

pub fn run_sync_hot_path(
    mut ws_stream: WebSocketReader,
    opportunity_tx: Sender<BackgroundTask>,
    all_tokens: Vec<String>,
    killswitch: Arc<AtomicBool>,
    mut token_pairs: HashMap<u64, (u64, u64)>,  // Bi-directional: hash -> (yes_hash, no_hash)
    edge_counter: Arc<AtomicU64>,
    rollover_rx: Receiver<RolloverCommand>,
    log_tx: crossbeam_channel::Sender<LogEvent>,
    valid_evals: Arc<AtomicU64>,
    missing_data: Arc<AtomicU64>,
) {
    println!("⚡ Rate-Limited Hot Path Started (Bi-directional)");
    println!("📊 Tracking {} token mappings", token_pairs.len());

    let mut orderbook: FxHashMap<u64, TokenBookState> = FxHashMap::default();
    let mut eval_trackers: FxHashMap<u64, EvalTracker> = FxHashMap::default();
    
    for &token_hash in token_pairs.keys() {
        eval_trackers.insert(token_hash, EvalTracker::new());
    }

    for token in &all_tokens {
        let hash = fast_hash(token.as_bytes());
        orderbook.entry(hash).or_insert_with(TokenBookState::new);
    }

    let mut messages = 0u64;
    let mut total_evals = 0u64;
    let mut last_ping = Instant::now();
    const PING_INTERVAL: Duration = Duration::from_secs(20);
    let mut edges_found = 0u64;
    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut last_eval_count = 0u64;

    println!("⚡ Hot Path Armed. Waiting for WebSocket events...");

    loop {
        if killswitch.load(Ordering::Relaxed) {
            println!("⚡ Killswitch triggered, exiting hot path");
            break;
        }

        // Process rollover commands
        while let Ok(cmd) = rollover_rx.try_recv() {
            match cmd {
                RolloverCommand::AddPair { yes_hash, no_hash, ws_sub_payload } => {
                    println!("🟢 [HOT PATH] Dynamic WS Subscription: YES={} NO={}", yes_hash, no_hash);
                    
                    // Update bi-directional map
                    token_pairs.insert(yes_hash, (yes_hash, no_hash));
                    token_pairs.insert(no_hash, (yes_hash, no_hash));
                    eval_trackers.insert(yes_hash, EvalTracker::new());
                    eval_trackers.insert(no_hash, EvalTracker::new());
                    orderbook.entry(yes_hash).or_insert_with(TokenBookState::new);
                    orderbook.entry(no_hash).or_insert_with(TokenBookState::new);
                    
                    // Send WebSocket subscription (pre-formatted by rollover thread)
                    let _ = ws_stream.send(tungstenite::Message::Text(ws_sub_payload));
                }
                RolloverCommand::RemovePair { yes_hash, no_hash, ws_unsub_payload: _ } => {
                    println!("🔴 [HOT PATH] Removing: YES={} NO={}", yes_hash, no_hash);
                    token_pairs.remove(&yes_hash);
                    token_pairs.remove(&no_hash);
                    eval_trackers.remove(&yes_hash);
                    eval_trackers.remove(&no_hash);
                }
            }
        }

        // Send ping every 20 seconds to keep connection alive
            // Send ping every 20 seconds to keep connection alive
            if last_ping.elapsed() >= PING_INTERVAL {
                if let Err(e) = ws_stream.socket.send(Message::Ping(vec![])) {
                    eprintln!("Ping failed: {}", e);
                }
                last_ping = Instant::now();
            }
            
            let msg = match ws_stream.socket.read() {
            Ok(Message::Pong(_)) => {
                // Server responded to our ping - connection alive
                continue;
            }
            Ok(m) => m,
            
            // 30-second TCP timeout catcher (NotebookLM recommendation)
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                eprintln!("[TCP TIMEOUT] 30s of silence. Connection dead! Reconnecting...");
                break; // Break hot path loop, let caller reconnect
            },
            
            // Standard disconnects
            Err(e) => {
                eprintln!("WebSocket read error: {}", e);
                break;
            }
        };

        messages += 1;

        if let Message::Text(text) = msg {
            let bytes = text.as_bytes();
            parse_and_update_orderbook(bytes, &mut orderbook, &token_pairs);

            // Get all mappings to evaluate
            let pairs: Vec<(u64, u64, u64)> = token_pairs.iter()
                .map(|(&k, &(yes, no))| (k, yes, no))
                .collect();
            
            for (token_hash, yes_hash, no_hash) in pairs {
                let now = Instant::now();
                if let Some(tracker) = eval_trackers.get_mut(&token_hash) {
                    if !tracker.can_evaluate(now) {
                        continue;
                    }
                    total_evals += 1;
                }

                // Debug: check orderbook state every 100 evals
                if total_evals % 100 == 0 {
                    let yes_has = orderbook.get(&yes_hash).map(|s| s.best_ask_price < u64::MAX).unwrap_or(false);
                    let no_has = orderbook.get(&no_hash).map(|s| s.best_ask_price < u64::MAX).unwrap_or(false);
                    if yes_has || no_has {
                        println!("[EDGE DEBUG] Token {} | YES has data: {} | NO has data: {}", 
                            token_hash, yes_has, no_has);
                        if yes_has {
                            if let Some(s) = orderbook.get(&yes_hash) {
                                println!("  YES: bid={} ask={} size={}", s.best_bid_price, s.best_ask_price, s.best_ask_size);
                            }
                        }
                        if no_has {
                            if let Some(s) = orderbook.get(&no_hash) {
                                println!("  NO: bid={} ask={} size={}", s.best_bid_price, s.best_ask_price, s.best_ask_size);
                            }
                        }
                    }
                }
                
                if let (Some(yes_state), Some(no_state)) = 
                    (orderbook.get(&yes_hash), orderbook.get(&no_hash)) {
                    
                    if let (Some((yes_ask_price, yes_ask_size)), 
                            Some((no_ask_price, no_ask_size))) = 
                        (yes_state.get_best_ask(), no_state.get_best_ask()) {
                        
                        // Debug: show why edge detection fails
                        if total_evals % 50 == 0 {
                            let combined = yes_ask_price + no_ask_price;
                            let is_dust = combined < MIN_VALID_COMBINED_U64;
                            let _ = log_tx.try_send(LogEvent::EdgeCheck {
                                yes_hash,
                                no_hash,
                                combined_ask: combined,
                                dust: is_dust,
                            });
                        }
                        
                        // Sanity checks
                        if yes_ask_price == 0 || yes_ask_price >= 100_000_000 || 
                           no_ask_price == 0 || no_ask_price >= 100_000_000 {
                            continue;
                        }

                        if yes_ask_size < TARGET_SHARES || no_ask_size < TARGET_SHARES {
                            continue;
                        }

                        let combined_ask = yes_ask_price + no_ask_price;

                        if combined_ask <= EDGE_THRESHOLD_U64 && combined_ask >= MIN_VALID_COMBINED_U64 {
                            edges_found += 1;
                            edge_counter.fetch_add(1, Ordering::Relaxed);

                            println!("🎯 [EDGE] Combined ASK=${:.4} (YES=${} NO=${})", 
                                combined_ask as f64 / 1_000_000.0,
                                yes_ask_price as f64 / 1_000_000.0,
                                no_ask_price as f64 / 1_000_000.0);

                            let _ = opportunity_tx.try_send(BackgroundTask::EdgeDetected {
                                yes_token_hash: yes_hash,
                                no_token_hash: no_hash,
                                yes_best_bid: yes_state.best_bid_price,
                                yes_best_ask: yes_ask_price,
                                yes_ask_size: yes_ask_size,
                                no_best_bid: no_state.best_bid_price,
                                no_best_ask: no_ask_price,
                                no_ask_size: no_ask_size,
                                combined_ask,
                                timestamp_nanos: 0,
                            });
                        }
                    }
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(1) {
            let evals_this_sec = total_evals - last_eval_count;
            let _ = log_tx.try_send(LogEvent::Metric {
                uptime: start.elapsed().as_secs(),
                msgs: messages,
                evals: total_evals,
                edges: edges_found,
                evals_sec: evals_this_sec as u64,
                pairs: token_pairs.len() as u64,
                dropped: 0,
                valid_evals: 0,
                missing_data: 0,
            });
            last_eval_count = total_evals;
            last_report = Instant::now();
        }
    }

    let elapsed = start.elapsed();
    println!("[HFT] Processed {} messages in {:?}", messages, elapsed);
    println!("[HFT] Total evaluations: {} | Edges found: {}", total_evals, edges_found);
}

fn parse_and_update_orderbook(
    bytes: &[u8],
    orderbook: &mut FxHashMap<u64, TokenBookState>,
    token_pairs: &HashMap<u64, (u64, u64)>,
) {
    let mut current_token_hash: Option<u64> = None;
    let mut is_bid = false;
    let mut pos = 0;
    let mut found_asset = 0u64;
    let mut found_price = 0u64;

    let asset_marker: &[u8] = b"\"asset_id\":\"";
    let price_marker: &[u8] = b"\"price\":\"";
    let buy_marker: &[u8] = b"\"side\":\"BUY\"";
    let sell_marker: &[u8] = b"\"side\":\"SELL\"";

    while pos < bytes.len() {
        let remaining = &bytes[pos..];

        if remaining.starts_with(asset_marker) {
            let token_start = pos + asset_marker.len();
            if let Some(token_end) = memchr(b'"', &bytes[token_start..]) {
                current_token_hash = Some(fast_hash(&bytes[token_start..token_start + token_end]));
                found_asset += 1;
                pos = token_start + token_end + 1;
                continue;
            }
        }

        if remaining.starts_with(buy_marker) {
            is_bid = true;
        } else if remaining.starts_with(sell_marker) {
            is_bid = false;
        }

        if remaining.starts_with(price_marker) {
            let price_start = pos + price_marker.len();
            if let Some(price_end) = memchr(b'"', &bytes[price_start..]) {
                let price = parse_fixed_6(&bytes[price_start..price_start + price_end]);
                found_price += 1;
                
                if let Some(token_hash) = current_token_hash {
                    if let Some(state) = orderbook.get_mut(&token_hash) {
                        if is_bid {
                            state.update_bid(price, 100);
                        } else {
                            state.update_ask(price, 100);
                        }
                    }
                }
                pos = price_start + price_end + 1;
                continue;
            }
        }

        pos += 1;
    }
}
