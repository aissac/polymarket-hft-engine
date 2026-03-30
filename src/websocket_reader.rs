//! WebSocket Integration for HFT Hot Path
//! Uses sync tungstenite for sub-microsecond latency (no tokio)

use std::io::Read;
use tungstenite::{connect, Message};
use std::net::TcpStream;
use tungstenite::stream::MaybeTlsStream;

/// Connect to Polymarket WebSocket and return a stream for the hot path
pub fn connect_to_polymarket(tokens: Vec<String>) -> WebSocketReader {
    let url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    
    println!("📡 Connecting to Polymarket WebSocket...");
    
    let (mut socket, _response) = connect(url)
        .expect("Failed to connect to WebSocket");
    
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
    
    println!("📡 Subscribed to {} tokens", tokens.len());
    
    WebSocketReader { socket, buffer: vec![] }
}

/// Wrapper to implement Read for WebSocket
pub struct WebSocketReader {
    socket: tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    buffer: Vec<u8>,
}

impl Read for WebSocketReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // If we have leftover data from previous message, return it
        if !self.buffer.is_empty() {
            let len = std::cmp::min(buf.len(), self.buffer.len());
            buf[..len].copy_from_slice(&self.buffer[..len]);
            self.buffer.drain(..len);
            return Ok(len);
        }
        
        // Read next WebSocket message
        loop {
            match self.socket.read() {
                Ok(Message::Text(text)) => {
                    let bytes = text.as_bytes();
                    let len = std::cmp::min(buf.len(), bytes.len());
                    buf[..len].copy_from_slice(&bytes[..len]);
                    
                    // Store remainder for next read
                    if bytes.len() > len {
                        self.buffer = bytes[len..].to_vec();
                    }
                    
                    return Ok(len);
                }
                Ok(Message::Ping(data)) => {
                    // Respond to ping automatically
                    let _ = self.socket.send(Message::Pong(data));
                    continue;
                }
                Ok(Message::Pong(_)) => {
                    // Ignore pong
                    continue;
                }
                Ok(Message::Close(_)) => {
                    return Ok(0); // EOF
                }
                Err(e) => {
                    return Err(std::io::Error::new(std::io::ErrorKind::Other, e));
                }
                _ => continue,
            }
        }
    }
}
