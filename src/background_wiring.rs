//! Background Thread Wiring (NotebookLM Integration)
//!
//! Bridges hot path (crossbeam) → execution (tokio)
//! - Maps token_hash → token_id → condition_id
//! - Determines Maker (GTC Post-Only) vs Taker (FAK)
//! - Calculates fees using March 30 formula
//! - Simulates MATCHED in DRY_RUN

use std::sync::Arc;
use tokio::time::{sleep, Duration};
use reqwest::Client;

use crate::token_map::hash_token;
use crate::fees::calculate_hybrid_fee;
use crate::maker_taker_routing::{determine_maker_taker, execute_dry_run_sequence, MakerLeg, TakerLeg};

/// Process an edge detection from hot path
pub async fn process_edge(
    token_hash: u64,
    complement_hash: u64,
    yes_size: u64,
    no_size: u64,
    combined_price: u64,
    hash_to_id: &std::collections::HashMap<u64, String>,
    id_to_condition: &std::collections::HashMap<String, String>,
    _client: &Client,
    _signer: &Arc<alloy_signer_local::PrivateKeySigner>,
    _api_key: &str,
    _api_secret: &str,
    _api_passphrase: &str,
    _signer_address: &str,
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

    // Convert sizes to f64 (in shares)
    let yes_size_f = yes_size as f64 / 1_000_000.0; // Assuming u64 is in micro-shares
    let no_size_f = no_size as f64 / 1_000_000.0;

    // Combined price as f64
    let combined_f = combined_price as f64 / 1_000_000.0;

    // For now, estimate prices from combined (will need actual prices from orderbook)
    // YES + NO = combined, assume roughly equal
    let yes_ask_price = combined_f / 2.0;
    let no_ask_price = combined_f / 2.0;

    // Step 3: Determine Maker/Taker based on depth
    let (maker, taker) = determine_maker_taker(
        &yes_token_id,
        &no_token_id,
        yes_size_f,
        no_size_f,
        yes_ask_price,
        no_ask_price,
        5.0, // $5 max position
    );

    // Step 4: Calculate fees using March 30 formula
    let (net_fee, maker_rebate, taker_fee) = calculate_hybrid_fee(
        maker.price,
        taker.price,
        maker.size,
    );

    println!();
    println!("[EXEC] 🎯 Edge detected: combined=${:.4}", combined_f);
    println!("[EXEC]   YES token: {}..{}", &yes_token_id[..10], &yes_token_id[yes_token_id.len()-6..]);
    println!("[EXEC]   NO token:  {}..{}", &no_token_id[..10], &no_token_id[no_token_id.len()-6..]);
    println!("[EXEC]   Condition: {}..{}", &condition_id[..10], &condition_id[condition_id.len()-6..]);
    println!("[EXEC]   YES depth: {:.0}, NO depth: {:.0}", yes_size_f, no_size_f);
    println!("[EXEC]   Thick side: {} (Maker)", maker.side);
    println!("[EXEC]   Thin side: {} (Taker)", taker.side);
    println!("[EXEC]   Net fee: ${:.4} (rebate ${:.4}, taker ${:.4})", net_fee, maker_rebate, taker_fee);

    // Step 5: In DRY_RUN, simulate the matching engine
    if dry_run {
        execute_dry_run_sequence(&maker, &taker, net_fee, combined_f).await;
    } else {
        // TODO: Live execution
        // 1. Sign and submit Maker order (GTC Post-Only)
        // 2. Wait for MATCHED on User WebSocket
        // 3. Sign and submit Taker order (FAK)
        // 4. Start stop-loss timer
        eprintln!("⚠️ Live execution not implemented yet");
    }
}

/// Wire the background thread to execute arbitrage pairs
pub async fn wire_background_execution(
    _client: Arc<Client>,
    _hash_to_id: Arc<std::collections::HashMap<u64, String>>,
    _id_to_condition: Arc<std::collections::HashMap<String, String>>,
    _signer: Arc<alloy_signer_local::PrivateKeySigner>,
    _api_key: String,
    _api_secret: String,
    _api_passphrase: String,
    _signer_address: String,
    dry_run: bool,
) {
    println!("[BG] 🔧 Background execution thread started (DRY_RUN={})", dry_run);
}