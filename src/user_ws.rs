//! User WebSocket Monitor
//! 
//! Monitors trade events for stop-loss triggering and CTF merge dispatching.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};
use futures_util::{StreamExt, SinkExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use serde_json::{json, Value};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::mpsc::Sender;

use crate::state::ExecutionState;
use crate::merge_worker::MergeTask;

/// Generates the HMAC-SHA256 signature for WebSocket authentication
fn generate_ws_signature(api_secret: &str, timestamp: &str) -> String {
    // Message: timestamp + "GET" + "/ws/user"
    let msg = format!("{}GET/ws/user", timestamp);
    let decoded_secret = BASE64.decode(api_secret).expect("Invalid base64 secret");
    
    let mut mac = Hmac::<Sha256>::new_from_slice(&decoded_secret).unwrap();
    mac.update(msg.as_bytes());
    
    BASE64.encode(mac.finalize().into_bytes())
}

/// Run the User WebSocket with automatic reconnection
pub async fn run_user_ws(
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    state: Arc<ExecutionState>,
    merge_tx: Sender<MergeTask>,
) {
    let mut backoff = 1u64;
    let url = "wss://ws-subscriptions-clob.polymarket.com/ws/user";

    loop {
        match connect_async(url).await {
            Ok((mut ws_stream, _)) => {
                println!("✅ [USER_WS] Connected to Polymarket User Stream.");
                backoff = 1; // Reset backoff on success

                // 1. Authenticate
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .to_string();
                let signature = generate_ws_signature(&api_secret, &timestamp);

                let auth_payload = json!({
                    "type": "auth",
                    "api_key": api_key,
                    "signature": signature,
                    "timestamp": timestamp,
                    "passphrase": api_passphrase
                });

                if ws_stream.send(Message::Text(auth_payload.to_string())).await.is_err() {
                    println!("⚠️ [USER_WS] Failed to send auth. Reconnecting...");
                    continue;
                }

                // 2. Event Loop
                while let Some(msg) = ws_stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            process_ws_message(&text, &state, &merge_tx).await;
                        }
                        Ok(Message::Ping(p)) => {
                            // Respond to heartbeat
                            let _ = ws_stream.send(Message::Pong(p)).await;
                        }
                        Ok(Message::Pong(_)) => {
                            // Connection alive
                        }
                        Ok(Message::Close(_)) => {
                            println!("⚠️ [USER_WS] Server closed connection.");
                            break;
                        }
                        Err(e) => {
                            eprintln!("⚠️ [USER_WS] Stream error: {}. Reconnecting...", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "❌ [USER_WS] Connection failed: {}. Retrying in {}s",
                    e, backoff
                );
            }
        }

        // Exponential backoff up to 30 seconds
        sleep(Duration::from_secs(backoff)).await;
        backoff = std::cmp::min(backoff * 2, 30);
    }
}

/// Process incoming WebSocket messages
async fn process_ws_message(
    text: &str,
    state: &Arc<ExecutionState>,
    merge_tx: &Sender<MergeTask>,
) {
    let event: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Only process trade events
    if event["event"] != "trade" {
        return;
    }

    let status = event["status"].as_str().unwrap_or("");
    let order_id = event["order_id"].as_str().unwrap_or("").to_string();
    let size_str = event["size"].as_str().unwrap_or("0");
    let filled_size = size_str.parse::<u64>().unwrap_or(0);
    let asset_id = event["asset_id"].as_str().unwrap_or("").to_string();

    // -- SCENARIO A: TAKER LEG UPDATE --
    if let Some(maker_id) = state.taker_to_maker.get(&order_id) {
        let maker_key = maker_id.clone();

        if let Some(mut ctx) = state.pending_hedges.get_mut(&maker_key) {
            match status {
                "MATCHED" => {
                    // Deduct from remaining Taker size
                    ctx.remaining_taker_size = ctx.remaining_taker_size.saturating_sub(filled_size);
                    println!(
                        "📊 [USER_WS] Taker MATCHED: {} micro-USDC, remaining: {}",
                        filled_size, ctx.remaining_taker_size
                    );
                }
                "MINED" => {
                    ctx.taker_mined = true;
                    println!("✅ [USER_WS] Taker MINED on chain.");
                    check_and_trigger_merge(&maker_key, state, merge_tx).await;
                }
                "CONFIRMED" => {
                    println!("✅ [USER_WS] Taker CONFIRMED (finalized).");
                }
                _ => {}
            }
        }
        return;
    }

    // -- SCENARIO B: MAKER LEG UPDATE --
    if state.pending_hedges.contains_key(&order_id) {
        match status {
            "MATCHED" => {
                println!("⚠️ [USER_WS] Maker leg MATCHED! Starting 3-second Stop-Loss Timer...");

                // Get context for stop-loss
                let (taker_token, remaining) = state
                    .pending_hedges
                    .get(&order_id)
                    .map(|ctx| (ctx.taker_token_id.clone(), ctx.remaining_taker_size))
                    .unwrap_or((String::new(), 0));

                // 💥 3-Second Stop-Loss Timer 💥
                let state_clone = Arc::clone(state);
                let order_id_clone = order_id.clone();

                tokio::spawn(async move {
                    sleep(Duration::from_secs(3)).await;

                    if let Some(ctx) = state_clone.pending_hedges.get(&order_id_clone) {
                        if ctx.remaining_taker_size > 0 {
                            println!(
                                "🚨 [STOP-LOSS] Taker failed to fill completely. Market buying {} shares of {}!",
                                ctx.remaining_taker_size, ctx.taker_token_id
                            );

                            // TODO: Call execute_fak_stop_loss()
                            // This would need access to the CLOB client
                            // execute_fak_stop_loss(&clob_client, &ctx.taker_token_id, ctx.remaining_taker_size).await;
                        }
                    }
                });
            }
            "MINED" => {
                if let Some(mut ctx) = state.pending_hedges.get_mut(&order_id) {
                    ctx.maker_mined = true;
                    println!("✅ [USER_WS] Maker MINED on chain.");
                }
                check_and_trigger_merge(&order_id, state, merge_tx).await;
            }
            "CONFIRMED" => {
                println!("✅ [USER_WS] Maker CONFIRMED (finalized).");
            }
            _ => {}
        }
    }
}

/// Check if both legs are MINED and dispatch CTF merge
async fn check_and_trigger_merge(
    maker_order_id: &str,
    state: &Arc<ExecutionState>,
    merge_tx: &Sender<MergeTask>,
) {
    let should_merge = {
        if let Some(ctx) = state.pending_hedges.get(maker_order_id) {
            if ctx.maker_mined && ctx.taker_mined {
                Some(MergeTask {
                    condition_id: ctx.condition_id,
                    amount: ctx.target_size,
                })
            } else {
                None
            }
        } else {
            None
        }
    };

    if let Some(task) = should_merge {
        println!("✅ [USER_WS] Both legs MINED. Dispatching CTF Merge...");
        let _ = merge_tx.send(task).await;

        // Clean up state (arbitrage lifecycle complete)
        state.cleanup(maker_order_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_signature() {
        // Test that signature generation works
        let sig = generate_ws_signature("dGVzdA==", "1234567890");
        assert!(!sig.is_empty());
    }
}