//! Fetch active crypto up/down markets from Gamma API
//! 
//! Filters for BTC, ETH, SOL, XRP 5-minute, 15-minute, and 1-hour markets
//! Only returns markets that are currently ACTIVE (started, not expired)

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
    pub start_time: chrono::DateTime<Utc>,
    pub end_time: chrono::DateTime<Utc>,
}

/// Get current period timestamps for different timeframes
pub fn get_current_periods() -> Vec<(i64, &'static str)> {
    let now = Utc::now();
    let mut periods = Vec::new();
    
    // 5-minute periods (round to nearest 5min)
    let minute_5 = (now.minute() / 5) * 5;
    let period_5m = now.with_minute(minute_5).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_5m = period_5m.timestamp();
    periods.push((ts_5m, "5m"));
    periods.push((ts_5m + 300, "5m")); // Next 5m period
    
    // 15-minute periods (round to nearest 15min)
    let minute_15 = (now.minute() / 15) * 15;
    let period_15m = now.with_minute(minute_15).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_15m = period_15m.timestamp();
    periods.push((ts_15m, "15m"));
    periods.push((ts_15m + 900, "15m")); // Next 15m period
    
    // 1-hour periods (round to nearest hour)
    let period_1h = now.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_1h = period_1h.timestamp();
    periods.push((ts_1h, "1h"));
    periods.push((ts_1h + 3600, "1h")); // Next 1h period
    
    periods
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
    
    // Extract start and end times
    let start_date_str = market.get("startDate")?.as_str()?;
    let end_date_str = market.get("endDate")?.as_str()?;
    
    let start_time = chrono::DateTime::parse_from_rfc3339(start_date_str).ok()?.with_timezone(&Utc);
    let end_time = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&Utc);
    
    // CRITICAL: Only return markets that are currently ACTIVE
    // Skip pre-trading markets (haven't started yet)
    if now < &start_time {
        return None;
    }
    
    // Skip expired markets
    if now >= &end_time {
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
        start_time,
        end_time,
    })
}


/// Fetch all active crypto up/down markets
/// 
/// Searches for BTC, ETH, SOL, XRP 5-minute, 15-minute, and 1-hour markets
/// Only returns markets that are currently ACTIVE (started, not expired)
pub async fn fetch_active_crypto_markets(
    client: &Client,
) -> (Vec<String>, HashMap<u64, u64>, HashMap<u64, String>) {
    let now = Utc::now();
    let assets = ["btc", "eth", "sol", "xrp"];
    let periods = get_current_periods();
    
    let mut all_tokens: Vec<String> = Vec::new();
    let mut token_pairs: HashMap<u64, u64> = HashMap::new();
    let mut token_strings: HashMap<u64, String> = HashMap::new();
    let mut market_count = 0;
    let mut skipped_pre_trading = 0;
    let mut skipped_expired = 0;
    
    // First, try to fetch all active markets from Gamma API
    println!("🎯 Fetching active crypto markets (BTC/ETH/SOL/XRP 5m/15m/1h)...");
    
    for asset in &assets {
        for (period_ts, timeframe) in &periods {
            // Construct slug based on timeframe
            let slug = match *timeframe {
                "5m" => format!("{}-updown-5m-{}", asset, period_ts - 600), // 5m starts 10min before period
                "15m" => format!("{}-updown-15m-{}", asset, period_ts),
                "1h" => format!("{}-updown-1h-{}", asset, period_ts),
                _ => continue,
            };
            
            // Try primary slug format first
            let mut market = fetch_market_by_slug(client, &slug, &now).await;
            
            // For 5m markets, also try alternative format if not found
            if market.is_none() && *timeframe == "5m" {
                let alt_slug = format!("{}-updown-5m-{}", asset, period_ts - 300);
                market = fetch_market_by_slug(client, &alt_slug, &now).await;
            }
            
            if let Some(market) = market {
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
                    
                    let minutes_remaining = (market.end_time - now).num_minutes();
                    println!("✅ [ACTIVE] {} {} market | {} min remaining", 
                        asset.to_uppercase(), 
                        timeframe,
                        minutes_remaining);
                }
            } else {
                // Market not found or not active yet
                // Check if it's a pre-trading market
                let test_url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
                if let Ok(resp) = client.get(&test_url).send().await {
                    if resp.status().is_success() {
                        if let Ok(markets) = resp.json::<Vec<Value>>().await {
                            if let Some(m) = markets.first() {
                                if let Some(end_str) = m.get("endDate").and_then(|v| v.as_str()) {
                                    if let Ok(end_time) = chrono::DateTime::parse_from_rfc3339(end_str) {
                                        let end_utc = end_time.with_timezone(&Utc);
                                        if now >= end_utc {
                                            skipped_expired += 1;
                                        } else {
                                            skipped_pre_trading += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Remove duplicates
    all_tokens.sort();
    all_tokens.dedup();
    
    println!("📊 Fetched {} tokens from {} ACTIVE markets", all_tokens.len(), market_count);
    println!("⏭️  Skipped {} pre-trading markets", skipped_pre_trading);
    println!("⚰️  Skipped {} expired markets", skipped_expired);
    
    (all_tokens, token_pairs, token_strings)
}

/// Parse end date from slug timestamp (approximate)
fn end_date_from_slug(slug: &str) -> Option<chrono::DateTime<Utc>> {
    // Extract timestamp from slug like "btc-updown-15m-1743283200"
    let parts: Vec<&str> = slug.split('-').collect();
    if parts.len() >= 4 {
        if let Ok(ts) = parts[3].parse::<i64>() {
            // Different timeframes have different durations
            let duration = if slug.contains("-5m-") {
                300 // 5 minutes
            } else if slug.contains("-1h-") || slug.contains("-60m-") {
                3600 // 1 hour
            } else {
                900 // 15 minutes (default)
            };
            
            // End = start + duration
            let end_ts = ts + duration;
            return chrono::Utc.timestamp_opt(end_ts, 0).single();
        }
    }
    None
}

