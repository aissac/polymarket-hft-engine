//! CLOB Order Executor - Phase 1: Pre-warmed HTTP/2 + Fire-and-Forget
//! 
//! Optimizations:
//! 1. Pre-warmed reqwest HTTP/2 client (no TLS handshake per order)
//! 2. EIP-712 pre-computation cache
//! 3. Fire-and-forget submission (background channel, don't await in hot path)
//! 4. Maker-order support (post inside spread, avoid 500ms taker delay)

use reqwest::Client;
use serde::{Serialize, Deserialize};
use tokio::sync::mpsc;
use tracing::{info, error, debug};

/// Pre-warmed CLOB client for sub-100ms order submission
pub struct ClobExecutor {
    /// Pre-warmed HTTP/2 client (connection pooling enabled)
    client: Client,
    /// CLOB API base URL
    base_url: String,
    /// API key for authentication (Builder Program)
    api_key: Option<String>,
    /// Order submission channel (fire-and-forget)
    order_tx: mpsc::UnboundedSender<OrderSubmission>,
}

/// Order submission for background processing
#[derive(Debug, Clone)]
pub struct OrderSubmission {
    pub api_key: Option<String>,
    pub condition_id: String,
    pub side: String, // "BUY" or "SELL"
    pub price: f64,
    pub size: f64,
    pub is_maker: bool, // true = post inside spread, false = cross spread
    pub signature: String, // EIP-712 signature
}

/// CLOB order payload (EIP-712 signed)
#[derive(Debug, Serialize, Deserialize)]
pub struct ClobOrder {
    pub order_id: String,
    pub ticker: String,
    pub asset_id: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String, // "GTC" or "IOC"
    pub price: String,
    pub size: String,
    pub expiration: String,
    pub signature: String,
    #[serde(rename = "signatureType")]
    pub signature_type: i32, // 0 = EOA, 2 = Gnosis Safe Proxy (gasless)
}

impl ClobExecutor {
    /// Create pre-warmed executor with HTTP/2 connection pooling
    pub fn new(base_url: &str, api_key: Option<String>) -> Self {
        // Pre-warm HTTP/2 client with connection pooling
        let client = Client::builder()
            .http2_prior_knowledge()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(std::time::Duration::from_secs(60))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to create pre-warmed HTTP/2 client");
        
        let (order_tx, mut order_rx) = mpsc::unbounded_channel::<OrderSubmission>();
        
        // Spawn background order processor (fire-and-forget)
        let client_clone = client.clone();
        let base_url_clone = base_url.to_string();
        tokio::spawn(async move {
            while let Some(submission) = order_rx.recv().await {
                Self::process_order_background(&client_clone, &base_url_clone, submission).await;
            }
        });
        
        Self {
            client,
            base_url: base_url.to_string(),
            api_key,
            order_tx,
        }
    }

    /// Submit order asynchronously (fire-and-forget)
    /// Returns immediately, order processed in background
    pub fn submit_order(&self, submission: OrderSubmission) {
        debug!(
            "📤 Submitting {} order: {} @ ${:.4} x {:.0} (maker: {})",
            submission.side, submission.condition_id, submission.price, submission.size, submission.is_maker
        );
        
        // Fire-and-forget: send to background channel, don't await
        if let Err(e) = self.order_tx.send(submission) {
            error!("❌ Failed to queue order for background processing: {}", e);
        }
    }

    /// Background order processor (runs in spawned task)
    async fn process_order_background(client: &Client, base_url: &str, submission: OrderSubmission) {
        let order_payload = ClobOrder {
            order_id: format!("{}-{}-{}", submission.condition_id, submission.side, chrono::Utc::now().timestamp()),
            ticker: submission.condition_id.clone(),
            asset_id: submission.condition_id.clone(),
            side: submission.side,
            order_type: if submission.is_maker { "GTC".to_string() } else { "IOC".to_string() },
            price: format!("{:.4}", submission.price),
            size: format!("{:.0}", submission.size),
            expiration: "0".to_string(),
            signature: submission.signature,
            signature_type: 2, // Gnosis Safe Proxy (gasless)
        };

        let url = format!("{}/order", base_url);
        
        let start = std::time::Instant::now();
        
        let mut request = client.post(&url)
            .json(&order_payload);
        
        if let Some(api_key) = &submission.api_key {
            request = request.header("POLY_BUILDER_API_KEY", api_key);
        }
        
        match request.send().await {
            Ok(response) => {
                let latency = start.elapsed();
                match response.text().await {
                    Ok(body) => {
                        if body.contains("success") || body.contains("order_id") {
                            info!("✅ Order submitted in {:?}: {}", latency, body);
                        } else {
                            error!("❌ Order rejected in {:?}: {}", latency, body);
                        }
                    }
                    Err(e) => error!("❌ Failed to read response in {:?}: {}", latency, e),
                }
            }
            Err(e) => {
                let latency = start.elapsed();
                error!("❌ Order submission failed in {:?}: {}", latency, e);
            }
        }
    }

    /// Pre-compute EIP-712 hash static parts (optimization)
    pub fn precompute_eip712_domain(condition_id: &str) -> Eip712Domain {
        Eip712Domain {
            name: "Clob".to_string(),
            version: "1".to_string(),
            chain_id: 137, // Polygon
            verifying_contract: "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E".to_string(),
            condition_id: condition_id.to_string(),
        }
    }
}

/// Pre-computed EIP-712 domain (cache static parts)
#[derive(Debug, Clone)]
pub struct Eip712Domain {
    pub name: String,
    pub version: String,
    pub chain_id: i32,
    pub verifying_contract: String,
    pub condition_id: String,
}
