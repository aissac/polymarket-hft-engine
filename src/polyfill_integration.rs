//! Polyfill-rs Integration Module
//! 
//! High-performance replacement for REST API and WebSocket parsing.
//! Uses polyfill-rs for ~0.28µs orderbook decode + 11% faster REST.
//!
//! Key components:
//! - ClobClient: Optimized HTTP/2 client with connection pre-warming
//! - WsBookUpdateProcessor: SIMD-accelerated JSON parsing, zero-allocation hot path
//! - OrderBookManager: Pre-allocated memory pool BTreeMap, 70ns spread calc

use anyhow::Result;
use polyfill_rs::{ClobClient, OrderBookManager, WsBookUpdateProcessor};
use std::collections::HashMap;
use tracing::info;

// Re-export for convenience
pub use polyfill_rs::Side;

#[derive(Debug, Clone)]
pub struct SimplifiedBook {
    pub condition_id: String,
    pub yes_best_bid: f64,
    pub yes_best_ask: f64,
    pub no_best_bid: f64,
    pub no_best_ask: f64,
    pub yes_total_bid_size: f64,
    pub no_total_bid_size: f64,
    pub last_update: std::time::Instant,
}

/// Polyfill-rs powered orderbook state
pub struct PolyfillOrderBookState {
    manager: OrderBookManager,
    simplified: HashMap<String, SimplifiedBook>,
    processor: WsBookUpdateProcessor,
}

impl PolyfillOrderBookState {
    pub fn new(book_levels: usize) -> Self {
        Self {
            manager: OrderBookManager::new(book_levels),
            simplified: HashMap::new(),
            processor: WsBookUpdateProcessor::new(4096),
        }
    }
    
    /// Process a WebSocket text message using SIMD-accelerated parsing
    pub fn process_ws_message(&mut self, text: String) -> Result<usize> {
        let stats = self.processor.process_text(text, &self.manager)?;
        Ok(stats.book_levels_applied)
    }
    
    /// Get best YES ask + NO ask for arbitrage check
    pub fn get_best_prices(&self, condition_id: &str) -> Option<(f64, f64)> {
        // Try our simplified cache first
        if let Some(book) = self.simplified.get(condition_id) {
            if book.yes_best_ask > 0.0 && book.no_best_ask > 0.0 {
                return Some((book.yes_best_ask, book.no_best_ask));
            }
        }
        None
    }
    
    /// Update simplified cache from polyfill-rs book
    pub fn update_simplified(&mut self, condition_id: &str) {
        if let Ok(book) = self.manager.get_book(condition_id) {
            // book.asks contains asks, book.bids contains bids
            let best_yes_ask = book.asks.first()
                .and_then(|l| l.price.to_string().parse::<f64>().ok())
                .unwrap_or(0.0);
            let best_no_ask = book.bids.first()
                .and_then(|l| l.price.to_string().parse::<f64>().ok())
                .unwrap_or(0.0);
            
            let yes_total_size: f64 = book.asks.iter()
                .map(|l| l.size.to_string().parse::<f64>().unwrap_or(0.0))
                .sum();
            let no_total_size: f64 = book.bids.iter()
                .map(|l| l.size.to_string().parse::<f64>().unwrap_or(0.0))
                .sum();
            
            let simplified = SimplifiedBook {
                condition_id: condition_id.to_string(),
                yes_best_bid: 0.0, // bids are on the NO side typically
                yes_best_ask: best_yes_ask,
                no_best_bid: 0.0,
                no_best_ask: best_no_ask,
                yes_total_bid_size: yes_total_size,
                no_total_bid_size: no_total_size,
                last_update: std::time::Instant::now(),
            };
            
            self.simplified.insert(condition_id.to_string(), simplified);
        }
    }
    
    /// Get top-of-book size for liquidity check
    pub fn get_top_size(&self, condition_id: &str) -> f64 {
        self.simplified.get(condition_id)
            .map(|b| b.yes_total_bid_size.max(b.no_total_bid_size))
            .unwrap_or(0.0)
    }
    
    /// Get the underlying polyfill-rs manager
    pub fn manager(&self) -> &OrderBookManager {
        &self.manager
    }
}

/// Create a high-performance REST client using polyfill-rs ClobClient
pub fn create_polyfill_client(base_url: &str) -> ClobClient {
    info!("Creating polyfill-rs ClobClient (11% faster HTTP/2 + connection pre-warming)");
    ClobClient::new(base_url)
}

/// Create client optimized for co-located deployment
pub fn create_polyfill_client_colocated(base_url: &str) -> ClobClient {
    info!("Creating co-located polyfill-rs ClobClient");
    ClobClient::new_colocated(base_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_state_creation() {
        let state = PolyfillOrderBookState::new(100);
        assert!(state.get_best_prices("nonexistent").is_none());
    }
}
