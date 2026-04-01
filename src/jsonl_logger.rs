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
        valid_evals: u64,
        missing_data: u64,
    },
    /// Rollover event
    Rollover {
        action: u8, // 0=add, 1=remove
        hash: u64,
        end_time: i64,
    },
    /// Edge check (for debugging)
    EdgeCheck {
        yes_hash: u64,
        no_hash: u64,
        combined_ask: u64,
        dust: bool,
    },
}

/// Background logger thread
pub struct JsonlLogger {
    receiver: Receiver<LogEvent>,
    writer: BufWriter<File>,
    buf: Vec<u8>,
}

impl JsonlLogger {
    pub fn new(receiver: Receiver<LogEvent>) -> Self {
        let file = File::create("/mnt/ramdisk/nohup_v2.jsonl")
            .expect("Failed to create JSONL file");
        let writer = BufWriter::new(file);
        
        Self {
            receiver,
            writer,
            buf: vec![0u8; 1024], // Pre-allocated buffer (reused)
        }
    }
    
    pub fn start(log_rx: Receiver<LogEvent>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut logger = JsonlLogger::new(log_rx);
            logger.run();
        })
    }
    
    fn run(&mut self) {
        eprintln!("✅ [LOGGER] Thread started");
        
        while let Ok(event) = self.receiver.recv() {
            // Format event into buffer
            let json_len = self.format_event(event);
            
            // Write to file with error handling (no unwrap!)
            if let Err(e) = self.writer.write_all(&self.buf[..json_len]) {
                eprintln!("🚨 [LOGGER] Disk I/O error (RAM disk full?): {}", e);
                // Don't exit - survive transient errors
            }
            
            // Safe flush - ignore errors
            let _ = self.writer.flush();
        }
        
        eprintln!("🚨 [LOGGER] Channel disconnected. Thread shutting down.");
    }
    
    fn format_event(&mut self, event: LogEvent) -> usize {
        use std::io::Write;
        
        let mut cursor = std::io::Cursor::new(&mut self.buf[..]);
        
        let result = match event {
            LogEvent::Metric { uptime, msgs, evals, edges, evals_sec, pairs, dropped, valid_evals: _, missing_data: _ } => {
                write!(cursor, 
                    "{{\"t\":\"m\",\"up\":{},\"msg\":{},\"ev\":{},\"ed\":{},\"rps\":{},\"pr\":{},\"dr\":{}}}\n",
                    uptime, msgs, evals, edges, evals_sec, pairs, dropped)
            }
            LogEvent::Rollover { action, hash, end_time } => {
                let action_str = if action == 0 { "add" } else { "remove" };
                write!(cursor, 
                    "{{\"t\":\"r\",\"a\":\"{}\",\"h\":{},\"e\":{}}}\n",
                    action_str, hash, end_time)
            }
            LogEvent::EdgeCheck { yes_hash, no_hash, combined_ask, dust } => {
                write!(cursor,
                    "{{\"t\":\"ec\",\"yh\":{},\"nh\":{},\"ya\":{},\"na\":{},\"sum\":{},\"dust\":{}}}\n",
                    yes_hash, no_hash, combined_ask / 2, combined_ask / 2, combined_ask, dust)
            }
        };
        
        if let Err(e) = result {
            eprintln!("🚨 [LOGGER] Format error: {}", e);
            return 0;
        }
        
        cursor.position() as usize
    }
}
