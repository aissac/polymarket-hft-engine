//! CTF Merge Worker for Capital Recycling
//! 
//! Burns YES+NO shares to reclaim USDC.e collateral without waiting for market resolution.

use alloy_primitives::{Address, B256, U256, Bytes};
use tokio::sync::mpsc::Receiver;
use tokio::time::{sleep, Duration};
use reqwest::Client;
use serde_json::json;

use crate::state::ExecutionState;
use std::sync::Arc;

const RELAYER_URL: &str = "https://relayer.polymarket.com";
const USDC_ADDRESS: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const CTF_ADDRESS: &str = "0x4D97DCd97eC945f40cF65F87097ACe5EA0476045";

// Rate limit: 25 requests per minute = 1 request per 2.4 seconds
const RELAYER_DELAY_MS: u64 = 2400;

/// Task to queue for merge
pub struct MergeTask {
    pub condition_id: String,
    pub amount: u64, // in micro-USDC
}

/// Run the CTF merge worker
/// Listens for MINED orders on User WebSocket and executes CTF merge
pub async fn run_merge_worker(
    mut receiver: Receiver<MergeTask>,
    state: Arc<ExecutionState>,
    client: Client,
    dry_run: bool,
) {
    loop {
        // Wait for next task
        let task = match receiver.recv().await {
            Some(t) => t,
            None => {
                println!("Merge channel closed, exiting worker");
                break;
            }
        };

        println!("🔄 Attempting CTF Merge for condition: {:?}", task.condition_id);

        // Rate limit: 25 RPM = 2.4s delay between requests
        sleep(Duration::from_millis(RELAYER_DELAY_MS)).await;

        if dry_run {
            println!("[DRY_RUN] Would execute CTF merge:");
            println!("  Condition: {}", task.condition_id);
            println!("  Amount: {} USDC", task.amount as f64 / 1_000_000.0);
            continue;
        }

        // Build merge transaction
        let merge_result = execute_ctf_merge(
            &task.condition_id,
            task.amount,
            &client,
        ).await;

        match merge_result {
            Ok(tx_hash) => {
                println!("✅ CTF Merge Successful. Tx: {}", tx_hash);
                
                // Update state - capital recycled
                // In production, track PnL here
            }
            Err(e) => {
                println!("❌ CTF Merge Failed: {}", e);
                // Retry logic could go here
            }
        }
    }
}

/// Execute CTF merge via Relayer
async fn execute_ctf_merge(
    condition_id: &str,
    amount: u64,
    _client: &Client,
) -> Result<String, String> {
    // TODO: Build actual merge transaction
    // For now, return mock hash
    
    // CTF merge calldata:
    // function mergePositions(
    //     address collateralToken,
    //     bytes32 parentCollectionId,
    //     bytes32 conditionId,
    //     uint256[] calldata partition,
    //     uint256 amount
    // )
    
    // The partition for binary markets is always [1, 2]
    // (YES = index 1, NO = index 2)
    
    println!("Building merge for condition: {}", condition_id);
    println!("Amount: {} USDC", amount as f64 / 1_000_000.0);
    
    // Mock successful merge
    Ok(format!("0x{}", condition_id))
}

/// Fetch condition_id from Gamma API
/// Maps token_id to its parent condition_id
pub async fn fetch_condition_id(
    client: &Client,
    token_id: &str,
) -> Result<String, String> {
    let url = format!(
        "https://gamma-api.polymarket.com/markets?token_id={}",
        token_id
    );

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Gamma API error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Gamma API returned: {}", resp.status()));
    }

    let body = resp.text().await.map_err(|e| e.to_string())?;
    let markets: Vec<serde_json::Value> = serde_json::from_str(&body)
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let condition_id = markets
        .first()
        .and_then(|m| m.get("conditionId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No conditionId in response".to_string())?;

    Ok(condition_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relayer_delay() {
        assert_eq!(RELAYER_DELAY_MS, 2400);
    }

    #[test]
    fn test_constants() {
        assert!(!RELAYER_URL.is_empty());
        assert!(!USDC_ADDRESS.is_empty());
        assert!(!CTF_ADDRESS.is_empty());
    }
}