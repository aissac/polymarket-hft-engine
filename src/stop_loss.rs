//! Stop-Loss Handler for Partial Fills
//! 
//! Implements 3-second timer for unhedged Maker legs.
//! Executes FAK (Fill-And-Kill) orders to close exposure.

use std::sync::Arc;
use tokio::time::{sleep, Duration};
use serde_json::Value;
use reqwest::Client;

use crate::execution::{fetch_fee_rate, create_order_payload, generate_salt};
use crate::state::ExecutionState;
use alloy_primitives::Address;
use alloy_signer_local::PrivateKeySigner;

const STOP_LOSS_TIMEOUT_SECS: u64 = 3;

/// Start a 3-second stop-loss timer for a Maker order
/// If the Taker leg doesn't fill within 3 seconds, execute a FAK order
pub async fn start_stop_loss_timer(
    maker_order_id: String,
    taker_token_id: String,
    taker_size: u64,
    state: Arc<ExecutionState>,
    client: Client,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    signer_address: String,
    signer: PrivateKeySigner,
    dry_run: bool,
) -> Result<(), String> {
    // Wait for Taker fill
    sleep(Duration::from_secs(STOP_LOSS_TIMEOUT_SECS)).await;

    // Check if Taker filled
    let filled = state.pending_hedges.get(&maker_order_id)
        .map(|ctx| ctx.remaining_taker_size == 0)
        .unwrap_or(false);

    if filled {
        println!("✅ Taker filled within {}s, no stop-loss needed", STOP_LOSS_TIMEOUT_SECS);
        return Ok(());
    }

    println!("⚠️ Taker did NOT fill within {}s, executing FAK stop-loss", STOP_LOSS_TIMEOUT_SECS);

    // Execute FAK order to close position
    execute_fak_order(
        taker_token_id,
        taker_size,
        client,
        api_key,
        api_secret,
        api_passphrase,
        signer_address,
        signer,
        dry_run,
    ).await
}

/// Execute a FAK (Fill-And-Kill) order to close unhedged position
pub async fn execute_fak_order(
    token_id: String,
    size: u64,
    client: Client,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    signer_address: String,
    _signer: PrivateKeySigner,
    dry_run: bool,
) -> Result<(), String> {
    // Fetch current fee rate
    let fee_rate = fetch_fee_rate(&client, &token_id).await?;
    
    // For stop-loss, we cross the spread aggressively
    // BUY at $0.99 (or SELL at $0.01) to ensure immediate fill
    let price = 0.99; // Willing to pay 99 cents for YES
    
    // Create order payload
    let maker = Address::default(); // TODO: Get from config
    let taker = Address::ZERO; // Anyone can fill
    
    let order_payload = create_order_payload(
        maker,
        maker,
        taker,
        &token_id,
        size, // makerAmount
        (size as f64 / price) as u64, // takerAmount (shares)
        fee_rate,
        0, // side: BUY
        generate_salt(),
        0, // never expires
        0, // nonce
    );

    if dry_run {
        println!("[DRY_RUN] Would execute FAK order:");
        println!("  Token: {}", token_id);
        println!("  Size: {}", size);
        println!("  Price: ${}", price);
        println!("  Fee: {} bps", fee_rate);
        return Ok(());
    }

    // TODO: Sign order and submit
    // For now, log the intent
    println!("🚨 STOP-LOSS: Execute FAK for token {} size {}", token_id, size);
    
    Ok(())
}

/// Handle trade events from User WebSocket
/// Updates state when Maker or Taker fills
pub fn handle_trade_event(
    event: &Value,
    state: &Arc<ExecutionState>,
) {
    let event_type = event.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
    let order_id = event.get("order_id").and_then(|v| v.as_str()).unwrap_or("");
    
    match event_type {
        "MATCHED" => {
            // Order matched on CLOB, waiting for on-chain confirmation
            println!("📋 Order {} MATCHED", order_id);
        }
        "MINED" => {
            // Order confirmed on Polygon
            println!("✅ Order {} MINED", order_id);
            
            // Update state if this is a Maker or Taker order
            if let Some(mut ctx) = state.pending_hedges.get_mut(order_id) {
                // This is a Maker order - mark it as mined
                ctx.maker_mined = true;
            } else if let Some(maker_id) = state.taker_to_maker.get(order_id) {
                // This is a Taker order - mark it as mined
                if let Some(mut ctx) = state.pending_hedges.get_mut(maker_id.as_str()) {
                    ctx.taker_mined = true;
                }
            }
        }
        "CONFIRMED" => {
            // Final confirmation
            println!("🎉 Order {} CONFIRMED", order_id);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stop_loss_timeout() {
        assert_eq!(STOP_LOSS_TIMEOUT_SECS, 3);
    }
}