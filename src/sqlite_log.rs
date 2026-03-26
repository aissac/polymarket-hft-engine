//! Structured logging to SQLite with batched writes
//! 
//! Architecture: Hot path sends structs over mpsc channel → background task batches inserts to SQLite
//! This keeps the hot path fast (~0.28µs) while enabling rich SQL queries for reports.

use r2d2::{self, Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Transaction};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use parking_lot::Mutex;

/// Database pool type
pub type DbPool = Pool<SqliteConnectionManager>;
pub type DbConn = PooledConnection<SqliteConnectionManager>;

/// Event types for structured logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TradeEvent {
    /// Arbitrage opportunity detected (before execution decision)
    ArbDetected {
        condition_id: String,
        yes_price: f64,
        no_price: f64,
        combined: f64,
        edge: f64,
        size: i32,
    },
    /// Trade completed (or dry run completed)
    TradeCompleted {
        condition_id: String,
        edge: f64,
        size: i32,
        profit: f64,
        profit_total: f64,
        mode: String, // "dry" or "live"
    },
    /// Orderbook snapshot for analytics
    OrderbookSnapshot {
        condition_id: String,
        yes_bid: f64,
        yes_ask: f64,
        no_bid: f64,
        no_ask: f64,
    },
    /// Risk event (MLE block, error, etc)
    RiskEvent {
        event_type: String,
        condition_id: Option<String>,
        details: String,
    },
}

/// Shared database state
pub struct DbState {
    pool: DbPool,
    stats: Mutex<DbStats>,
}

#[derive(Debug, Default)]
pub struct DbStats {
    pub arbs_detected: u64,
    pub trades_completed: u64,
    pub risk_events: u64,
    pub last_insert: Option<u64>,
}

impl DbState {
    /// Initialize database with schema
    pub fn new(db_path: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let manager = SqliteConnectionManager::file(db_path);
        let pool = Pool::builder()
            .max_size(2) // Minimal connections needed
            .build(manager)?;
        
        let conn = pool.get()?;
        Self::create_schema(&conn)?;
        
        Ok(Self {
            pool,
            stats: Mutex::new(DbStats::default()),
        })
    }
    
    /// Create tables and indexes
    fn create_schema(conn: &DbConn) -> Result<(), rusqlite::Error> {
        conn.execute_batch(
            r#"
            -- Arb events: opportunities detected (may or may not be executed)
            CREATE TABLE IF NOT EXISTS arb_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                ts_unix INTEGER NOT NULL,
                condition_id TEXT NOT NULL,
                yes_price REAL NOT NULL,
                no_price REAL NOT NULL,
                combined REAL NOT NULL,
                edge REAL NOT NULL,
                size INTEGER NOT NULL,
                executed INTEGER DEFAULT 0
            );
            
            -- Trades: completed trades (dry or live)
            CREATE TABLE IF NOT EXISTS trades (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                ts_unix INTEGER NOT NULL,
                condition_id TEXT NOT NULL,
                edge REAL NOT NULL,
                size INTEGER NOT NULL,
                profit REAL NOT NULL,
                profit_total REAL NOT NULL,
                mode TEXT NOT NULL
            );
            
            -- Orderbook snapshots (for analytics, sampled)
            CREATE TABLE IF NOT EXISTS orderbook_snapshots (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                ts_unix INTEGER NOT NULL,
                condition_id TEXT NOT NULL,
                yes_bid REAL NOT NULL,
                yes_ask REAL NOT NULL,
                no_bid REAL NOT NULL,
                no_ask REAL NOT NULL
            );
            
            -- Risk events: MLE blocks, errors, pauses
            CREATE TABLE IF NOT EXISTS risk_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                ts_unix INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                condition_id TEXT,
                details TEXT NOT NULL
            );
            
