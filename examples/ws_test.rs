// Test WebSocket using SDK's client directly
use futures::StreamExt;
use polymarket_client_sdk::clob::ws::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = Client::default();
    
    let asset_ids = vec![
        "92703761682322480664976766247614127878023988651992837287050266308961660624165".to_owned(),
    ];
    
    println!("Subscribing to asset_id: {}", &asset_ids[0][..20]);
    
    let stream = client.subscribe_orderbook(asset_ids)?;
    let mut stream = Box::pin(stream);
    
    println!("Waiting for messages...");
    
    while let Some(msg_result) = stream.next().await {
        match msg_result {
            Ok(msg) => {
                println!("Got orderbook: {:?}", msg);
            }
            Err(e) => {
                eprintln!("Error: {:?}", e);
            }
        }
    }
    
    Ok(())
}
