//! Polymarket REST API Client
//! 
//! Fetches markets, orderbooks, and prices from Polymarket CLOB API.

use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, warn};

/// Polymarket API base URL
const API_BASE: &str = "https://clob.polymarket.com";

/// HTTP client with sensible defaults
pub fn create_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("gabagool-bot/0.1")
        .build()
        .expect("Failed to create HTTP client")
}

/// Token price data from simplified-markets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    pub token_id: String,
    pub outcome: String,
    #[serde(flatten)]
    pub price_raw: Value,
}

impl TokenData {
    /// Get price as f64 (handles string or number)
    pub fn price(&self) -> Option<f64> {
        match &self.price_raw {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }
}

/// Market from simplified-markets endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
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
            .and_then(|t| t.price())
    }
    
    /// Get NO token price
    pub fn no_price(&self) -> Option<f64> {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "no")
            .and_then(|t| t.price())
    }
    
    /// Combined cost (YES + NO)
    pub fn combined_cost(&self) -> Option<f64> {
        match (self.yes_price(), self.no_price()) {
            (Some(yes), Some(no)) => Some(yes + no),
            _ => None,
        }
    }
    
    /// Check if arbitrage exists
    pub fn has_arbitrage(&self, target: f64) -> bool {
        self.combined_cost()
            .map(|c| c < target && !self.closed)
            .unwrap_or(false)
    }
}

/// API response wrapper
#[derive(Debug, Deserialize)]
pub struct MarketsResponse {
    pub data: Vec<SimplifiedMarket>,
}

/// Polymarket REST API Client
pub struct PolyClient {
    client: Client,
    base_url: String,
}

impl PolyClient {
    pub fn new() -> Self {
        Self {
            client: create_client(),
            base_url: API_BASE.to_string(),
        }
    }
    
    /// Get simplified markets (faster, less data)
    pub async fn get_markets(&self) -> Result<Vec<SimplifiedMarket>> {
        let url = format!("{}/sampling-simplified-markets?limit=100", self.base_url);
        
        debug!("Fetching markets from: {}", url);
        
        let response = self.client.get(&url)
            .send()
            .await?;
        
        if !response.status().is_success() {
            return Err(anyhow!("API returned status: {}", response.status()));
        }
        
        let markets_resp: MarketsResponse = response.json().await?;
        
        // Filter: only active, not closed, has both YES and NO prices
        let markets: Vec<SimplifiedMarket> = markets_resp.data
            .into_iter()
            .filter(|m| {
                m.active && 
                !m.closed && 
                m.yes_price().is_some() && 
                m.no_price().is_some()
            })
            .collect();
        
        Ok(markets)
    }
    
    /// Get orderbook for a specific condition ID
    pub async fn get_orderbook(&self, condition_id: &str) -> Result<(Vec<OrderBookLevel>, Vec<OrderBookLevel>)> {
        let url = format!("{}/orderbook/{}", self.base_url, condition_id);
        
        #[derive(Deserialize)]
        struct OrderBookResponse {
            bids: Vec<PriceLevel>,
            asks: Vec<PriceLevel>,
        }
        
        #[derive(Deserialize)]
        struct PriceLevel {
            price: Value,
            size: Value,
        }
        
        let response = self.client.get(&url)
            .send()
            .await?;
        
        if !response.status().is_success() {
            return Err(anyhow!("Orderbook API returned: {}", response.status()));
        }
        
        let ob: OrderBookResponse = response.json().await?;
        
        let bids: Vec<OrderBookLevel> = ob.bids
            .into_iter()
            .filter_map(|p| {
                let price = match p.price {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                }?;
                let size = match p.size {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                }?;
                Some(OrderBookLevel { price, size })
            })
            .collect();
        
        let asks: Vec<OrderBookLevel> = ob.asks
            .into_iter()
            .filter_map(|p| {
                let price = match p.price {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                }?;
                let size = match p.size {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => s.parse().ok(),
                    _ => None,
                }?;
                Some(OrderBookLevel { price, size })
            })
            .collect();
        
        Ok((bids, asks))
    }
    
    /// Check API health
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/sampling-markets?limit=1", self.base_url);
        
        match self.client.get(&url).send().await {
            Ok(resp) => Ok(resp.status().is_success()),
            Err(_) => Ok(false),
        }
    }
}

/// Orderbook level
#[derive(Debug, Clone)]
pub struct OrderBookLevel {
    pub price: f64,
    pub size: f64,
}

impl Default for PolyClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_token_price() {
        let t = TokenData {
            token_id: "1".to_string(),
            outcome: "Yes".to_string(),
            price_raw: Value::Number(serde_json::Number::from_f64(0.50).unwrap()),
        };
        assert_eq!(t.price(), Some(0.50));
        
        let t = TokenData {
            token_id: "1".to_string(),
            outcome: "Yes".to_string(),
            price_raw: Value::String("0.44".to_string()),
        };
        assert_eq!(t.price(), Some(0.44));
    }
    
    #[test]
    fn test_simplified_market() {
        let m = SimplifiedMarket {
            condition_id: "0x123".to_string(),
            tokens: vec![
                TokenData { 
                    token_id: "1".to_string(), 
                    outcome: "Yes".to_string(), 
                    price_raw: Value::Number(serde_json::Number::from_f64(0.50).unwrap()),
                },
                TokenData { 
                    token_id: "2".to_string(), 
                    outcome: "No".to_string(), 
                    price_raw: Value::Number(serde_json::Number::from_f64(0.44).unwrap()),
                },
            ],
            active: true,
            closed: false,
        };
        
        assert_eq!(m.yes_price(), Some(0.50));
        assert_eq!(m.no_price(), Some(0.44));
        assert_eq!(m.combined_cost(), Some(0.94));
        assert!(m.has_arbitrage(0.95));
        assert!(!m.has_arbitrage(0.90));
    }
}
