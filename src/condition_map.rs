//! Token Hash → Token ID Mapping (ACTIVE MARKETS ONLY)
//! 
//! BTC/ETH 5m/15m markets use dynamic slugs: {asset}-updown-{duration}-{timestamp}
//! Timestamp is Unix epoch for the END of the period.
//! 
//! This module ONLY tracks CURRENT and NEXT periods (no past markets).
//! Past markets have no liquidity (trading halted).

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

/// Get ONLY the current active 15-minute period
fn get_current_15m_period() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    // Round down to nearest 15 minutes (900 seconds)
    (now / 900) * 900
}

/// Get ONLY the current active 5-minute period
fn get_current_5m_period() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    // Round down to nearest 5 minutes (300 seconds)
    (now / 300) * 300
}

/// Get current + next period (for pre-subscription buffer)
/// NotebookLM: "Track current + immediate future only"
fn get_active_15m_periods() -> Vec<i64> {
    let current = get_current_15m_period();
    vec![current, current + 900]  // Current + Next (no past!)
}

fn get_active_5m_periods() -> Vec<i64> {
    let current = get_current_5m_period();
    vec![current, current + 300]  // Current + Next (no past!)
}

/// Build maps from BTC/ETH 5m/15m markets - ACTIVE ONLY
pub async fn build_maps(
    client: &Client,
) -> (HashMap<u64, String>, HashMap<String, String>, HashMap<u64, u64>) {
    let mut hash_to_id = HashMap::new();
    let mut id_to_condition = HashMap::new();
    let mut complement_map = HashMap::new();

    println!("📊 Querying Gamma API for ACTIVE BTC/ETH 5m/15m markets...");

    // Assets to query
    let assets = vec!["btc", "eth"];
    
    // Get ONLY current active periods (no past!)
    let periods_15m = get_active_15m_periods();
    let periods_5m = get_active_5m_periods();
    
    println!("  15m periods (current + next): {:?}", periods_15m);
    println!("  5m periods (current + next): {:?}", periods_5m);
    
    let mut market_count = 0;
    
    // Query each asset + duration combination
    for asset in &assets {
        // 15-minute markets (current + next only)
        for period_ts in &periods_15m {
            let slug = format!("{}-updown-15m-{}", asset, period_ts);
            if let Ok(markets) = query_market(client, &slug).await {
                for (condition_id, yes_token, no_token) in markets {
                    let yes_hash = hash_token(&yes_token);
                    let no_hash = hash_token(&no_token);
                    
                    hash_to_id.insert(yes_hash, yes_token.clone());
                    hash_to_id.insert(no_hash, no_token.clone());
                    id_to_condition.insert(yes_token.clone(), condition_id.clone());
                    id_to_condition.insert(no_token.clone(), condition_id.clone());
                    complement_map.insert(yes_hash, no_hash);
                    complement_map.insert(no_hash, yes_hash);
                    market_count += 1;
                    
                    println!("  ✅ {} (condition: {}..{})", 
                        slug, 
                        &condition_id[..8], 
                        &condition_id[condition_id.len()-6..]);
                }
            }
        }
        
        // 5-minute markets (current + next only)
        for period_ts in &periods_5m {
            let slug = format!("{}-updown-5m-{}", asset, period_ts);
            if let Ok(markets) = query_market(client, &slug).await {
                for (condition_id, yes_token, no_token) in markets {
                    let yes_hash = hash_token(&yes_token);
                    let no_hash = hash_token(&no_token);
                    
                    hash_to_id.insert(yes_hash, yes_token.clone());
                    hash_to_id.insert(no_hash, no_token.clone());
                    id_to_condition.insert(yes_token.clone(), condition_id.clone());
                    id_to_condition.insert(no_token.clone(), condition_id.clone());
                    complement_map.insert(yes_hash, no_hash);
                    complement_map.insert(no_hash, yes_hash);
                    market_count += 1;
                    
                    println!("  ✅ {} (condition: {}..{})", 
                        slug, 
                        &condition_id[..8], 
                        &condition_id[condition_id.len()-6..]);
                }
            }
        }
    }
    
    println!("✅ Found {} BTC/ETH 5m/15m markets (ACTIVE ONLY)", market_count);
    println!("✅ Mapped {} token hashes, {} complement pairs", 
        hash_to_id.len(), 
        complement_map.len() / 2);
    
    (hash_to_id, id_to_condition, complement_map)
}

/// Query Gamma API for a specific market slug
async fn query_market(
    client: &Client,
    slug: &str,
) -> Result<Vec<(String, String, String)>, Box<dyn std::error::Error>> {
    let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
    
    let resp = client.get(&url).send().await?;
    let json: Value = resp.json().await?;
    
    let mut results = Vec::new();
    
    if let Some(events) = json.as_array() {
        for event in events {
            if let Some(markets) = event["markets"].as_array() {
                for market in markets {
                    // Parse clobTokenIds - it's a JSON STRING, not array
                    let tokens_str = market["clobTokenIds"].as_str().unwrap_or("[]");
                    let outcomes_str = market["outcomes"].as_str().unwrap_or("[]");
                    
                    // Parse JSON strings
                    let token_ids: Vec<String> = serde_json::from_str(tokens_str).unwrap_or_default();
                    let outcomes: Vec<String> = serde_json::from_str(outcomes_str).unwrap_or_default();
                    
                    // Verify binary market with "Yes" and "No" outcomes
                    if outcomes.len() == 2 && 
                       (outcomes[0].to_lowercase() == "yes" || outcomes[1].to_lowercase() == "yes" || outcomes[0].to_lowercase() == "up" || outcomes[1].to_lowercase() == "up") {
                        if let Some(condition_id) = market["conditionId"].as_str() {
                            if token_ids.len() == 2 {
                                results.push((
                                    condition_id.to_string(),
                                    token_ids[0].clone(),
                                    token_ids[1].clone(),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(results)
}
// Re-exports for backwards compatibility
pub async fn build_condition_map(client: &Client) -> (HashMap<u64, String>, HashMap<String, String>, HashMap<u64, u64>) {
    build_maps(client).await
}

pub const MARKET_SLUGS: &[&str] = &[
    "btc-updown-15m",
    "btc-updown-5m",
    "eth-updown-15m",
    "eth-updown-5m",
];
