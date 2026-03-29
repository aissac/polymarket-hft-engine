//! Stop-Loss Handler for Partial Fills
//! 
//! Implements 3-second timer for unhedged Maker legs.
//! Executes FAK (Fill-And-Kill) orders to close exposure.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use serde_json::{json, Value};
use reqwest::Client;

// use crate::signing::{sign_polymarket_order, generate_salt};

use crate::execution::{submit_order_with_backoff, build_l2_headers, fetch_fee_rate};
use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;

/// Tracks pending hedges after Maker fills
pub struct ExecutionState {
    /// Maps maker_order_id -> (taker_token_id, remaining_taker_size)
    pub pending_hedges: DashMap<String, (String, u64)>,
    /// Maps taker_order_id -> maker_order_id (to route partial fills)
    pub taker_to_maker_map: DashMap<String, String>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            pending_hedges: DashMap::new(),
            taker_to_maker_map: DashMap::new(),
        }
    }
}

/// Start stop-loss timer when Maker order fills
pub fn start_stop_loss_timer(
    maker_order_id: String,
    taker_token_id: String,
    remaining_size: u64,
    state: Arc<ExecutionState>,
    client: Arc<Client>,
    signer: Arc<PrivateKeySigner>,
    maker_address: Address,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
) {
    tokio::spawn(async move {
        // Wait exactly 3 seconds
        sleep(Duration::from_secs(3)).await;

        // Check if Taker still has remaining size
        if let Some((taker_token, remaining)) = state.pending_hedges.get(&maker_order_id).map(|r| r.clone()) {
            if remaining > 0 {
                // Taker failed or partially filled - execute FAK stop-loss
                println!(
                    "🚨 STOP-LOSS: Market buying {} micro-USDC of {}",
                    remaining, taker_token
                );

                match execute_fak_order(
                    &client,
                    &signer,
                    maker_address,
                    &taker_token,
                    remaining,
                    &api_key,
                    &api_secret,
                    &api_passphrase,
                )
                .await
                {
                    Ok(_) => {
                        println!("✅ Stop-loss executed successfully");
                        state.pending_hedges.remove(&maker_order_id);
                    }
                    Err(e) => {
                        eprintln!("❌ Stop-loss failed: {}", e);
                    }
                }
            }
        }
    });
}

/// Execute FAK (Fill-And-Kill) order for stop-loss
/// 
/// Prices aggressively at $0.99 to guarantee fill across the spread.
pub async fn execute_fak_order(
    client: &Client,
    signer: &PrivateKeySigner,
    maker: Address,
    token_id: &str,
    size: u64,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
) -> Result<Value, String> {
    // Fetch dynamic fee rate
    let fee_rate = fetch_fee_rate(client, token_id).await.unwrap_or(180);

    // Create order - price aggressively at $0.99 to cross spread
    // makerAmount = 0.99 * size (we're willing to pay up to $0.99 per share)
    let maker_amount = (990_000 * size) / 1_000_000;
    let taker_amount = size;

    let order = create_order(
        maker,
        signer.address(),
        Address::ZERO,
        token_id,
        maker_amount,
        taker_amount,
        fee_rate,
        0, // BUY
    );

    // Sign the order
    let signature = sign_polymarket_order(&order, signer)?;

    // Build JSON payload with FAK order type
    let payload = json!({
        "salt": order.salt.to_string(),
        "maker": format!("{:?}", order.maker),
        "signer": format!("{:?}", order.signer),
        "taker": format!("{:?}", order.taker),
        "tokenId": token_id,
        "makerAmount": order.makerAmount.to_string(),
        "takerAmount": order.takerAmount.to_string(),
        "expiration": "0",
        "nonce": "0",
        "feeRateBps": order.feeRateBps.to_string(),
        "side": 0,
        "signatureType": 2,
        "signature": signature,
        "orderType": "FAK"  // CRITICAL: Fill-And-Kill
    });

    // Build L2 headers
    let body_str = payload.to_string();
    let headers = build_l2_headers(
        api_key,
        api_secret,
        api_passphrase,
        &format!("{:?}", signer.address()),
        "POST",
        "/order",
        &body_str,
    );

    // Submit order
    submit_order_with_backoff(client, payload, headers).await
}

/// Handle WebSocket trade event
/// 
/// Returns true if this is a Maker fill that needs stop-loss timer
pub fn handle_trade_event(
    event: &Value,
    state: Arc<ExecutionState>,
    client: Arc<Client>,
    signer: Arc<PrivateKeySigner>,
    maker_address: Address,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
) -> bool {
    if event["event"] != "trade" || event["status"] != "MATCHED" {
        return false;
    }

    let order_id = event["order_id"].as_str().unwrap_or("").to_string();
    let filled_size = event["size"]
        .as_str()
        .unwrap_or("0")
        .parse::<u64>()
        .unwrap_or(0);
    let asset_id = event["asset_id"].as_str().unwrap_or("").to_string();

    // Check if this is a TAKER order fill
    if let Some(maker_id) = state.taker_to_maker_map.get(&order_id) {
        if let Some(mut hedge_data) = state.pending_hedges.get_mut(maker_id.key()) {
            // Subtract the filled amount from remaining
            hedge_data.1 = hedge_data.1.saturating_sub(filled_size);
            println!(
                "📊 Taker fill: {} micro-USDC, remaining: {}",
                filled_size, hedge_data.1
            );
        }
        return false;
    }

    // Check if this is a MAKER order fill - start stop-loss timer!
    if state.pending_hedges.contains_key(&order_id) {
        let (taker_token, remaining_size) = state
            .pending_hedges
            .get(&order_id)
            .map(|r| r.clone())
            .unwrap();

        println!(
            "📊 Maker filled: {} micro-USDC of {}",
            filled_size, taker_token
        );

        // Start 3-second stop-loss timer
        start_stop_loss_timer(
            order_id,
            taker_token,
            remaining_size,
            state,
            client,
            signer,
            maker_address,
            api_key,
            api_secret,
            api_passphrase,
        );

        return true;
    }

    false
}

/// Record a new hedge pair after order submission
pub fn record_hedge_pair(
    state: Arc<ExecutionState>,
    maker_order_id: String,
    taker_order_id: String,
    taker_token_id: String,
    taker_size: u64,
) {
    // Map taker -> maker for fill routing
    state.taker_to_maker_map.insert(taker_order_id, maker_order_id.clone());

    // Track pending hedge
    state.pending_hedges.insert(maker_order_id.clone(), (taker_token_id.clone(), taker_size));

    println!(
        "📝 Hedge recorded: Maker={} Taker={} Size={}",
        maker_order_id, taker_token_id.clone(), taker_size
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_state() {
        let state = ExecutionState::new();
        assert!(state.pending_hedges.is_empty());
        assert!(state.taker_to_maker_map.is_empty());
    }
}