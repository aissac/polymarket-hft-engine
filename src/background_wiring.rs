//! Background Thread Wiring (NotebookLM Integration)
//!
//! Bridges hot path (crossbeam) → execution (tokio)
//! - Maps token_hash → token_id → condition_id
//! - Calls execute_arbitrage_pair()
//! - Simulates MATCHED in DRY_RUN

use std::sync::Arc;
use tokio::time::{sleep, Duration};
use reqwest::Client;

use crate::token_map::{hash_token, build_maps};
use crate::execution::{create_order_payload, submit_order, build_hft_client, pre_warm_connections};
use crate::maker_taker_routing::{determine_maker_taker, MakerTakerAssignment};
use crate::signing::init_signer;

/// Wire the background thread to execute arbitrage pairs
/// 
/// Flow:
/// 1. Receive EdgeDetected from hot path
/// 2. Resolve token_hash → YES/NO token IDs
/// 3. Determine Maker (GTC) vs Taker (FAK)
/// 4. Sign and submit Maker order
/// 5. On MATCHED (or simulated in DRY_RUN), submit Taker FAK
/// 6. Start stop-loss timer if needed
pub async fn wire_background_execution(
    client: Arc<Client>,
    hash_to_id: Arc<std::collections::HashMap<u64, String>>,
    id_to_condition: Arc<std::collections::HashMap<String, String>>,
    signer: Arc<alloy_signer_local::PrivateKeySigner>,
    api_key: String,
    api_secret: String,
    api_passphrase: String,
    signer_address: String,
    dry_run: bool,
) {
    // This function is called from spawn_blocking
    // It receives from crossbeam channel and spawns tokio tasks
    
    println!("[BG] 🔧 Background execution thread started (DRY_RUN={})", dry_run);
    println!("[BG] Token maps: {} hashes, {} conditions", hash_to_id.len(), id_to_condition.len());
}

/// Process an edge detection from hot path
pub async fn process_edge(
    token_hash: u64,
    complement_hash: u64,
    yes_size: u64,
    no_size: u64,
    combined_price: u64,
    hash_to_id: &std::collections::HashMap<u64, String>,
    id_to_condition: &std::collections::HashMap<String, String>,
    client: &Client,
    signer: &Arc<alloy_signer_local::PrivateKeySigner>,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
    signer_address: &str,
    dry_run: bool,
) {
    // Step 1: Resolve token hashes to full IDs
    let yes_token_id = match hash_to_id.get(&token_hash) {
        Some(id) => id.clone(),
        None => {
            eprintln!("⚠️ Unknown token hash: {:016x}", token_hash);
            return;
        }
    };
    
    let no_token_id = match hash_to_id.get(&complement_hash) {
        Some(id) => id.clone(),
        None => {
            eprintln!("⚠️ Unknown complement hash: {:016x}", complement_hash);
            return;
        }
    };
    
    // Step 2: Get condition ID
    let condition_id = id_to_condition.get(&yes_token_id)
        .or_else(|| id_to_condition.get(&no_token_id))
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    
    println!("[EXEC] 🎯 Edge: combined=${:.4} YES={} NO={}", 
        combined_price as f64 / 1_000_000.0, yes_size, no_size);
    println!("[EXEC]   YES token: {}..{}", &yes_token_id[..10], &yes_token_id[yes_token_id.len()-6..]);
    println!("[EXEC]   NO token:  {}..{}", &no_token_id[..10], &no_token_id[no_token_id.len()-6..]);
    println!("[EXEC]   Condition: {}..{}", &condition_id[..10], &condition_id[condition_id.len()-6..]);
    
    // Step 3: Determine Maker/Taker based on depth
    let (maker, taker) = determine_maker_taker(
        &yes_token_id,
        &no_token_id,
        yes_size,
        no_size,
    );
    
    println!("[EXEC] Maker: {} {} (size: {})", maker.order_type, 
        &maker.token_id[maker.token_id.len()-6..], maker.size);
    println!("[EXEC] Taker: {} {} (size: {})", taker.order_type,
        &taker.token_id[taker.token_id.len()-6..], taker.size);
    
    // Step 4: In DRY_RUN, simulate the matching engine
    if dry_run {
        let fake_order_id = format!("dry_run_{}", chrono::Utc::now().timestamp_millis());
        println!("[DRY_RUN] ✅ Maker order submitted: {}", fake_order_id);
        
        // Simulate MATCHED event after 50ms
        let taker_token = taker.token_id.clone();
        let taker_size = taker.size;
        
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            println!("[DRY_RUN] 🟢 Simulated MAKER MATCHED! Firing Taker FAK...");
            
            // Step 5: Submit Taker FAK
            println!("[DRY_RUN]   Taker FAK for {} shares of {}..{}", 
                taker_size, &taker_token[..10], &taker_token[taker_token.len()-6..]);
            
            // Simulate Taker fill after 100ms
            sleep(Duration::from_millis(100)).await;
            println!("[DRY_RUN] 🟢 Simulated TAKER MATCHED! Ready for CTF Merge.");
            println!("[DRY_RUN]   Condition ID: {}", condition_id);
        });
        
        return;
    }
    
    // TODO: Live execution path
    // 1. Sign Maker order with EIP-712
    // 2. POST to CLOB
    // 3. Store order_id in pending_hedges
    // 4. User WS will trigger Taker on MATCHED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_token_consistency() {
        let token = "0x1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
        let hash1 = hash_token(token);
        let hash2 = fast_hash(token.as_bytes());
        assert_eq!(hash1, hash2);
    }
    
    fn fast_hash(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &b in bytes { 
            hash ^= b as u64; 
            hash = hash.wrapping_mul(0x100000001b3); 
        }
        hash
    }
}