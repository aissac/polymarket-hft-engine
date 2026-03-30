//! Token Hash → Token ID Mapping (DYNAMIC TIMESTAMP SLUGS)
//! 
//! BTC/ETH 5m/15m markets use dynamic slugs: {asset}-updown-{duration}-{timestamp}
//! Timestamp is Unix epoch for the END of the period.
//! 
//! This module calculates current timestamps and queries Gamma API directly.

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

/// Calculate current 15-minute period timestamps
/// Returns [current_period, previous_period, next_period]
fn get_15m_periods() -> Vec<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    // Round down to nearest 15 minutes (900 seconds)
    let current_period = (now / 900) * 900;
    
    vec![
        current_period,           // Current period
        current_period - 900,     // Previous period (still resolving)
        current_period + 900,     // Next period (accepting orders)
    ]
}

/// Calculate current 5-minute period timestamps
fn get_5m_periods() -> Vec<i64> {
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    
    // Round down to nearest 5 minutes (300 seconds)
    let current_period = (now / 300) * 300;
    
    vec![
        current_period,
        current_period - 300,
        current_period + 300,
    ]
}

/// Build maps from BTC/ETH 5m/15m markets with DYNAMIC SLUGS
pub async fn build_maps(
    client: &Client,
) -> (HashMap<u64, String>, HashMap<String, String>, HashMap<u64, u64>) {
    let mut hash_to_id = HashMap::new();
    let mut id_to_condition = HashMap::new();
    let mut complement_map = HashMap::new();

    println!("📊 Querying Gamma API for BTC/ETH 5m/15m markets...");

    // Assets to query
    let assets = vec!["btc", "eth"];
    
    // Get current periods
    let periods_15m = get_15m_periods();
    let periods_5m = get_5m_periods();
    
    println!("  15m periods: {:?}", periods_15m);
    println!("  5m periods: {:?}", periods_5m);
    
    let mut market_count = 0;
    
    // Query each asset + duration combination
    for asset in &assets {
        // 15-minute markets
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
                        slug, &condition_id[..10], &condition_id[condition_id.len()-6..]);
                }
            }
        }
        
        // 5-minute markets
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
                        slug, &condition_id[..10], &condition_id[condition_id.len()-6..]);
                }
            }
        }
    }
    
    println!("✅ Found {} BTC/ETH 5m/15m markets", market_count);
    println!("✅ Mapped {} token hashes, {} complement pairs", 
        hash_to_id.len(), complement_map.len() / 2);
    
    if hash_to_id.is_empty() {
        eprintln!("⚠️ WARNING: No tokens fetched. Check slug format or API availability.");
    }
    
    (hash_to_id, id_to_condition, complement_map)
}

/// Query a specific market by slug
async fn query_market(client: &Client, slug: &str) -> Result<Vec<(String, String, String)>, String> {
    let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
    
    match client.get(&url).send().await {
        Ok(resp) => {
            match resp.json::<Value>().await {
                Ok(json) => {
                    let mut results = Vec::new();
                    
                    // Gamma API returns array of events
                    if let Some(events) = json.as_array() {
                        for event in events {
                            if let Some(markets) = event["markets"].as_array() {
                                for market in markets {
                                    // Check if active
                                    let is_active = market["active"].as_bool().unwrap_or(false);
                                    if !is_active {
                                        continue;
                                    }
                                    
                                    // Get condition ID
                                    let condition_id = market["conditionId"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    
                                    if condition_id.is_empty() {
                                        continue;
                                    }
                                    
                                    // Parse clobTokenIds (JSON STRING)
                                    if let Some(clob_ids_str) = market["clobTokenIds"].as_str() {
                                        if let Ok(clob_ids) = serde_json::from_str::<Vec<String>>(clob_ids_str) {
                                            if clob_ids.len() == 2 {
                                                let yes_token = clob_ids[0].clone();
                                                let no_token = clob_ids[1].clone();
                                                
                                                if !yes_token.is_empty() && !no_token.is_empty() {
                                                    results.push((condition_id, yes_token, no_token));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    
                    Ok(results)
                }
                Err(e) => Err(format!("Failed to parse JSON: {}", e)),
            }
        }
        Err(e) => Err(format!("Failed to query API: {}", e)),
    }
}

// Legacy exports
pub const MARKET_SLUGS: &[&str] = &[];

pub async fn build_condition_map(
    client: &Client,
    _slugs: &[&str],
) -> (HashMap<u64, String>, HashMap<String, String>) {
    let (hash_to_id, id_to_condition, _) = build_maps(client).await;
    (hash_to_id, id_to_condition)
}
