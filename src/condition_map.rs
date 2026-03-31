//! Condition ID Mapping from Gamma API
//! 
//! Maps token_id to condition_id at startup for CTF merge.
//! Filters for BTC/ETH/SOL/XRP 5m/15m/1h Up/Down markets only.
//! Only active markets (started, not expired) are included.

use reqwest::Client;
use std::collections::HashMap;
use crate::token_map::hash_token;

pub async fn build_maps(client: &Client) -> (HashMap<u64, String>, HashMap<String, String>, HashMap<u64, u64>) {
    // crypto_markets already filters for:
    // - Assets: BTC, ETH, SOL, XRP
    // - Timeframes: 5m, 15m, 1h
    // - Only ACTIVE markets (now >= start_time && now < end_time)
    let (all_tokens, token_pairs, token_strings) = crate::crypto_markets::fetch_active_crypto_markets(client).await;
    
    // Build hash_to_id from token_strings
    let hash_to_id = token_strings;
    
    // Build id_to_condition: token_id (String) -> condition_id (String)
    // For now, use token_id as condition_id (simplified)
    let mut id_to_condition = HashMap::new();
    for token_id in &all_tokens {
        id_to_condition.insert(token_id.clone(), token_id.clone());
    }
    
    (hash_to_id, id_to_condition, token_pairs)
}

pub fn get_current_periods() -> Vec<i64> {
    // Return just the timestamps for backward compatibility
    crate::crypto_markets::get_current_periods().iter().map(|(ts, _)| *ts).collect()
}
