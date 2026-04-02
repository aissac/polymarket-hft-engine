// Pre-Order Merge Trap - Maker Strategy for 5m/15m Markets

use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct MergeConfig {
    pub limit_price: f64,
    pub order_size: f64,
    pub pre_market_seconds: u64,
    pub max_wait_seconds: u64,
}

impl Default for MergeConfig {
    fn default() -> Self {
        Self {
            limit_price: 0.45,
            order_size: 100.0,
            pre_market_seconds: 60,
            max_wait_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MergeState {
    Waiting,
    OrdersPlaced,
    OneSideFilled,
    BothFilled,
    Merged,
    Cancelled,
}

pub struct MergeAttempt {
    pub market_slug: String,
    pub yes_token: String,
    pub no_token: String,
    pub state: MergeState,
    pub yes_order_id: Option<String>,
    pub no_order_id: Option<String>,
    pub yes_filled: f64,
    pub no_filled: f64,
    pub start_time: u64,
    pub end_time: u64,
}

impl MergeAttempt {
    pub fn new(slug: String, yes: String, no: String) -> Self {
        Self {
            market_slug: slug,
            yes_token: yes,
            no_token: no,
            state: MergeState::Waiting,
            yes_order_id: None,
            no_order_id: None,
            yes_filled: 0.0,
            no_filled: 0.0,
            start_time: 0,
            end_time: 0,
        }
    }
}

pub fn parse_market_times(slug: &str) -> Option<(u64, u64)> {
    let parts: Vec<&str> = slug.split("-").collect();
    if parts.len() >= 4 {
        if let Ok(end_ts) = parts[3].parse::<u64>() {
            let duration = if parts[2].contains("5m") { 300 } else { 900 };
            let start_ts = end_ts - duration;
            return Some((start_ts, end_ts));
        }
    }
    None
}

pub fn should_place_orders(market_slug: &str, config: &MergeConfig) -> bool {
    if let Some((start_ts, _end_ts)) = parse_market_times(market_slug) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let time_to_start = start_ts as i64 - now as i64;
        return time_to_start >= 0 && time_to_start <= config.pre_market_seconds as i64;
    }
    false
}

pub fn market_started(market_slug: &str) -> bool {
    if let Some((start_ts, _end_ts)) = parse_market_times(market_slug) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        return now >= start_ts;
    }
    false
}

pub fn should_cancel(market_slug: &str, config: &MergeConfig, order_time: u64) -> bool {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    if now - order_time > config.max_wait_seconds {
        return true;
    }
    if let Some((_start_ts, end_ts)) = parse_market_times(market_slug) {
        if now >= end_ts {
            return true;
        }
    }
    false
}

pub fn calculate_profit(yes_filled: f64, no_filled: f64, yes_price: f64, no_price: f64) -> f64 {
    let cost = (yes_filled * yes_price) + (no_filled * no_price);
    let merge_value = yes_filled.min(no_filled) * 1.0;
    let rebate = (yes_filled * yes_price * 0.0036) + (no_filled * no_price * 0.0036);
    merge_value + rebate - cost
}
