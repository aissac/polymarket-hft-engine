//! Polymarket API Client Module
//! 
//! Handles market data fetching from Gamma API (active markets only)

use anyhow::Result;
use tracing::{debug, info};

pub use crate::websocket::TokenData;

/// Simplified market representation
#[derive(Debug, Clone)]
pub struct SimplifiedMarket {
    pub condition_id: String,
    pub tokens: Vec<TokenData>,
    pub active: bool,
    pub closed: bool,
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

impl PolyClient {
    pub fn new() -> Self {
        Self {
            gamma_url: "https://gamma-api.polymarket.com".to_string(),
        }
    }
    
    /// Get active markets from Gamma API
    pub async fn get_markets(&self) -> Result<Vec<SimplifiedMarket>> {
        use reqwest::Client;
        
        debug!("Fetching active markets from Gamma API...");
        
        // Use Gamma API to get ACTIVE markets (filter by closed=false&active=true)
        let client = Client::new();
        let resp = client
            .get(format!("{}/markets?closed=false&active=true&limit=100", self.gamma_url))
            .send()
            .await?
            .json::<Vec<serde_json::Value>>()
            .await?;
        
        info!("Gamma API returned {} markets", resp.len());
        
        // Filter for active markets with clob token IDs
        let mut simplified: Vec<SimplifiedMarket> = vec![];
        
        for market in resp.iter().take(50) {
            // Skip if no clob token IDs
            let token_ids_str = match market["clobTokenIds"].as_str() {
                Some(s) => s,
                None => continue,
            };
            
            // Parse the token IDs
            let token_ids = match serde_json::from_str::<Vec<String>>(token_ids_str) {
                Ok(ids) => ids,
                Err(_) => continue,
            };
            
            // Need at least 2 tokens (YES + NO)
            if token_ids.len() < 2 {
                continue;
            }
            
            // Skip if both tokens are empty
            if token_ids[0].is_empty() || token_ids[1].is_empty() {
                continue;
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
            
            simplified.push(SimplifiedMarket {
                condition_id: market["conditionId"].as_str().unwrap_or("").to_string(),
                tokens,
                active: market["active"].as_bool().unwrap_or(false),
                closed: market["closed"].as_bool().unwrap_or(true),
            });
        }
        
        info!("Found {} active markets with token IDs", simplified.len());
        
        Ok(simplified)
    }
    
    /// Check API health
    pub async fn health_check(&self) -> Result<bool> {
        use reqwest::Client;
        
        let client = Client::new();
        match client.get(&self.gamma_url).send().await {
            Ok(_) => Ok(true),
            Err(e) => {
                info!("Gamma API health check failed: {}", e);
                Ok(false)
            }
        }
    }
}

impl Default for PolyClient {
    fn default() -> Self {
        Self::new()
    }
}
