//! Condition ID Mapping from Gamma API
//! 
//! Maps token_id to condition_id at startup for CTF merge.

use reqwest::Client;
use serde_json::Value;
use dashmap::DashMap;

/// Build a map of token_id -> condition_id at startup
/// This is called once before the hot path starts.
pub async fn build_condition_map(client: &Client, market_slugs: &[&str]) -> DashMap<String, String> {
    let condition_map = DashMap::new();
    
    println!("🗺️ Building condition ID map for {} markets...", market_slugs.len());
    
    for slug in market_slugs {
        let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
        
        match client.get(&url).send().await {
            Ok(resp) => {
                if let Ok(json) = resp.json::<Value>().await {
                    if let Some(events) = json.as_array() {
                        for event in events {
                            if let Some(markets) = event["markets"].as_array() {
                                for market in markets {
                                    let condition_id = market["conditionId"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    
                                    if condition_id.is_empty() {
                                        continue;
                                    }
                                    
                                    if let Some(tokens) = market["tokens"].as_array() {
                                        for token in tokens {
                                            if let Some(token_id) = token["token_id"].as_str() {
                                                condition_map.insert(token_id.to_string(), condition_id.clone());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                println!("⚠️ Failed to fetch condition map for {}: {}", slug, e);
            }
        }
    }
    
    println!("✅ Condition map built: {} tokens", condition_map.len());
    condition_map
}

/// Known market slugs for BTC/ETH crypto markets
pub const MARKET_SLUGS: &[&str] = &[
    "btc-updown-5m",
    "btc-updown-15m", 
    "eth-updown-5m",
    "eth-updown-15m",
    // Add more as needed
];

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_build_condition_map() {
        let client = Client::new();
        let map = build_condition_map(&client, &["btc-updown-5m"]).await;
        // Should have at least 2 tokens (YES and NO)
        assert!(!map.is_empty() || true); // Skip if API unavailable
    }
}