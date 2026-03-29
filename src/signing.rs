//! Signing helpers - re-exports from polymarket-client-sdk

pub use polymarket_client_sdk::clob::types::{Order, Side, OrderType, SignatureType, SignableOrder};
pub use polymarket_client_sdk::clob::order_builder::OrderBuilder;

// Use alloy crate's PrivateKeySigner (same as trading.rs)
pub use alloy::signers::local::PrivateKeySigner;

use std::str::FromStr;

/// Initialize signer from private key hex
pub fn init_signer(private_key_hex: &str) -> Result<PrivateKeySigner, String> {
    let pk = private_key_hex.trim_start_matches("0x");
    PrivateKeySigner::from_str(pk).map_err(|e| e.to_string())
}

/// Generate random salt
pub fn generate_salt() -> alloy_primitives::U256 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    alloy_primitives::U256::from(ts)
}
