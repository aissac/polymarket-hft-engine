//! Maker/Taker Order Routing (NotebookLM Fix #3)
//! 
//! Implements volatility-based routing:
//! - Token with MORE depth = stable = Maker (GTC)
//! - Token with LESS depth = volatile = Taker (FAK)

use crate::execution::{create_order_payload, submit_order, fetch_fee_rate, build_hft_client, pre_warm_connections};
use crate::state::ExecutionState;
use std::sync::Arc;
use reqwest::Client;

/// Volatility-based Maker/Taker routing
/// 
/// Heuristic: The token with LESS depth is more "volatile" (takes less capital to sweep).
/// Make the thicker leg the Maker (GTC), and aggressively take the thinner leg (FAK).
pub fn determine_maker_taker(
    yes_token: &str,
    no_token: &str,
    yes_depth: u64,
    no_depth: u64,
) -> (MakerTakerAssignment, MakerTakerAssignment) {
    // YES has more depth → YES is stable (Maker), NO is volatile (Taker)
    if yes_depth > no_depth {
        (
            MakerTakerAssignment {
                token_id: yes_token.to_string(),
                side: 0, // BUY
                size: yes_depth,
                order_type: "GTC",
            },
            MakerTakerAssignment {
                token_id: no_token.to_string(),
                side: 0, // BUY
                size: no_depth,
                order_type: "FAK",
            },
        )
    } else {
        // NO has more depth → NO is stable (Maker), YES is volatile (Taker)
        (
            MakerTakerAssignment {
                token_id: no_token.to_string(),
                side: 0, // BUY
                size: no_depth,
                order_type: "GTC",
            },
            MakerTakerAssignment {
                token_id: yes_token.to_string(),
                side: 0, // BUY
                size: yes_depth,
                order_type: "FAK",
            },
        )
    }
}

#[derive(Debug, Clone)]
pub struct MakerTakerAssignment {
    pub token_id: String,
    pub side: u8,
    pub size: u64,
    pub order_type: &'static str, // "GTC" or "FAK"
}

/// Execute arbitrage pair with Maker/Taker routing
/// 
/// Flow:
/// 1. Detect which leg is volatile vs stable
/// 2. Submit Maker (GTC) order on stable leg
/// 3. Wait for MATCHED event from User WS
/// 4. Submit Taker (FAK) order on volatile leg
/// 5. If Taker ghosts after 3s, execute stop-loss FAK
pub async fn execute_arbitrage_pair(
    yes_token: &str,
    no_token: &str,
    yes_depth: u64,
    no_depth: u64,
    combined_price: u64,
    client: &Client,
    state: &Arc<ExecutionState>,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
    signer_address: &str,
    dry_run: bool,
) -> Result<(), String> {
    // Step 1: Determine Maker/Taker assignment based on depth
    let (maker, taker) = determine_maker_taker(yes_token, no_token, yes_depth, no_depth);
    
    println!("[EXEC] Maker: {} {} ({}), Taker: {} {} ({})",
        maker.order_type, maker.token_id, maker.size,
        taker.order_type, taker.token_id, taker.size
    );
    
    // Step 2: Fetch fee rate for Maker token
    let fee_rate = fetch_fee_rate(client, &maker.token_id).await?;
    
    // Step 3: Create Maker order (GTC)
    let maker_payload = create_order_payload(
        alloy_primitives::Address::default(), // TODO: Get from config
        alloy_primitives::Address::default(), // TODO: signer
        alloy_primitives::Address::ZERO,     // Anyone can fill
        &maker.token_id,
        maker.size,
        (maker.size as f64 / (combined_price as f64 / 1_000_000.0)) as u64, // Size / price
        fee_rate,
        maker.side,
        crate::execution::generate_salt(),
        0, // Never expires for GTC
        0, // Nonce
        maker.order_type,
    );
    
    // Step 4: Submit Maker order
    // In DRY_RUN, this returns immediately
    let maker_result = submit_order(
        client,
        &maker_payload,
        "signature_placeholder", // TODO: Actually sign
        api_key,
        api_secret,
        api_passphrase,
        signer_address,
        dry_run,
        3, // max_retries
    ).await?;
    
    let maker_order_id = maker_result.get("order_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    
    println!("[EXEC] Maker order submitted: {}", maker_order_id);
    
    // Step 5: Register in ExecutionState for User WS tracking
    state.pending_hedges.insert(maker_order_id.to_string(), crate::state::HedgeContext {
        taker_token_id: taker.token_id.clone(),
        condition_id: alloy_primitives::B256::ZERO, // TODO: Lookup from condition_map
        target_size: maker.size,
        remaining_taker_size: taker.size,
        maker_mined: false,
        taker_mined: false,
    });
    
    // Step 6: Taker order will be submitted by User WS when Maker MATCHED event arrives
    // (See user_ws.rs for Taker FAK submission)
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_maker_taker_routing() {
        // YES has more depth → YES is Maker (stable)
        let (maker, taker) = determine_maker_taker("yes_token", "no_token", 1000, 500);
        assert_eq!(maker.token_id, "yes_token");
        assert_eq!(maker.order_type, "GTC");
        assert_eq!(taker.token_id, "no_token");
        assert_eq!(taker.order_type, "FAK");
        
        // NO has more depth → NO is Maker (stable)
        let (maker, taker) = determine_maker_taker("yes_token", "no_token", 500, 1000);
        assert_eq!(maker.token_id, "no_token");
        assert_eq!(maker.order_type, "GTC");
        assert_eq!(taker.token_id, "yes_token");
        assert_eq!(taker.order_type, "FAK");
    }
}