//! WebSocket Integration for HFT Hot Path
//! Uses sync tungstenite for sub-microsecond latency (no tokio)

use std::io::Read;
use tungstenite::{Message, client_tls};
use std::net::TcpStream;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;

/// Connect to Polymarket WebSocket and return a stream for the hot path
pub fn connect_to_polymarket(tokens: Vec<String>) -> WebSocketReader {
    let url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    
    println!("Connecting to Polymarket WebSocket...");
    
    // OPTION C: Set TCP timeout BEFORE TLS handshake (NotebookLM recommendation)
    // 1. Manually connect TCP socket to port 443
    let tcp_stream = TcpStream::connect("ws-subscriptions-clob.polymarket.com:443")
        .expect("Failed to connect TCP stream");
    
    // 2. Set 30-second read timeout BEFORE TLS handshake
    tcp_stream.set_read_timeout(Some(Duration::from_secs(30)))
        .expect("Failed to set read timeout");
    
    println!("TCP connected with 30s timeout");
    
    // 3. Perform TLS/WS handshake with pre-configured stream
    let (mut socket, _response) = client_tls(url, tcp_stream)
        .expect("Failed TLS handshake");
    
    println!("WebSocket connected");
    
    // Subscribe to all tokens
    let subscribe_msg = serde_json::json!({
        "type": "market",
        "operation": "subscribe",
        "markets": [],
        "assets_ids": tokens,
        "initial_dump": true
    });
    
    let msg = Message::Text(subscribe_msg.to_string());
    socket.send(msg).expect("Failed to subscribe");
    
    println!("Subscribed to {} tokens", tokens.len());
    
    WebSocketReader { socket, buffer: vec![] }
}

/// Wrapper to implement Read for WebSocket
pub struct WebSocketReader {
    pub socket: tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl Read for WebSocketReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.socket.read() {
            Ok(Message::Text(text)) => {
                let bytes = text.as_bytes();
                let len = std::cmp::min(buf.len(), bytes.len());
                buf[..len].copy_from_slice(&bytes[..len]);
                Ok(len)
            }
            Ok(Message::Binary(data)) => {
                let len = std::cmp::min(buf.len(), data.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            Ok(_) => Ok(0),
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        }
    }
}

impl WebSocketReader {
    pub fn send(&mut self, msg: Message) -> Result<(), tungstenite::Error> {
        self.socket.send(msg)
    }
}
