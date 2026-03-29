//! CLOB Order Execution for Polymarket
//! 
//! Handles order submission, L2 authentication, and dynamic fee rate fetching.
//! Uses direct HTTP POST for order submission (DRY RUN mode ready).

use reqwest::{Client, StatusCode, header::{HeaderMap, HeaderValue, CONTENT_TYPE}};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

// Re-export from signing
pub use crate::signing::{Order, Side, OrderType, SignatureType, PrivateKeySigner};

use alloy_primitives::{Address, U256};
use std::str::FromStr;

const CLOB_BASE: &str = "https://clob.polymarket.com";

/// Build L2 authentication headers for Polymarket API
pub fn build_l2_headers(
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
    signer_address: &str,
    method: &str,
    request_path: &str,
    body_str: &str,
) -> HeaderMap {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    // Message: timestamp + method + request_path + body
    let msg = format!("{}{}{}{}", timestamp, method, request_path, body_str);

    // Decode base64 API secret
    let decoded_secret = BASE64.decode(api_secret).expect("Invalid base64 secret");

    // HMAC-SHA256 signature
    let mut mac = Hmac::<Sha256>::new_from_slice(&decoded_secret).expect("HMAC key error");
    mac.update(msg.as_bytes());
    let signature_bytes = mac.finalize().into_bytes();
    let signature_b64 = BASE64.encode(signature_bytes);

    // Build headers
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("POLY_ADDRESS", HeaderValue::from_str(signer_address).unwrap());
    headers.insert("POLY_API_KEY", HeaderValue::from_str(api_key).unwrap());
    headers.insert("POLY_PASSPHRASE", HeaderValue::from_str(api_passphrase).unwrap());
    headers.insert("POLY_TIMESTAMP", HeaderValue::from_str(&timestamp).unwrap());
    headers.insert("POLY_SIGNATURE", HeaderValue::from_str(&signature_b64).unwrap());

    headers
}

/// Fetch dynamic fee rate for a token
pub async fn fetch_fee_rate(client: &Client, token_id: &str) -> Result<u64, String> {
    let url = format!("{}/fee-rate?token_id={}", CLOB_BASE, token_id);
    
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Fee rate fetch error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("Failed to fetch fee rate: {}", resp.status()));
    }

    let body = resp.text().await.map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;

    // feeRateBps is the fee in basis points
    json.get("feeRateBps")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "No feeRateBps in response".to_string())
}

/// Create order payload for Polymarket CLOB
/// This creates the JSON payload directly (no SDK dependency)
pub fn create_order_payload(
    maker: Address,
    signer: Address,
    taker: Address,
    token_id: &str,
    maker_amount: u64,
    taker_amount: u64,
    fee_rate_bps: u64,
    side: u8,
    salt: u64,
    expiration: u64,
    nonce: u64,
) -> Value {
    json!({
        "salt": salt,
        "maker": format!("{:?}", maker),
        "signer": format!("{:?}", signer),
        "taker": format!("{:?}", taker),
        "tokenId": token_id,
        "makerAmount": maker_amount.to_string(),
        "takerAmount": taker_amount.to_string(),
        "expiration": expiration.to_string(),
        "nonce": nonce.to_string(),
        "feeRateBps": fee_rate_bps.to_string(),
        "side": side,
        "signatureType": 2,  // GNOSIS_SAFE for gasless
    })
}

/// Submit order with exponential backoff for 429 errors
/// In DRY_RUN mode, returns Ok(()) without actually sending
pub async fn submit_order(
    client: &Client,
    order_payload: &Value,
    signature: &str,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
    signer_address: &str,
    dry_run: bool,
    max_retries: u32,
) -> Result<Value, String> {
    // DRY RUN mode: validate payload but don't submit
    if dry_run {
        println!("[DRY_RUN] Would submit order:");
        println!("  Payload: {}", serde_json::to_string_pretty(order_payload).unwrap());
        println!("  Signature: {}", signature);
        return Ok(json!({"order_id": "dry_run_mock", "status": "dry_run"}));
    }
    
    let mut retry_delay_ms = 100;
    
    for attempt in 0..=max_retries {
        let body_str = serde_json::to_string(order_payload).unwrap();
        let headers = build_l2_headers(
            api_key,
            api_secret,
            api_passphrase,
            signer_address,
            "POST",
            "/order",
            &body_str,
        );

        // Add signature to payload
        let mut full_payload = order_payload.clone();
        full_payload["signature"] = Value::String(signature.to_string());

        let resp = client
            .post(&format!("{}/order", CLOB_BASE))
            .headers(headers)
            .json(&full_payload)
            .send()
            .await
            .map_err(|e| format!("Request error: {}", e))?;

        match resp.status() {
            StatusCode::OK | StatusCode::CREATED => {
                let body = resp.text().await.map_err(|e| e.to_string())?;
                let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
                return Ok(json);
            }
            StatusCode::TOO_MANY_REQUESTS => {
                if attempt == max_retries {
                    return Err("Max retries exceeded for rate limit".to_string());
                }
                sleep(Duration::from_millis(retry_delay_ms)).await;
                retry_delay_ms *= 2;
            }
            status => {
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Order rejected: {} - {}", status, body));
            }
        }
    }
    
    Err("Unexpected error".to_string())
}

/// Generate a random salt for orders
pub fn generate_salt() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_l2_headers() {
        let headers = build_l2_headers(
            "test_key",
            "dGVzdF9zZWNyZXQ=", // base64 of "test_secret"
            "test_pass",
            "0x1234",
            "POST",
            "/order",
            "{}",
        );
        
        assert!(headers.contains_key("POLY_ADDRESS"));
        assert!(headers.contains_key("POLY_API_KEY"));
        assert!(headers.contains_key("POLY_TIMESTAMP"));
        assert!(headers.contains_key("POLY_SIGNATURE"));
    }

    #[test]
    fn test_create_order_payload() {
        let maker = Address::from_str("0x1234567890123456789012345678901234567890").unwrap();
        let signer = maker;
        let taker = Address::ZERO;
        
        let payload = create_order_payload(
            maker, signer, taker,
            "1234567890",
            1000000, // $1.00 in USDC (6 decimals)
            1000000,
            180, // 1.80% fee
            0,   // BUY
            12345,
            0,   // never expires
            0,
        );
        
        assert_eq!(payload["side"], 0);
        assert_eq!(payload["signatureType"], 2);
        assert_eq!(payload["feeRateBps"], "180");
    }
}