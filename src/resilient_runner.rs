//! Resilient WebSocket runner with TCP timeout and auto-reconnect
//! Prevents silent half-open TCP connection death

use std::time::Duration;
use std::net::TcpStream;
use tungstenite::{connect, stream::MaybeTlsStream};
use std::sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::collections::HashMap;
use crossbeam_channel::bounded;

use crate::hft_hot_path::{run_sync_hot_path, RolloverCommand, BackgroundTask, LogEvent};
use crate::websocket_reader::WebSocketReader;
use crate::market_rollover::run_rollover_thread;
use crate::jsonl_logger::JsonlLogger;

/// Start resilient hot path with auto-reconnect on WebSocket failure
pub fn start_resilient_hot_path(
    all_tokens: Vec<String>,
    killswitch: Arc<AtomicBool>,
    edge_counter: Arc<AtomicU64>,
) {
    let mut token_pairs: HashMap<u64, (u64, u64)> = HashMap::new();
    
    // Outer recovery loop - reconnects on any failure
    loop {
        if killswitch.load(Ordering::Relaxed) {
            println!("🛑 Killswitch triggered, exiting resilient runner");
            break;
        }
        
        println!("🔄 Attempting to connect to Polymarket WS...");
        
        // Connect to WebSocket
        let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
        let (ws_stream, _) = match connect(ws_url) {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!("❌ Connection failed: {}. Retrying in 2s...", e);
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        
        // 🚨 CRITICAL FIX: Set TCP read timeout to prevent silent half-open hang
        let tcp_stream = match ws_stream.get_mut() {
            MaybeTlsStream::Plain(s) => s,
            MaybeTlsStream::Rustls(s) => s.sock.get_mut(),
            _ => {
                eprintln!("⚠️ Unsupported TLS stream type, continuing without timeout");
                match ws_stream.get_mut() {
                    MaybeTlsStream::Plain(s) => s,
                    MaybeTlsStream::Rustls(s) => s.sock.get_mut(),
                    _ => panic!("Cannot extract TCP stream"),
                }
            }
        };
        
        // Set 30-second read timeout - if no data in 30s, connection is dead
        if let Err(e) = tcp_stream.set_read_timeout(Some(Duration::from_secs(30))) {
            eprintln!("⚠️ Failed to set read timeout: {}", e);
        } else {
            println!("✅ TCP read timeout set to 30 seconds");
        }
        
        // Create channels for this connection instance
        let (rollover_tx, rollover_rx) = bounded::<RolloverCommand>(64);
        let (background_tx, _) = bounded::<BackgroundTask>(1024);
        let (log_tx, log_rx) = bounded::<LogEvent>(4096);
        let _logger_handle = JsonlLogger::start(log_rx);
        
        // Start rollover thread
        let rollover_client = Arc::new(reqwest::blocking::Client::new());
        let rollover_tx_clone = rollover_tx.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                run_rollover_thread(rollover_client, rollover_tx_clone).await;
            });
        });
        
        println!("✅ WebSocket connected, starting hot path...");
        
        // Wrap in WebSocketReader
        let ws_reader = WebSocketReader::new(ws_stream);
        
        // Run hot path - will return on timeout or error
        let result = run_sync_hot_path(
            ws_reader,
            background_tx,
            all_tokens.clone(),
            killswitch.clone(),
            token_pairs.clone(),
            edge_counter.clone(),
            rollover_rx,
            log_tx.clone(),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        );
        
        // Hot path exited - connection died or timeout
        eprintln!("⚠️ Hot path exited. Reconnecting...");
        std::thread::sleep(Duration::from_secs(1));
    }
}
