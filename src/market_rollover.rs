//! Market Rollover with Signal Filter - T-65s Check, T-60s Execute
//! Avoids 60-second liquidity vacuum (MM exodus at T-60s)

use std::time::{SystemTime, UNIX_EPOCH, Duration};
use std::collections::HashSet;
use tokio::time::sleep;
use tokio::sync::watch;
use crossbeam_channel::Sender;
use serde_json::Value;

use crate::hft_hot_path::RolloverCommandV2;
use crate::price_cache::PriceCache;
use crate::signal_filter::SignalFilter;

pub async fn run_rollover_thread(
    price_cache: &PriceCache,
    ws_tokens_tx: watch::Sender<Vec<String>>,
    hot_path_tx: Sender<RolloverCommandV2>,
) {
    let client = reqwest::Client::new();
    let signal_filter = SignalFilter::new();
    
    let mut discovered_slugs: HashSet<String> = HashSet::new();
    let mut active_ws_tokens: Vec<String> = Vec::new();

    println!("🔍 [DISCOVERY] Starting with INITIAL token subscription...");

    // Bootstrap: Subscribe to CURRENT markets immediately
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let current_5m = (now / 300) * 300;
    let current_15m = (now / 900) * 900;
    
    let assets = vec!["btc", "eth", "sol", "xrp"];
    for asset in &assets {
        let slug_5m = format!("{}-updown-5m-{}", asset, current_5m);
        if let Some((yes, no)) = fetch_market_tokens(&client, &slug_5m).await {
            println!("📊 [INIT] {} → YES={}, NO={}", slug_5m, &yes[..16], &no[..16]);
            active_ws_tokens.push(yes);
            active_ws_tokens.push(no);
            discovered_slugs.insert(slug_5m);
        }
        
        let slug_15m = format!("{}-updown-15m-{}", asset, current_15m);
        if let Some((yes, no)) = fetch_market_tokens(&client, &slug_15m).await {
            println!("📊 [INIT] {} → YES={}, NO={}", slug_15m, &yes[..16], &no[..16]);
            active_ws_tokens.push(yes);
            active_ws_tokens.push(no);
            discovered_slugs.insert(slug_15m);
        }
    }
    
    println!("🔗 [DISCOVERY] Subscribing to {} initial tokens", active_ws_tokens.len());
    let _ = ws_tokens_tx.send(active_ws_tokens.clone());
    println!("✅ [DISCOVERY] Initial subscription complete");

    // Main loop
    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let next_5m = (now / 300) * 300 + 300;
        let next_15m = (now / 900) * 900 + 900;
        
        let mut new_markets_found = false;
        
        for asset in &assets {
            // 5m: Check at T-65s, execute at T-60s
            let time_to_5m = next_5m - now;
            if time_to_5m <= 65 && time_to_5m > 55 {
                let next_slug = format!("{}-updown-5m-{}", asset, next_5m);
                if !discovered_slugs.contains(&next_slug) {
                    let current_slug = format!("{}-updown-5m-{}", asset, (now / 300) * 300);
                    if let Some((cur_yes, cur_no)) = fetch_market_tokens(&client, &current_slug).await {
                        // T-65s: Check signal BEFORE MM exodus
                        if signal_filter.is_market_ranging(price_cache, &cur_yes, &cur_no) {
                            println!("✅ [SIGNAL @ T-65s] {} RANGING - will place trap at T-60s", asset);
                            // Sleep remaining time to hit T-60s exactly
                            let trigger_time = next_5m.saturating_sub(60);
                            let sleep_time = trigger_time.saturating_sub(now);
                            if sleep_time > 0 {
                                sleep(Duration::from_secs(sleep_time)).await;
                            }
                            place_trap(&client, &next_slug, next_5m, &hot_path_tx, &mut active_ws_tokens, &mut discovered_slugs).await;
                            new_markets_found = true;
                        } else {
                            println!("⚠️ [SIGNAL @ T-65s] {} TRENDING - SKIP next 5m", asset);
                            discovered_slugs.insert(next_slug);
                        }
                    }
                }
            }
            
            // 15m: Same logic
            let time_to_15m = next_15m - now;
            if time_to_15m <= 65 && time_to_15m > 55 {
                let next_slug = format!("{}-updown-15m-{}", asset, next_15m);
                if !discovered_slugs.contains(&next_slug) {
                    let current_slug = format!("{}-updown-15m-{}", asset, (now / 900) * 900);
                    if let Some((cur_yes, cur_no)) = fetch_market_tokens(&client, &current_slug).await {
                        if signal_filter.is_market_ranging(price_cache, &cur_yes, &cur_no) {
                            println!("✅ [SIGNAL @ T-65s] {} 15m RANGING - will place trap at T-60s", asset);
                            let trigger_time = next_15m.saturating_sub(60);
                            let sleep_time = trigger_time.saturating_sub(now);
                            if sleep_time > 0 {
                                sleep(Duration::from_secs(sleep_time)).await;
                            }
                            place_trap(&client, &next_slug, next_15m, &hot_path_tx, &mut active_ws_tokens, &mut discovered_slugs).await;
                            new_markets_found = true;
                        } else {
                            println!("⚠️ [SIGNAL @ T-65s] {} 15m TRENDING - SKIP next 15m", asset);
                            discovered_slugs.insert(next_slug);
                        }
                    }
                }
            }
        }
        
        if new_markets_found {
            println!("🔗 [DISCOVERY] Total tracked: {} tokens", active_ws_tokens.len());
            let _ = ws_tokens_tx.send(active_ws_tokens.clone());
        }
        
        // Cleanup expired
        discovered_slugs.retain(|s| {
            let parts: Vec<&str> = s.split("-").collect();
            if let Some(ts_str) = parts.last() {
                if let Ok(ts) = ts_str.parse::<u64>() {
                    return ts > now;
                }
            }
            false
        });
        
        sleep(Duration::from_secs(10)).await;
    }
}

async fn fetch_market_tokens(client: &reqwest::Client, slug: &str) -> Option<(String, String)> {
    let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
    match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
        Ok(Ok(resp)) => {
            if let Ok(json) = resp.json::<Vec<Value>>().await {
                if let Some(market) = json.first() {
                    if let Ok(tokens) = serde_json::from_str::<Vec<String>>(market["clobTokenIds"].as_str()?) {
                        if tokens.len() >= 2 {
                            return Some((tokens[0].clone(), tokens[1].clone()));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    None
}

async fn place_trap(
    client: &reqwest::Client,
    slug: &str,
    end_time: u64,
    hot_path_tx: &Sender<RolloverCommandV2>,
    active_ws_tokens: &mut Vec<String>,
    discovered_slugs: &mut HashSet<String>,
) {
    if let Some((yes, no)) = fetch_market_tokens(client, slug).await {
        println!("✅ [TRAP PLACED @ T-60s] {}", slug);
        println!("   YES: {} | NO: {} | Ends: {}", &yes[..16], &no[..16], end_time);
        discovered_slugs.insert(slug.to_string());
        active_ws_tokens.push(yes.clone());
        active_ws_tokens.push(no.clone());
        let _ = hot_path_tx.send(RolloverCommandV2::AddPair { yes_token: yes, no_token: no, end_time });
    }
}

#[derive(Debug, Clone)]
pub enum RolloverCommandV2 {
    AddPair { yes_token: String, no_token: String, end_time: u64 },
    RemovePair { yes_token: String, no_token: String },
}
