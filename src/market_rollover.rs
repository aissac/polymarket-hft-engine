//! Market Rollover - Dynamic Token Pair Management
//!
//! Fetches markets by constructing slugs (same as startup).
//! Uses clobTokenIds field (JSON string array) to get token IDs.
//! Sends AddPair/RemovePair commands to hot path via crossbeam_channel.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use chrono::{Utc, Timelike};
use serde_json::Value;
use crossbeam_channel::Sender;
use crate::hft_hot_path::RolloverCommand;

/// Hash token ID string to u64 (MUST match hot path fast_hash)
pub fn hash_token(token_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    use std::collections::hash_map::DefaultHasher;
    let mut hasher = DefaultHasher::new();
    token_id.as_bytes().hash(&mut hasher);
    hasher.finish()
}

/// Get CURRENT period timestamps (same as startup)
fn get_current_periods() -> Vec<(i64, &'static str)> {
    let now = Utc::now();
    let mut periods = Vec::new();
    
    // 5-minute periods
    let minute_5 = (now.minute() / 5) * 5;
    let period_5m = now.with_minute(minute_5).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_5m = period_5m.timestamp();
    periods.push((ts_5m, "5m"));
    periods.push((ts_5m + 300, "5m"));  // Next 5m
    
    // 15-minute periods
    let minute_15 = (now.minute() / 15) * 15;
    let period_15m = now.with_minute(minute_15).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_15m = period_15m.timestamp();
    periods.push((ts_15m, "15m"));
    periods.push((ts_15m + 900, "15m"));  // Next 15m
    
    // 1-hour periods
    let period_1h = now.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
    let ts_1h = period_1h.timestamp();
    periods.push((ts_1h, "1h"));
    periods.push((ts_1h + 3600, "1h"));  // Next 1h
    
    periods
}

/// Fetch tokens for a specific market slug using clobTokenIds
async fn fetch_market_tokens(client: &reqwest::Client, slug: &str, now: &chrono::DateTime<Utc>) -> Option<(String, String)> {
    let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
    
    match client.get(&url).send().await {
        Ok(response) => {
            if !response.status().is_success() {
                return None;
            }
            
            match response.json::<Value>().await {
                Ok(json) => {
                    if let Some(markets) = json.as_array() {
                        if let Some(market) = markets.first() {
                            // Check if market is active (started, not expired, not closed)
                            let active = market.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                            let closed = market.get("closed").and_then(|v| v.as_bool()).unwrap_or(true);
                            
                            // Parse dates
                            let start_str = market.get("startDate").and_then(|v| v.as_str()).unwrap_or("");
                            let end_str = market.get("endDate").and_then(|v| v.as_str()).unwrap_or("");
                            
                            let start_time = chrono::DateTime::parse_from_rfc3339(start_str).ok()?.with_timezone(&Utc);
                            let end_time = chrono::DateTime::parse_from_rfc3339(end_str).ok()?.with_timezone(&Utc);
                            
                            // Only add if currently active (started, not expired, not closed)
                            if !active || closed || now < &start_time || now >= &end_time {
                                return None;
                            }
                            
                            // Extract from clobTokenIds (JSON string array like startup does)
                            let clob_ids_str = market.get("clobTokenIds").and_then(|v| v.as_str())?;
                            let clob_ids: Vec<String> = serde_json::from_str(clob_ids_str).ok()?;
                            
                            if clob_ids.len() >= 2 && !clob_ids[0].is_empty() && !clob_ids[1].is_empty() {
                                println!("[ROLLOVER] Found: {} (clobTokenIds[0] len={})", 
                                    slug, clob_ids[0].len());
                                return Some((clob_ids[0].clone(), clob_ids[1].clone()));
                            }
                        }
                    }
                    None
                }
                Err(e) => {
                    println!("[ROLLOVER] {} JSON error: {}", slug, e);
                    None
                }
            }
        }
        Err(e) => {
            println!("[ROLLOVER] {} network error: {}", slug, e);
            None
        }
    }
}

/// Run the rollover monitoring thread
pub async fn run_rollover_thread(
    client: Arc<reqwest::Client>,
    rollover_tx: Sender<RolloverCommand>,
) {
    println!("🔄 [ROLLOVER] Thread started - monitoring for active markets");
    
    let mut tracked_markets: HashSet<String> = HashSet::new();
    
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        
        let now = Utc::now();
        let periods = get_current_periods();
        let assets = ["btc", "eth", "sol", "xrp"];
        
        let mut active_this_cycle: HashSet<String> = HashSet::new();
        let mut found_count = 0;
        
        for asset in &assets {
            for (period_ts, timeframe) in &periods {
                let slug = match *timeframe {
                    "5m" => format!("{}-updown-5m-{}", asset, period_ts - 600),
                    "15m" => format!("{}-updown-15m-{}", asset, period_ts),
                    "1h" => format!("{}-updown-1h-{}", asset, period_ts),
                    _ => continue,
                };
                
                match fetch_market_tokens(&client, &slug, &now).await {
                    Some((yes_token, no_token)) => {
                        found_count += 1;
                        let market_id = yes_token.clone();
                        active_this_cycle.insert(market_id.clone());
                        
                        if !tracked_markets.contains(&market_id) {
                            println!("🟢 [ROLLOVER] Adding: {} ({} {})", slug, asset, timeframe);
                            
                            let yes_hash = hash_token(&yes_token);
                            let no_hash = hash_token(&no_token);
                            
                            if let Err(e) = rollover_tx.send(RolloverCommand::AddPair(yes_hash, no_hash)) {
                                eprintln!("🚨 [ROLLOVER] Channel disconnected: {}", e);
                                return;
                            }
                            
                            tracked_markets.insert(market_id.clone());
                            println!("[ROLLOVER] Now tracking {} markets", tracked_markets.len());
                        }
                    }
                    None => {
                        // Market doesn't exist, not active, or expired
                    }
                }
            }
        }
        
        println!("[ROLLOVER] Checked {} slugs, found {} active markets, tracking {}", 
            assets.len() * periods.len(), found_count, tracked_markets.len());
        
        // Remove expired markets
        let expired: Vec<String> = tracked_markets
            .iter()
            .filter(|id| !active_this_cycle.contains(*id))
            .cloned()
            .collect();
        
        for expired_id in expired {
            println!("🔴 [ROLLOVER] Removing expired: {}", expired_id);
            
            let expired_hash = hash_token(&expired_id);
            
            let _ = rollover_tx.send(RolloverCommand::RemovePair(expired_hash));
            tracked_markets.remove(&expired_id);
        }
    }
}
