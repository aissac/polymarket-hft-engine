//! CLOB Order Execution for Polymarket
//! 
//! Handles order submission, L2 authentication, and 429 backoff.

use reqwest::{Client, StatusCode, header::{HeaderMap, HeaderValue, CONTENT_TYPE}};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::signing::{sign_polymarket_order};

use alloy_signer_local::PrivateKeySigner;
use alloy_primitives::Address;

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

/// Submit order with exponential backoff for 429 errors
pub async fn submit_order_with_backoff(
    client: &Client,
    payload: Value,
    headers: HeaderMap,
) -> Result<Value, String> {
    let mut retries = 0;
    let max_retries = 3;
    let url = format!("{}/order", CLOB_BASE);

    loop {
        let resp = client
            .post(&url)
            .headers(headers.clone())
            .json(&payload)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        match resp.status() {
            StatusCode::OK => {
                let body = resp.text().await.unwrap_or_default();
                if body.contains("error") || body.contains("success\":false") {
                    return Err(format!("CLOB Error: {}", body));
                }
                return serde_json::from_str(&body).map_err(|e| e.to_string());
            }
            StatusCode::TOO_MANY_REQUESTS => {
                if retries >= max_retries {
                    return Err("Max retries exceeded on 429".to_string());
                }
                let backoff_ms = 2u64.pow(retries) * 100; // 100ms, 200ms, 400ms
                println!("⚠️ 429 Rate Limit Hit. Backing off for {}ms", backoff_ms);
                sleep(Duration::from_millis(backoff_ms)).await;
                retries += 1;
            }
            other => return Err(format!("API HTTP Error: {}", other)),
        }
    }
}

/// Fetch dynamic fee rate for a token
pub async fn fetch_fee_rate(client: &Client, token_id: &str) -> Result<u64, String> {
    let url = format!("{}/fee-rate?token_id={}", CLOB_BASE, token_id);
    
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("Failed to fetch fee rate: {}", resp.status()));
    }

    let body = resp.text().await.map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    
    // Fee rate is returned in basis points
    json.get("feeRateBps")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "No feeRateBps in response".to_string())
}

/// Create and sign a Polymarket order
pub async fn create_signed_order(
    signer: &PrivateKeySigner,
    maker: Address,
    token_id: &str,
    maker_amount: u64,
    taker_amount: u64,
    fee_rate_bps: u64,
    side: u8, // 0 = BUY, 1 = SELL
) -> Result<Value, String> {
    // Create order struct
    let order = create_order(
        maker,
        signer.address(),
        Address::ZERO, // taker = 0 for resting orders
        token_id,
        maker_amount,
        taker_amount,
        fee_rate_bps,
        side,
    );

    // Sign the order
    let signature = sign_polymarket_order(&order, signer)?;

    // Convert to JSON payload
    Ok(json!({
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
        "side": order.side,
        "signatureType": 2,
        "signature": signature
    }))
}

/// Execute Maker order (resting limit order)
pub async fn execute_maker_order(
    client: &Client,
    signer: &PrivateKeySigner,
    maker: Address,
    token_id: &str,
    price: f64,      // In USDC (e.g., 0.40)
    size: u64,        // In micro-USDC (e.g., 5000000 = $5)
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
) -> Result<Value, String> {
    // Fetch dynamic fee rate
    let fee_rate = fetch_fee_rate(client, token_id).await?;
    
    // Convert price to maker amount (USDC) and taker amount (shares)
    let maker_amount = (price * 1_000_000.0) as u64 * size / 1_000_000;
    let taker_amount = size;

    // Create signed order
    let payload = create_signed_order(
        signer,
        maker,
        token_id,
        maker_amount,
        taker_amount,
        fee_rate,
        0, // BUY
    ).await?;

    // Build headers
    let body_str = serde_json::to_string(&payload).unwrap();
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

/// Execute Taker order (immediate fill - FAK)
pub async fn execute_taker_order(
    client: &Client,
    signer: &PrivateKeySigner,
    maker: Address,
    token_id: &str,
    price: f64,
    size: u64,
    api_key: &str,
    api_secret: &str,
    api_passphrase: &str,
) -> Result<Value, String> {
    // Fetch dynamic fee rate
    let fee_rate = fetch_fee_rate(client, token_id).await?;
    
    // For taker, we cross the spread at aggressive price
    let maker_amount = (price * 1_000_000.0) as u64 * size / 1_000_000;
    let taker_amount = size;

    let payload = create_signed_order(
        signer,
        maker,
        token_id,
        maker_amount,
        taker_amount,
        fee_rate,
        0, // BUY
    ).await?;

    let body_str = serde_json::to_string(&payload).unwrap();
    let headers = build_l2_headers(
        api_key,
        api_secret,
        api_passphrase,
        &format!("{:?}", signer.address()),
        "POST",
        "/order",
        &body_str,
    );

    submit_order_with_backoff(client, payload, headers).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_construction() {
        let headers = build_l2_headers(
            "test_key",
            "dGVzdF9zZWNyZXQ=", // base64 "test_secret"
            "test_pass",
            "0x1234",
            "POST",
            "/order",
            "{}",
        );
        assert!(headers.contains_key("POLY_ADDRESS"));
        assert!(headers.contains_key("POLY_API_KEY"));
    }
}