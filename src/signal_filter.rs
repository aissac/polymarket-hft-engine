// Signal Filter - Using WebSocket Price Cache (Production Ready)

use crate::price_cache::PriceCache;

pub struct SignalConfig {
    pub stable_min_price: f64,  // 0.35
    pub stable_max_price: f64,  // 0.65
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            stable_min_price: 0.35,
            stable_max_price: 0.65,
        }
    }
}

pub struct SignalFilter {
    config: SignalConfig,
}

impl SignalFilter {
    pub fn new() -> Self {
        Self {
            config: SignalConfig::default(),
        }
    }

    /// Check if market is ranging using cached WebSocket prices
    pub fn is_market_ranging(&self, price_cache: &PriceCache, yes_token: &str, no_token: &str) -> bool {
        let yes_ask = price_cache.get(yes_token).map(|v| *v).unwrap_or(1.0);
        let no_ask = price_cache.get(no_token).map(|v| *v).unwrap_or(1.0);

        println!("📊 [SIGNAL] YES ask={:.3}, NO ask={:.3}", yes_ask, no_ask);

        // PolySmartX: If prices outside [0.35, 0.65], market is trending
        if yes_ask < self.config.stable_min_price || yes_ask > self.config.stable_max_price {
            return false;
        }
        if no_ask < self.config.stable_min_price || no_ask > self.config.stable_max_price {
            return false;
        }

        true
    }
}
