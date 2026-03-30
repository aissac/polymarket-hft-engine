//! Fetch active crypto up/down markets from Gamma API
//! 
//! Uses dynamic slug generation: {asset}-updown-{duration}-{timestamp}
//! Queries: GET https://gamma-api.polymarket.com/markets?slug={slug}

use chrono::{Utc, TimeZone, Datelike, Timelike};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashMap;

use crate::token_map::hash_token;

/// Market info returned from Gamma API
#[derive(Debug, Clone)]
pub struct MarketInfo {
    pub condition_id: String,
    pub token_ids: Vec<String>,
    pub slug: String,
}

/// Get current 15-minute period timestamps
/// Returns timestamps for: current, previous, and next 15-minute periods
pub fn get_current_periods() -> Vec<i64> {
    let now = Utc::now();
    let minute = (now.minute() / 15) * 15;
    let period_start = now.with_minute(minute).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let base_ts = period_start.timestamp();
    vec![base_ts, base_ts - 900, base_ts + 900]
}

/// Fetch market by slug from Gamma API
pub async fn fetch_market_by_slug(
    client: &Client,
    slug: &str,
    now: &chrono::DateTime<Utc>,
) -> Option<MarketInfo> {
    let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
    
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    
    let markets: Vec<Value> = resp.json().await.ok()?;
    let market = markets.into_iter().next()?;
    
    // Check if market is still active (not expired)
    let end_date_str = market.get("endDate")?.as_str()?;
    let end_date = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&Utc);
    let hours_until_resolve = (end_date - *now).num_hours();
    
    // Only accept markets that resolve soon (< 1 hour)
    if hours_until_resolve < 0 || hours_until_resolve > 1 {
        return None;
    }
    
    // Extract token IDs from clobTokenIds
    let token_ids_str = market.get("clobTokenIds")?.as_str()?;
    let token_ids: Vec<String> = serde_json::from_str(token_ids_str).ok()?;
    
    if token_ids.len() < 2 || token_ids[0].is_empty() {
        return None;
    }
    
    Some(MarketInfo {
        condition_id: market.get("conditionId")?.as_str()?.to_string(),
        token_ids: token_ids.into_iter().take(2).collect(),
        slug: slug.to_string(),
    })
}

/// Fetch all active crypto up/down markets
/// 
/// Searches for BTC and ETH 5-minute and 15-minute markets
pub async fn fetch_active_crypto_markets(
    client: &Client,
) -> (Vec<String>, HashMap<u64, u64>, HashMap<u64, String>) {
    let now = Utc::now();
    let assets = ["btc", "eth"];
    let periods = get_current_periods();
    
    let mut all_tokens: Vec<String> = Vec::new();
    let mut token_pairs: HashMap<u64, u64> = HashMap::new();
    let mut token_strings: HashMap<u64, String> = HashMap::new();
    let mut market_count = 0;
    
    for asset in &assets {
        for &period_ts in &periods {
            // Try 15-minute market
            let slug_15m = format!("{}-updown-15m-{}", asset, period_ts);
            if let Some(market) = fetch_market_by_slug(client, &slug_15m, &now).await {
                if market.token_ids.len() >= 2 {
                    let yes_token = &market.token_ids[0];
                    let no_token = &market.token_ids[1];
                    let yes_hash = hash_token(yes_token);
                    let no_hash = hash_token(no_token);
                    
                    token_pairs.insert(yes_hash, no_hash);
                    token_pairs.insert(no_hash, yes_hash);
                    token_strings.insert(yes_hash, yes_token.clone());
                    token_strings.insert(no_hash, no_token.clone());
                    
                    all_tokens.extend(market.token_ids);
                    market_count += 1;
                    println!("✅ Found 15m {} market: {} ({} min remaining)", 
                        asset.to_uppercase(), slug_15m, 
                        (end_date_from_slug(&slug_15m).unwrap_or(now) - now).num_minutes());
                }
            }
            
            // Try 5-minute market (starts 10 minutes before 15m period)
            let slug_5m = format!("{}-updown-5m-{}", asset, period_ts - 600);
            if let Some(market) = fetch_market_by_slug(client, &slug_5m, &now).await {
                if market.token_ids.len() >= 2 {
                    let yes_token = &market.token_ids[0];
                    let no_token = &market.token_ids[1];
                    let yes_hash = hash_token(yes_token);
                    let no_hash = hash_token(no_token);
                    
                    token_pairs.insert(yes_hash, no_hash);
                    token_pairs.insert(no_hash, yes_hash);
                    token_strings.insert(yes_hash, yes_token.clone());
                    token_strings.insert(no_hash, no_token.clone());
                    
                    all_tokens.extend(market.token_ids);
                    market_count += 1;
                    println!("✅ Found 5m {} market: {}", asset.to_uppercase(), slug_5m);
                }
            }
        }
    }
    
    // Remove duplicates
    all_tokens.sort();
    all_tokens.dedup();
    
    println!("📊 Fetched {} tokens from {} markets", all_tokens.len(), market_count);
    
    (all_tokens, token_pairs, token_strings)
}

/// Parse end date from slug timestamp (approximate)
fn end_date_from_slug(slug: &str) -> Option<chrono::DateTime<Utc>> {
    // Extract timestamp from slug like "btc-updown-15m-1743283200"
    let parts: Vec<&str> = slug.split('-').collect();
    if parts.len() >= 4 {
        if let Ok(ts) = parts[3].parse::<i64>() {
            // 15m markets last 15 minutes, so end = start + 900 seconds
            let end_ts = ts + 900;
            return chrono::Utc.timestamp_opt(end_ts, 0).single();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_get_current_periods() {
        let periods = get_current_periods();
        assert_eq!(periods.len(), 3);
        // Each period should be 15 minutes (900 seconds) apart
        assert_eq!(periods[1] - periods[0], 900);
        assert_eq!(periods[2] - periods[1], 900);
    }
}
