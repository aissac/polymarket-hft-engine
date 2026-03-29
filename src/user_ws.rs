//! User WebSocket for Trade Events
//! 
//! Listens for MATCHED, MINED, and CONFIRMED events to trigger stop-loss and CTF merge.

use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;

use crate::state::ExecutionState;
use crate::merge_worker::MergeTask;

const USER_WS_URL: &str = "wss://user-ws.polymarket.com/ws";

/// Run the User WebSocket listener
/// Handles trade events and coordinates stop-loss and CTF merge
pub async fn run_user_ws(
    state: Arc<ExecutionState>,
    merge_sender: tokio::sync::mpsc::Sender<MergeTask>,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
) -> Result<(), String> {
    println!("🔌 Connecting to User WebSocket: {}", USER_WS_URL);

    // Build auth payload for WebSocket
    let auth_payload = json!({
        "type": "auth",
        "api_key": api_key,
        "api_secret": api_secret,
        "api_passphrase": api_passphrase,
    });

    // Connect to WebSocket
    let (ws_stream, _) = connect_async(USER_WS_URL)
        .await
        .map_err(|e| format!("WebSocket connect error: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    // Send auth
    write
        .send(Message::Text(auth_payload.to_string()))
        .await
        .map_err(|e| format!("Auth send error: {}", e))?;

    println!("✅ User WebSocket authenticated");

    // Listen for events
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let event: serde_json::Value = serde_json::from_str(&text)
                    .unwrap_or_else(|_| json!({"raw": text}));

                handle_event(&event, &state, &merge_sender).await;
            }
            Ok(Message::Ping(data)) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Ok(Message::Close(_)) => {
                println!("User WebSocket closed, reconnecting...");
                break;
            }
            Err(e) => {
                println!("User WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Handle a trade event from User WebSocket
async fn handle_event(
    event: &serde_json::Value,
    state: &Arc<ExecutionState>,
    merge_sender: &tokio::sync::mpsc::Sender<MergeTask>,
) {
    let event_type = event.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
    let order_id = event.get("order_id").and_then(|v| v.as_str()).unwrap_or("");
    
    match event_type {
        "MATCHED" => {
            // Order matched on CLOB, start 3-second stop-loss timer
            println!("📋 Order {} MATCHED", order_id);
            
            // If this is a Maker order, start stop-loss timer
            if let Some(_ctx) = state.pending_hedges.get(order_id) {
                println!("⏱️ Starting 3s stop-loss timer for Maker: {}", order_id);
                // Stop-loss timer is started by the executor
            }
        }
        "MINED" => {
            // Order confirmed on Polygon
            println!("✅ Order {} MINED", order_id);
            
            // Update state
            if let Some(mut ctx) = state.pending_hedges.get_mut(order_id) {
                // This is a Maker order - mark it as mined
                ctx.maker_mined = true;
                println!("  Maker mined, taker_remaining: {}", ctx.remaining_taker_size);
            } else if let Some(maker_id) = state.taker_to_maker.get(order_id) {
                // This is a Taker order - mark it as mined and check for merge
                if let Some(mut ctx) = state.pending_hedges.get_mut(maker_id.as_str()) {
                    ctx.taker_mined = true;
                    println!("  Taker mined, checking for merge...");
                    
                    // If both legs mined, queue for CTF merge
                    if ctx.maker_mined && ctx.taker_mined {
                        println!("🔄 Both legs MINED, queuing for CTF merge");
                        let _ = merge_sender.send(MergeTask {
                            condition_id: format!("{:?}", ctx.condition_id),
                            amount: ctx.target_size,
                        }).await;
                    }
                }
            }
        }
        "CONFIRMED" => {
            // Final Polygon finality
            println!("🎉 Order {} CONFIRMED", order_id);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_url() {
        assert!(USER_WS_URL.starts_with("wss://"));
    }
}