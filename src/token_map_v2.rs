//! Token Hash → Token ID Mapping for Hot Path Resolution
//! 
//! Hot path uses 64-bit token hashes for speed (memchr parsing).
//! Background thread needs full 66-char token IDs for API calls.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use reqwest::Client;

use crate::crypto_markets::fetch_active_crypto_markets;

/// Simple u64 hasher for hot path tokens
pub fn hash_token(token_str: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    token_str.hash(&mut hasher);
    hasher.finish()
}

/// Build both mapping tables from active crypto markets
pub async fn build_maps_from_markets(
    client: &Client,
) -> (HashMap<u64, String>, HashMap<u64, String>, HashMap<u64, u64>) {
    let (all_tokens, token_pairs, token_strings) = fetch_active_crypto_markets(client).await;
    
    // Build hash_to_id map
    let mut hash_to_id = HashMap::new();
    for (hash, token_id) in token_strings {
        hash_to_id.insert(hash, token_id);
    }
    
    // Build id_to_condition map (simplified - use token_id as key)
    let mut id_to_condition = HashMap::new();
    for token_id in &all_tokens {
        // For now, map token to itself (can be enhanced later)
        id_to_condition.insert(hash_token(token_id), token_id.clone());
    }
    
    (hash_to_id, id_to_condition, token_pairs)
}
