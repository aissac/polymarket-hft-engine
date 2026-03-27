//! Poly Merger - Async capital recycling for high-frequency trading
//! Merges YES+NO shares and redeems resolved positions to free capital

use reqwest;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time;
use tracing::{info, warn, error, debug};

const MERGE_INTERVAL_SECS: u64 = 30;
const CLOB_BASE: &str = "https://clob.polymarket.com";
const DATA_BASE: &str = "https://data-api.polymarket.com";

#[derive(Debug, Serialize, Deserialize)]
struct Position {
    condition_id: String,
    #[serde(default)]
    yes_shares: Option<String>,
    #[serde(default)]
    no_shares: Option<String>,
    #[serde(default)]
    resolved: Option<bool>,
}

#[derive(Debug, Serialize)]
struct MergeRequest {
    condition_id: String,
    amount: String,
}

#[derive(Debug, Serialize)]
struct RedeemRequest {
    condition_id: String,
}

pub struct PolyMerger {
    client: reqwest::Client,
    api_key: String,
    wallet: String,
}

impl PolyMerger {
    pub fn new(api_key: String, wallet: String, _dry_run: bool) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            wallet,
        }
    }

    /// Fetch mergeable positions
    async fn get_positions(&self) -> Result<Vec<Position>, reqwest::Error> {
        let url = format!("{}/positions", DATA_BASE);
        
        let resp = self.client
            .get(&url)
            .header("Authorization", &self.api_key)
            .query(&[("user", &self.wallet), ("mergeable", &"true".to_string())])
            .send()
            .await?
            .json::<Vec<Position>>()
            .await?;
        
        Ok(resp)
    }

    /// Merge YES+NO shares for a condition
    async fn merge(&self, condition_id: &str, yes: u64, no: u64) -> bool {
        let amount = yes.min(no);
        if amount == 0 {
            return false;
        }

        let url = format!("{}/merge", CLOB_BASE);
        let payload = MergeRequest {
            condition_id: condition_id.to_string(),
            amount: amount.to_string(),
        };

        match self.client
            .post(&url)
            .header("Authorization", &self.api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    info!("🔄 MERGE | {} | {} shares", &condition_id[..8.min(condition_id.len())], amount);
                    true
                } else {
                    warn!("Merge failed for {}: {:?}", &condition_id[..8], resp.status());
                    false
                }
            }
            Err(e) => {
                error!("Merge error for {}: {}", &condition_id[..8], e);
                false
            }
        }
    }

    /// Redeem resolved shares
    async fn redeem(&self, condition_id: &str) -> bool {
        let url = format!("{}/redeem", CLOB_BASE);
        let payload = RedeemRequest {
            condition_id: condition_id.to_string(),
        };

        match self.client
            .post(&url)
            .header("Authorization", &self.api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    info!("💰 REDEEM | {}", &condition_id[..8.min(condition_id.len())]);
                    true
                } else {
                    warn!("Redeem failed for {}: {:?}", &condition_id[..8], resp.status());
                    false
                }
            }
            Err(e) => {
                error!("Redeem error for {}: {}", &condition_id[..8], e);
                false
            }
        }
    }

    /// Run merger loop asynchronously
    pub async fn run_loop(self) {
        info!("🔄 Poly Merger started - checking every {}s", MERGE_INTERVAL_SECS);
        
        let mut interval = time::interval(Duration::from_secs(MERGE_INTERVAL_SECS));
        
        loop {
            interval.tick().await;
            
            match self.get_positions().await {
                Ok(positions) => {
                    if positions.is_empty() {
                        debug!("No mergeable positions");
                    } else {
                        info!("Found {} mergeable position(s)", positions.len());
                        
                        for pos in positions {
                            let yes = pos.yes_shares
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);
                            let no = pos.no_shares
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);
                            
                            if yes > 0 && no > 0 {
                                self.merge(&pos.condition_id, yes, no).await;
                            } else if pos.resolved.unwrap_or(false) {
                                self.redeem(&pos.condition_id).await;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to fetch positions: {}", e);
                }
            }
        }
    }
}
