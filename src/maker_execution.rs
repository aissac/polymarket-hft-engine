// Maker Execution - Post-Only Order Submission

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize)]
pub struct ClobOrder {
    pub orderID: String,
    pub signature: String,
    pub sender: String,
    pub price: String,
    pub size: String,
    pub side: String,
    pub token_id: String,
    pub expiration: String,
    pub post_only: bool,
    pub reduce_only: bool,
    pub order_type: String,
}

#[derive(Deserialize, Debug)]
pub struct OrderResponse {
    pub order_id: Option<String>,
    pub status: Option<String>,
    pub error: Option<String>,
}

pub async fn submit_maker_order(
    client: &Client,
    token_id: &str,
    price: f64,
    size: f64,
    side: &str,
    signer: &str,
    salt: &str,
) -> Result<OrderResponse, Box<dyn std::error::Error>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let expiration = now + 3600;
    
    let order = ClobOrder {
        orderID: format!("{}-{}", signer, salt),
        signature: "0x0".to_string(),
        sender: signer.to_string(),
        price: format!("{:.2}", price),
        size: format!("{:.0}", size),
        side: side.to_string(),
        token_id: token_id.to_string(),
        expiration: expiration.to_string(),
        post_only: true,
        reduce_only: false,
        order_type: "LIMIT".to_string(),
    };
    
    let response = client
        .post("https://clob.polymarket.com/order")
        .json(&order)
        .send()
        .await?;
    
    let result: OrderResponse = response.json().await?;
    Ok(result)
}

pub async fn check_order_status(
    client: &Client,
    order_id: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = client
        .get(format!("https://clob.polymarket.com/order/{}", order_id))
        .send()
        .await?;
    
    let result: Value = response.json().await?;
    Ok(result)
}

pub async fn merge_ctf(
    client: &Client,
    yes_token: &str,
    no_token: &str,
    amount: f64,
    signer: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let payload = serde_json::json!({
        "yes_token": yes_token,
        "no_token": no_token,
        "amount": amount.to_string(),
        "sender": signer,
    });
    
    let response = client
        .post("https://clob.polymarket.com/ctf/merge")
        .json(&payload)
        .send()
        .await?;
    
    let result: Value = response.json().await?;
    Ok(result)
}
