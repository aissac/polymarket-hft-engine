//! Market Rollover - Dynamic WebSocket Subscription Management
//!
//! Seamlessly transitions between market periods without disconnecting WebSocket.
//! - Pre-subscribes to future markets 1-2 minutes before start
//! - Unsubscribes from past markets after end
//! - Updates orderbook state to free memory

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Build WebSocket subscription message JSON
pub fn build_subscribe_message(token_ids: &[String]) -> String {
    let tokens_json = serde_json::to_string(token_ids).unwrap();
    format!(
        r#"{{"type":"subscribe","channel":"market","tokens":{}}}"#,
        tokens_json
    )
}

/// Build WebSocket unsubscribe message JSON
pub fn build_unsubscribe_message(token_ids: &[String]) -> String {
    let tokens_json = serde_json::to_string(token_ids).unwrap();
    format!(
        r#"{{"type":"unsubscribe","channel":"market","tokens":{}}}"#,
        tokens_json
    )
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

/// Get current and next market periods for all tracked assets
pub fn get_current_periods() -> Vec<MarketPeriod> {
    let now = now_ts();
    let mut periods = Vec::new();
    
    let assets = ["btc", "eth"];
    let durations = [("5m", 300), ("15m", 900)];
    
    for asset in &assets {
        for (duration, seconds) in &durations {
            let current_end = (now / seconds) * seconds + seconds;
            let next_end = current_end + seconds;
            
            // Current period
            periods.push(MarketPeriod {
                slug: format!("{}-updown-{}-{}", asset, duration, current_end),
                start_time: current_end - seconds,
                end_time: current_end,
                yes_token: String::new(),  // Will be fetched
                no_token: String::new(),
                condition_id: String::new(),
                duration: duration.to_string(),
                asset: asset.to_string(),
            });
            
            // Next period
            periods.push(MarketPeriod {
                slug: format!("{}-updown-{}-{}", asset, duration, next_end),
                start_time: current_end,
                end_time: next_end,
                yes_token: String::new(),
                no_token: String::new(),
                condition_id: String::new(),
                duration: duration.to_string(),
                asset: asset.to_string(),
            });
        }
    }
    
    periods
}