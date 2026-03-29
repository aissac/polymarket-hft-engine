//! Token Hash → Token ID Mapping for Hot Path Resolution
//! 
//! Hot path uses 64-bit token hashes for speed (memchr parsing).
//! Background thread needs full 66-char token IDs for API calls.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use reqwest::Client;
use serde_json::Value;

/// Simple u64 hasher for hot path tokens
pub fn hash_token(token_str: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    token_str.hash(&mut hasher);
    hasher.finish()
}

/// Build both mapping tables at startup
/// 
/// Returns:
/// - HashMap<u64, String>: token_hash → full 66-char token_id
/// - HashMap<String, String>: token_id → condition_id
pub async fn build_maps(
    client: &Client, 
    market_slugs: &[&str]
) -> (HashMap<u64, String>, HashMap<String, String>) {
    let mut hash_to_id = HashMap::new();
    let mut id_to_condition = HashMap::new();

    for slug in market_slugs {
        let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
        
        match client.get(&url).send().await {
            Ok(resp) => {
                match resp.json::<Value>().await {
                    Ok(json) => {
                        if let Some(events) = json.as_array() {
                            for event in events {
                                if let Some(markets) = event["markets"].as_array() {
                                    for market in markets {
                                        let condition_id = market["conditionId"]
                                            .as_str()
                                            .unwrap_or("")
                                            .to_string();
                                        
                                        if let Some(tokens) = market["tokens"].as_array() {
                                            for token in tokens {
                                                let token_id = token["token_id"]
                                                    .as_str()
                                                    .unwrap_or("")
                                                    .to_string();
                                                
                                                if !token_id.is_empty() {
                                                    let token_hash = hash_token(&token_id);
                                                    
                                                    hash_to_id.insert(token_hash, token_id.clone());
                                                    id_to_condition.insert(token_id, condition_id.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("⚠️ Failed to parse JSON for {}: {}", slug, e),
                }
            }
            Err(e) => eprintln!("⚠️ Failed to fetch {}: {}", slug, e),
        }
    }
    
    println!("✅ Mapped {} token hashes, {} conditions", hash_to_id.len(), id_to_condition.len());
    (hash_to_id, id_to_condition)
}

/// Get YES/NO token pair from token hash
/// 
/// Returns (yes_token_id, no_token_id, condition_id)
pub fn get_token_pair(
    token_hash: u64,
    hash_to_id: &HashMap<u64, String>,
    id_to_condition: &HashMap<String, String>,
) -> Option<(String, String, String)> {
    let token_id = hash_to_id.get(&token_hash)?;
    let condition_id = id_to_condition.get(token_id)?;
    
    // Find the complementary token (YES ↔ NO)
    // Token IDs in same condition share same condition_id
    // Need to find the other token in the pair
    
    // For now, return what we have
    // TODO: Build token_pairs map at startup for O(1) lookup
    Some((token_id.clone(), token_id.clone(), condition_id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_token() {
        let token = "0x1234567890abcdef";
        let hash = hash_token(token);
        assert_ne!(hash, 0);
        
        // Same token = same hash
        let hash2 = hash_token(token);
        assert_eq!(hash, hash2);
        
        // Different token = different hash
        let hash3 = hash_token("0xABCDEF");
        assert_ne!(hash, hash3);
    }
}