            -- Stats cache for quick reads
            CREATE TABLE IF NOT EXISTS stats_cache (
                key TEXT PRIMARY KEY,
                value REAL NOT NULL,
                updated TEXT NOT NULL
            );
            
            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_arbs_ts ON arb_events(ts_unix);
            CREATE INDEX IF NOT EXISTS idx_arbs_edge ON arb_events(edge);
            CREATE INDEX IF NOT EXISTS idx_arbs_cond ON arb_events(condition_id);
            CREATE INDEX IF NOT EXISTS idx_trades_ts ON trades(ts_unix);
            CREATE INDEX IF NOT EXISTS idx_trades_mode ON trades(mode);
            CREATE INDEX IF NOT EXISTS idx_risk_ts ON risk_events(ts_unix);
            "#,
        )?;
        Ok(())
    }
    
    /// Get a connection from the pool
    pub fn conn(&self) -> Result<DbConn, r2d2::Error> {
        self.pool.get()
    }
    
    /// Get current timestamp as ISO string and unix
    pub fn now() -> (String, i64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        (iso, now as i64)
    }
    
    /// Insert a single event (called from background task)
    pub fn insert_event(&self, event: &TradeEvent) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.conn()?;
        let (ts, ts_unix) = Self::now();
        
        match event {
            TradeEvent::ArbDetected { condition_id, yes_price, no_price, combined, edge, size } => {
                conn.execute(
                    "INSERT INTO arb_events (ts, ts_unix, condition_id, yes_price, no_price, combined, edge, size) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![ts, ts_unix, condition_id, *yes_price, *no_price, *combined, *edge, *size],
                )?;
            }
            TradeEvent::TradeCompleted { condition_id, edge, size, profit, profit_total, mode } => {
                conn.execute(
                    "INSERT INTO trades (ts, ts_unix, condition_id, edge, size, profit, profit_total, mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![ts, ts_unix, condition_id, *edge, *size, *profit, *profit_total, mode],
                )?;
            }
            TradeEvent::RiskEvent { event_type, condition_id, details } => {
                conn.execute(
                    "INSERT INTO risk_events (ts, ts_unix, event_type, condition_id, details) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![ts, ts_unix, event_type, condition_id, details],
                )?;
            }
            TradeEvent::OrderbookSnapshot { .. } => {
                // Skip for now - too high volume
            }
        }
        
        let mut stats = self.stats.lock();
        match event {
            TradeEvent::ArbDetected { .. } => stats.arbs_detected += 1,
            TradeEvent::TradeCompleted { .. } => stats.trades_completed += 1,
            TradeEvent::RiskEvent { .. } => stats.risk_events += 1,
            TradeEvent::OrderbookSnapshot { .. } => {}
        }
        stats.last_insert = Some(ts_unix);
        
        Ok(())
    }
    
    /// Insert batch of events efficiently
    pub fn insert_batch(&self, events: &[TradeEvent]) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        if events.is_empty() {
            return Ok(0);
        }
        
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let (ts, ts_unix) = Self::now();
        
        let mut count = 0;
        for event in events {
            match event {
                TradeEvent::ArbDetected { condition_id, yes_price, no_price, combined, edge, size } => {
                    tx.execute(
                        "INSERT INTO arb_events (ts, ts_unix, condition_id, yes_price, no_price, combined, edge, size) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![ts, ts_unix, condition_id, *yes_price, *no_price, *combined, *edge, *size],
                    )?;
                    count += 1;
                }
                TradeEvent::TradeCompleted { condition_id, edge, size, profit, profit_total, mode } => {
                    tx.execute(
                        "INSERT INTO trades (ts, ts_unix, condition_id, edge, size, profit, profit_total, mode) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![ts, ts_unix, condition_id, *edge, *size, *profit, *profit_total, mode],
                    )?;
                    count += 1;
                }
                TradeEvent::RiskEvent { event_type, condition_id, details } => {
                    tx.execute(
                        "INSERT INTO risk_events (ts, ts_unix, event_type, condition_id, details) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![ts, ts_unix, event_type, condition_id, details],
                    )?;
                    count += 1;
                }
                TradeEvent::OrderbookSnapshot { .. } => {}
            }
        }
        
        tx.commit()?;
        
        // Update stats
        let mut stats = self.stats.lock();
        for event in events {
            match event {
                TradeEvent::ArbDetected { .. } => stats.arbs_detected += 1,
                TradeEvent::TradeCompleted { .. } => stats.trades_completed += 1,
                TradeEvent::RiskEvent { .. } => stats.risk_events += 1,
                TradeEvent::OrderbookSnapshot { .. } => {}
            }
        }
        stats.last_insert = Some(ts_unix);
        
        Ok(count)
    }
    
    /// Get summary stats (for reports)
    pub fn get_summary(&self) -> Result<Summary, Box<dyn std::error::Error + Send + Sync>> {
        let conn = self.conn()?;
        
        let (dry_trades, dry_profit) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(profit), 0) FROM trades WHERE mode = 'dry'",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
        )?;
        
        let (live_trades, live_profit) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(profit), 0) FROM trades WHERE mode = 'live'",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
        )?;
        
        let (total_arbs, avg_edge, max_edge) = conn.query_row(
            "SELECT COUNT(*), COALESCE(AVG(edge), 0), COALESCE(MAX(edge), 0) FROM arb_events",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?, row.get::<_, f64>(2)?)),
        )?;
        
        let (arbs_last_hour, recent_edge_avg) = conn.query_row(
            "SELECT COUNT(*), COALESCE(AVG(edge), 0) FROM arb_events WHERE ts_unix > strftime('%s', 'now') - 3600",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, f64>(1)?)),
        )?;
        
        let mle_blocks = conn.query_row(
            "SELECT COUNT(*) FROM risk_events WHERE event_type = 'MLE_BLOCKED' AND ts_unix > strftime('%s', 'now') - 3600",
            [],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )?;
        
        Ok(Summary {
            dry_trades,
            dry_profit,
            live_trades,
            live_profit,
            total_arbs,
            avg_edge,
            max_edge,
            arbs_last_hour,
            recent_edge_avg,
            mle_blocks,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub dry_trades: u64,
    pub dry_profit: f64,
    pub live_trades: u64,
    pub live_profit: f64,
    pub total_arbs: u64,
    pub avg_edge: f64,
    pub max_edge: f64,
    pub arbs_last_hour: u64,
    pub recent_edge_avg: f64,
    pub mle_blocks: u64,
}

/// Spawn background task that receives events and batch-inserts to SQLite
pub fn spawn_logger(db: Arc<DbState>, mut rx: mpsc::Receiver<TradeEvent>) {
    let batch_size = 100;
    let max_wait_ms = 1000;
    
    std::thread::spawn(move || {
        let mut batch: Vec<TradeEvent> = Vec::with_capacity(batch_size);
        let mut last_flush = std::time::Instant::now();
        
        loop {
            // Try to receive with timeout
            match tokio::runtime::Handle::current().block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_millis(100),
                    rx.recv()
                ).await
            }) {
                Ok(Some(event)) => {
                    batch.push(event);
                    if batch.len() >= batch_size {
                        if let Err(e) = db.insert_batch(&batch) {
                            eprintln!("DB batch insert error: {}", e);
                        }
                        batch.clear();
                        last_flush = std::time::Instant::now();
                    }
                }
                Ok(None) => {
                    // Channel closed, flush remaining
                    if !batch.is_empty() {
                        let _ = db.insert_batch(&batch);
                    }
                    break;
                }
                Err(_) => {
                    // Timeout, check if we should flush due to time
                    if last_flush.elapsed().as_millis() as u64 > max_wait_ms && !batch.is_empty() {
                        if let Err(e) = db.insert_batch(&batch) {
                            eprintln!("DB batch insert error: {}", e);
                        }
                        batch.clear();
                        last_flush = std::time::Instant::now();
                    }
                }
            }
        }
        
        eprintln!("Logger thread shutting down");
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    
    #[test]
    fn test_db_init() {
        let db = DbState::new(":memory:").unwrap();
        let summary = db.get_summary().unwrap();
        assert_eq!(summary.dry_trades, 0);
    }
    
    #[test]
    fn test_insert_event() {
        let db = DbState::new(":memory:").unwrap();
        
        let event = TradeEvent::ArbDetected {
            condition_id: "0x1234".to_string(),
            yes_price: 0.50,
            no_price: 0.45,
            combined: 0.95,
            edge: 5.0,
            size: 100,
        };
        
        db.insert_event(&event).unwrap();
        
        let summary = db.get_summary().unwrap();
        assert_eq!(summary.total_arbs, 1);
        assert_eq!(summary.avg_edge, 5.0);
    }
}
