//! Zero-allocation JSONL logging for HFT hot path
//! 
//! Hot path sends LogEvent enum over crossbeam channel (no heap allocation)
//! Background thread formats JSONL using itoa (no serde_json overhead)

use std::io::{Write, BufWriter};
use std::fs::File;
use crossbeam_channel::{bounded, Receiver, Sender};

/// Log events - primitives only, no strings (fits in ~32 bytes on stack)
#[derive(Debug, Clone, Copy)]
pub enum LogEvent {
    /// Metrics heartbeat (every 1 second)
    Metric {
        uptime: u64,
        msgs: u64,
        evals: u64,
        edges: u64,
        evals_sec: u64,
        pairs: u64,
        dropped: u64,
    },
    /// Rollover event
    Rollover {
        action: u8, // 0=add, 1=remove
        hash: u64,
        end_time: i64,
    },
    /// Orderbook update
    Orderbook {
        hash: u64,
        price: u64,
        size: u64,
        is_bid: bool,
    },
    /// Edge check (every 50th evaluation)
    EdgeCheck {
        yes_hash: u64,
        no_hash: u64,
        yes_ask: u64,
        no_ask: u64,
        combined: u64,
        is_dust: bool,
    },
    /// Edge detected (actionable opportunity)
    EdgeFound {
        yes_hash: u64,
        no_hash: u64,
        yes_ask: u64,
        no_ask: u64,
        combined: u64,
        profit_usd: u64,
    },
    /// Sequence tracking
    Sequence {
        expected: u64,
        received: u64,
        dropped: u64,
    },
}

/// Logger handle for sending events from hot path
#[derive(Clone)]
pub struct JsonlLogger {
    tx: Sender<LogEvent>,
}

impl JsonlLogger {
    /// Create new logger with bounded channel (capacity: 4096 events)
    pub fn new() -> Self {
        let (tx, _rx) = bounded(4096);
        Self { tx }
    }
    
    /// Get sender for hot path (clone this, not the whole logger)
    pub fn sender(&self) -> Sender<LogEvent> {
        self.tx.clone()
    }
    
    /// Start background logging thread
    pub fn start(log_rx: Receiver<LogEvent>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            run_logger_thread(log_rx);
        })
    }
}

/// Background thread: formats JSONL using itoa (zero-allocation)
fn run_logger_thread(log_rx: Receiver<LogEvent>) {
    let file = File::create("/mnt/ramdisk/nohup_v2.jsonl")
        .expect("Failed to create JSONL log file");
    let mut writer = BufWriter::with_capacity(65536, file);
    let mut buf = [0u8; 256]; // Pre-allocated stack buffer
    
    while let Ok(event) = log_rx.recv() {
        let mut cursor = std::io::Cursor::new(&mut buf[..]);
        
        match event {
            LogEvent::Metric { uptime, msgs, evals, edges, evals_sec, pairs, dropped } => {
                write!(&mut cursor, 
                    "{{\"t\":\"m\",\"up\":{},\"msg\":{},\"ev\":{},\"ed\":{},\"rps\":{},\"pr\":{},\"dr\":{}}}\n",
                    uptime, msgs, evals, edges, evals_sec, pairs, dropped
                ).unwrap();
            },
            LogEvent::Rollover { action, hash, end_time } => {
                let action_str = if action == 0 { "add" } else { "rem" };
                write!(&mut cursor,
                    "{{\"t\":\"r\",\"act\":\"{}\",\"h\":{},\"end\":{}}}\n",
                    action_str, hash, end_time
                ).unwrap();
            },
            LogEvent::Orderbook { hash, price, size, is_bid } => {
                let side = if is_bid { "b" } else { "a" };
                write!(&mut cursor,
                    "{{\"t\":\"b\",\"h\":{},\"p\":{},\"s\":{},\"side\":\"{}\"}}\n",
                    hash, price, size, side
                ).unwrap();
            },
            LogEvent::EdgeCheck { yes_hash, no_hash, yes_ask, no_ask, combined, is_dust } => {
                write!(&mut cursor,
                    "{{\"t\":\"ec\",\"yh\":{},\"nh\":{},\"ya\":{},\"na\":{},\"sum\":{},\"dust\":{}}}\n",
                    yes_hash, no_hash, yes_ask, no_ask, combined, is_dust
                ).unwrap();
            },
            LogEvent::EdgeFound { yes_hash, no_hash, yes_ask, no_ask, combined, profit_usd } => {
                write!(&mut cursor,
                    "{{\"t\":\"e\",\"yh\":{},\"nh\":{},\"ya\":{},\"na\":{},\"sum\":{},\"prof\":{}}}\n",
                    yes_hash, no_hash, yes_ask, no_ask, combined, profit_usd
                ).unwrap();
            },
            LogEvent::Sequence { expected, received, dropped } => {
                write!(&mut cursor,
                    "{{\"t\":\"s\",\"exp\":{},\"recv\":{},\"drop\":{}}}\n",
                    expected, received, dropped
                ).unwrap();
            },
        }
        
        let pos = cursor.position() as usize;
        writer.write_all(&buf[..pos]).unwrap();
        writer.flush().unwrap();  // Flush after each event for real-time logging
    }
}
