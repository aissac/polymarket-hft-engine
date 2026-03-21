//! Polymarket REST API Client
//! 
//! Uses polymarket-client-sdk for proper compression handling.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::debug;

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
    
    /// Get token ID
    pub fn token_id(&self) -> &str {
        &self.token_id
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
    
    /// Get YES token ID
    pub fn yes_token_id(&self) -> String {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "yes")
            .map(|t| t.token_id.clone())
            .unwrap_or_default()
    }
    
    /// Get NO token ID
    pub fn no_token_id(&self) -> String {
        self.tokens.iter()
            .find(|t| t.outcome.to_lowercase() == "no")
            .map(|t| t.token_id.clone())
            .unwrap_or_default()
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

/// Polymarket REST API Client (unauthenticated, read-only)
pub struct PolyClient;

impl PolyClient {
    pub fn new() -> Self {
        Self
    }
    
    /// Get simplified markets via SDK (handles compression)
    pub async fn get_markets(&self) -> Result<Vec<SimplifiedMarket>> {
        use polymarket_client_sdk::clob::Client as ClobClient;
        use polymarket_client_sdk::clob::Config as ClobConfig;
        
        let client = ClobClient::new("https://clob.polymarket.com", ClobConfig::default())?;
        
        debug!("Fetching markets via SDK...");
        
        // Use SDK's simplified_markets endpoint
        let result = client.simplified_markets(None).await?;
        
        // Convert to our SimplifiedMarket format
        let simplified: Vec<SimplifiedMarket> = result.data
            .into_iter()
            .filter_map(|m| {
                // Extract YES/NO tokens
                let mut yes_price = None;
                let mut no_price = None;
                let mut yes_token_id = None;
                let mut no_token_id = None;
                
                for token in m.tokens {
                    let price: Option<f64> = token.price.to_string().parse().ok();
                    
                    if token.outcome.to_lowercase() == "yes" {
                        yes_price = price;
                        yes_token_id = Some(token.token_id);
                    } else if token.outcome.to_lowercase() == "no" {
                        no_price = price;
                        no_token_id = Some(token.token_id);
                    }
                }
                
                match (yes_price, no_price, yes_token_id, no_token_id) {
                    (Some(yp), Some(np), Some(ytid), Some(ntid)) => {
                        Some(SimplifiedMarket {
                            condition_id: m.condition_id,
                            tokens: vec![
                                TokenData {
                                    token_id: ytid,
                                    outcome: "Yes".to_string(),
                                    price_raw: serde_json::Value::Number(
                                        serde_json::Number::from_f64(yp).unwrap_or(serde_json::Number::from(0))
                                    ),
                                },
                                TokenData {
                                    token_id: ntid,
                                    outcome: "No".to_string(),
                                    price_raw: serde_json::Value::Number(
                                        serde_json::Number::from_f64(np).unwrap_or(serde_json::Number::from(0))
                                    ),
                                },
                            ],
                            active: m.active,
                            closed: m.closed,
                        })
                    }
                    _ => None,
                }
            })
            .take(20) // Limit to 20 markets
            .collect();
        
        Ok(simplified)
    }
    
    /// Check API health
    pub async fn health_check(&self) -> Result<bool> {
        use polymarket_client_sdk::clob::Client as ClobClient;
        use polymarket_client_sdk::clob::Config as ClobConfig;
        
        let client = ClobClient::new("https://clob.polymarket.com", ClobConfig::default())?;
        
        // Use None for cursor to get first page
        match client.simplified_markets(None).await {
            Ok(_) => Ok(true),
            Err(e) => {
                debug!("Health check error: {}", e);
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
