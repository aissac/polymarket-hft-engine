// Global Price Cache - Share WebSocket prices with Signal Filter

use dashmap::DashMap;
use std::sync::Arc;

/// Thread-safe price cache (token_id -> best_ask_price)
pub type PriceCache = Arc<DashMap<String, f64>>;

/// Create new price cache
pub fn create_price_cache() -> PriceCache {
    Arc::new(DashMap::new())
}

/// Update price in cache (called from SDK WebSocket)
pub fn update_price(cache: &PriceCache, token_id: String, price: f64) {
    cache.insert(token_id, price);
}

/// Get price from cache (called from Signal Filter)
pub fn get_price(cache: &PriceCache, token_id: &str) -> f64 {
    cache.get(token_id).map(|v| *v).unwrap_or(1.0)
}

/// Check if market is ranging using cached prices (PolySmartX approach)
pub fn is_market_ranging(cache: &PriceCache, yes_token: &str, no_token: &str) -> bool {
    let yes_ask = get_price(cache, yes_token);
    let no_ask = get_price(cache, no_token);
    
    println!("📊 [SIGNAL] Cached prices: YES={:.3}, NO={:.3}", yes_ask, no_ask);
    
    // PolySmartX: If prices outside [0.35, 0.65], market is trending
    if yes_ask < 0.35 || yes_ask > 0.65 || no_ask < 0.35 || no_ask > 0.65 {
        println!("🚨 [SIGNAL] Orderbook SKEWED - skipping trap");
        return false;
    }
    
    println!("✅ [SIGNAL] Orderbook RANGING - trap authorized");
    true
}
