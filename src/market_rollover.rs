//! Market Rollover - Dynamic WebSocket Subscription Management
//!
//! Seamlessly transitions between market periods without disconnecting WebSocket.
//! - Pre-subscribes to future markets 1-2 minutes before start
//! - Unsubscribes from past markets after end
//! - Updates orderbook state to free memory

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use serde_json::Value;
use tokio::sync::mpsc::Sender;
use tungstenite::protocol::Message;

/// Market period information
#[derive(Clone, Debug)]
pub struct MarketPeriod {
    pub slug: String,
    pub start_time: i64,  // Unix timestamp
    pub end_time: i64,    // Unix timestamp
    pub yes_token: String,
    pub no_token: String,
    pub condition_id: String,
    pub duration: String, // "5m" or "15m"
    pub asset: String,     // "btc" or "eth"
}

/// Rollover state
pub struct RolloverState {
    pub active_periods: HashMap<String, MarketPeriod>,  // slug -> period
    pub subscribed_tokens: HashMap<String, String>,      // token_id -> slug
}

impl RolloverState {
    pub fn new() -> Self {
        Self {
            active_periods: HashMap::new(),
            subscribed_tokens: HashMap::new(),
        }
    }
}

/// Get current Unix timestamp in seconds
fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Calculate current 5m period boundaries
fn get_5m_boundaries() -> (i64, i64, i64) {
    let now = now_ts();
    let current = (now / 300) * 300;
    (current, current + 300, current + 600)  // (current_start, current_end, next_end)
}

/// Calculate current 15m period boundaries
fn get_15m_boundaries() -> (i64, i64, i64) {
    let now = now_ts();
    let current = (now / 900) * 900;
    (current, current + 900, current + 1800)  // (current_start, current_end, next_end)
}

/// Fetch market periods from Gamma API
pub async fn fetch_market_periods(
    client: &reqwest::Client,
    asset: &str,
    duration: &str,
) -> Result<Vec<MarketPeriod>, Box<dyn std::error::Error>> {
    let now = now_ts();
    let mut periods = Vec::new();
    
    // Calculate which periods to query
    let (current_start, current_end, next_end) = match duration {
        "5m" => get_5m_boundaries(),
        "15m" => get_15m_boundaries(),
        _ => return Err("Invalid duration".into()),
    };
    
    // Query current and next period
    for end_ts in &[current_end, next_end] {
        let slug = format!("{}-updown-{}-{}", asset, duration, end_ts);
        let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
        
        let resp = client.get(&url).send().await?;
        let json: Value = resp.json().await?;
        
        if let Some(events) = json.as_array() {
            for event in events {
                if let Some(markets) = event["markets"].as_array() {
                    for market in markets {
                        // Parse tokens
                        let tokens_str = market["clobTokenIds"].as_str().unwrap_or("[]");
                        let outcomes_str = market["outcomes"].as_str().unwrap_or("[]");
                        
                        let token_ids: Vec<String> = serde_json::from_str(tokens_str).unwrap_or_default();
                        let outcomes: Vec<String> = serde_json::from_str(outcomes_str).unwrap_or_default();
                        
                        if token_ids.len() == 2 {
                            let condition_id = market["conditionId"].as_str().unwrap_or("").to_string();
                            let start_time = *end_ts - match duration {
                                "5m" => 300,
                                "15m" => 900,
                                _ => 300,
                            };
                            
                            periods.push(MarketPeriod {
                                slug: slug.clone(),
                                start_time,
                                end_time: *end_ts,
                                yes_token: token_ids[0].clone(),
                                no_token: token_ids[1].clone(),
                                condition_id,
                                duration: duration.to_string(),
                                asset: asset.to_string(),
                            });
                        }
                    }
                }
            }
        }
    }
    
    Ok(periods)
}

/// Check for market rollover and return subscription updates
pub fn check_rollover(
    state: &mut RolloverState,
    periods: &[MarketPeriod],
) -> (Vec<String>, Vec<String>) {  // (tokens_to_subscribe, tokens_to_unsubscribe)
    let now = now_ts();
    let mut to_subscribe = Vec::new();
    let mut to_unsubscribe = Vec::new();
    
    // Buffer time: subscribe 1 minute before start
    let subscribe_buffer = 60;
    // Unsubscribe 30 seconds after end (allow for resolution)
    let unsubscribe_buffer = 30;
    
    for period in periods {
        let should_subscribe = now >= period.start_time - subscribe_buffer 
            && now < period.end_time;
        let already_subscribed = state.active_periods.contains_key(&period.slug);
        
        // Pre-subscribe to future markets
        if should_subscribe && !already_subscribed {
            state.active_periods.insert(period.slug.clone(), period.clone());
            state.subscribed_tokens.insert(period.yes_token.clone(), period.slug.clone());
            state.subscribed_tokens.insert(period.no_token.clone(), period.slug.clone());
            
            to_subscribe.push(period.yes_token.clone());
            to_subscribe.push(period.no_token.clone());
        }
    }
    
    // Unsubscribe from expired markets
    let expired: Vec<String> = state.active_periods.iter()
        .filter(|(_, period)| now > period.end_time + unsubscribe_buffer)
        .map(|(slug, _)| slug.clone())
        .collect();
    
    for slug in expired {
        if let Some(period) = state.active_periods.remove(&slug) {
            state.subscribed_tokens.remove(&period.yes_token);
            state.subscribed_tokens.remove(&period.no_token);
            
            to_unsubscribe.push(period.yes_token);
            to_unsubscribe.push(period.no_token);
        }
    }
    
    (to_subscribe, to_unsubscribe)
}

/// Build WebSocket subscription message
pub fn build_subscribe_message(token_ids: &[String]) -> Message {
    let tokens_json = serde_json::to_string(token_ids).unwrap();
    Message::Text(format!(
        r#"{{"type":"subscribe","channel":"market","tokens":{}}}"#,
        tokens_json
    ))
}

/// Build WebSocket unsubscribe message
pub fn build_unsubscribe_message(token_ids: &[String]) -> Message {
    let tokens_json = serde_json::to_string(token_ids).unwrap();
    Message::Text(format!(
        r#"{{"type":"unsubscribe","channel":"market","tokens":{}}}"#,
        tokens_json
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_5m_boundaries() {
        let (current_start, current_end, next_end) = get_5m_boundaries();
        assert_eq!(current_end - current_start, 300);
        assert_eq!(next_end - current_end, 300);
    }
    
    #[test]
    fn test_15m_boundaries() {
        let (current_start, current_end, next_end) = get_15m_boundaries();
        assert_eq!(current_end - current_start, 900);
        assert_eq!(next_end - current_end, 900);
    }
}