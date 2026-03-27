//! Polymarket API Client Module
//! 
//! Handles market data fetching from Gamma API

use chrono::{DateTime, Utc};
use anyhow::Result;
use tracing::{debug, info};

pub use crate::websocket::TokenData;

/// Simplified market representation
#[derive(Debug, Clone)]
pub struct SimplifiedMarket {
    pub condition_id: String,
    pub question: String,
    pub tokens: Vec<TokenData>,
    pub active: bool,
    pub closed: bool,
    pub hours_until_resolve: i64,
    pub slug: String,
}

impl SimplifiedMarket {
    /// Get YES token price
    pub fn yes_price(&self) -> Option<f64> {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "yes")
            .and_then(|t| t.price)
    }
    
    /// Get NO token price
    pub fn no_price(&self) -> Option<f64> {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "no")
            .and_then(|t| t.price)
    }
    
    /// Get combined cost (YES + NO)
    pub fn combined_cost(&self) -> Option<f64> {
        let yes = self.yes_price()?;
        let no = self.no_price()?;
        Some(yes + no)
    }
    
    /// Get YES token ID
    pub fn yes_token_id(&self) -> Option<String> {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "yes")
            .map(|t| t.token_id.clone())
    }
    
    /// Get NO token ID
    pub fn no_token_id(&self) -> Option<String> {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "no")
            .map(|t| t.token_id.clone())
    }
}

/// Polymarket API client
pub struct PolyClient {
    gamma_url: String,
}

impl Clone for PolyClient {
    fn clone(&self) -> Self {
        Self { gamma_url: self.gamma_url.clone() }
    }
}

impl PolyClient {
    pub fn new() -> Self {
        Self {
            gamma_url: "https://gamma-api.polymarket.com".to_string(),
        }
    }
    
    /// Get current period timestamps for 5m and 15m markets
    fn get_current_periods(&self) -> Vec<i64> {
        use chrono::Timelike;
        
        let now = Utc::now();
        let minute = (now.minute() / 15) * 15;
        let second = now.second();
        
        // Create timestamp for current 15-min period
        let period_start = now.with_minute(minute).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap();
        let base_ts = period_start.timestamp();
        
        // For 15m markets, only one period
        // For 5m markets, we have 3 periods in one 15-min block
        vec![base_ts, base_ts - 900, base_ts + 900] // current, prev 15m, next 15m
    }
    
    /// Get active Up/Down markets (5m and 15m crypto markets)
    pub async fn get_markets(&self) -> Result<Vec<SimplifiedMarket>> {
        use reqwest::Client;
        
        debug!("Fetching Up/Down markets...");
        
        let client = Client::new();
        let mut all_markets: Vec<SimplifiedMarket> = vec![];
        let now = Utc::now();
        
        // Assets to try
        let assets = ["btc", "eth", "sol", "xrp"];
        let periods = self.get_current_periods();
        
        for asset in &assets {
            for &period_ts in &periods {
                // Try 15m market
                let slug_15m = format!("{}-updown-15m-{}", asset, period_ts);
                if let Some(market) = self.fetch_market_by_slug(&client, &slug_15m, period_ts, &now).await {
                    all_markets.push(market);
                }
                
                // Try 5m market
                let slug_5m = format!("{}-updown-5m-{}", asset, period_ts);
                if let Some(market) = self.fetch_market_by_slug(&client, &slug_5m, period_ts - 600, &now).await {
                    all_markets.push(market);
                }
            }
        }
        
        // Sort by hours_until_resolve (shortest first)
        all_markets.sort_by_key(|m| m.hours_until_resolve);
        
        info!("Found {} active Up/Down markets", all_markets.len());
        
        Ok(all_markets)
    }
    
    /// Fetch a single market by slug
    async fn fetch_market_by_slug(&self, client: &reqwest::Client, slug: &str, period_ts: i64, now: &chrono::DateTime<Utc>) -> Option<SimplifiedMarket> {
        let url = format!("{}/markets?slug={}", self.gamma_url, slug);
        
        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(_) => return None,
        };
        
        if !resp.status().is_success() {
            return None;
        }
        
        let markets = match resp.json::<Vec<serde_json::Value>>().await {
            Ok(m) => m,
            Err(_) => return None,
        };
        
        let market = markets.into_iter().next()?;
        
        // Parse end date
        let end_date_str = market["endDate"].as_str()?;
        let end_date = chrono::DateTime::parse_from_rfc3339(end_date_str).ok()?.with_timezone(&chrono::Utc);
        let hours_until_resolve = (end_date - *now).num_hours();
        
        // Skip if already resolved or more than 1 hour away
        if hours_until_resolve < 0 || hours_until_resolve > 1 {
            return None;
        }
        
        // Get token IDs
        let token_ids_str = market["clobTokenIds"].as_str()?;
        let token_ids: Vec<String> = serde_json::from_str(token_ids_str).ok()?;
        
        if token_ids.len() < 2 || token_ids[0].is_empty() || token_ids[1].is_empty() {
            return None;
        }
        
        let mut tokens = vec![];
        for (i, token_id) in token_ids.iter().take(2).enumerate() {
            let outcome = if i == 0 { "yes" } else { "no" };
            tokens.push(crate::websocket::TokenData {
                token_id: token_id.clone(),
                outcome: outcome.to_string(),
                price: None,
            });
        }
        
        Some(SimplifiedMarket {
            condition_id: market["conditionId"].as_str().unwrap_or("").to_string(),
            question: market["question"].as_str().unwrap_or("").to_string(),
            tokens,
            active: market["active"].as_bool().unwrap_or(false),
            closed: market["closed"].as_bool().unwrap_or(true),
            hours_until_resolve,
            slug: slug.to_string(),
        })
    }
    
    /// Check API health
    pub async fn health_check(&self) -> Result<bool> {
        use reqwest::Client;
        let client = Client::new();
        match client.get(&self.gamma_url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

impl Default for PolyClient {
    fn default() -> Self {
        Self::new()
    }
}
