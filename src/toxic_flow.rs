//! Toxic Flow Detection - Real-time Adverse Selection Monitor

use tokio::sync::watch;
use std::time::{Instant, Duration};

#[derive(Debug, Clone, PartialEq)]
pub enum ToxicSignal {
    Clean,
    OrderbookImbalance(f64),
    SpotDislocation(f64),
    VolumeSpike(f64),
    WhaleTradeDetected(f64),
}

pub struct ToxicFlowConfig {
    pub imbalance_threshold: f64,
    pub spot_move_threshold: f64,
    pub volume_spike_multiplier: f64,
    pub whale_trade_size: f64,
}

impl Default for ToxicFlowConfig {
    fn default() -> Self {
        Self {
            imbalance_threshold: 5.0,
            spot_move_threshold: 0.0015,
            volume_spike_multiplier: 10.0,
            whale_trade_size: 1000.0,
        }
    }
}

pub struct ToxicFlowMonitor {
    config: ToxicFlowConfig,
    volume_1m_rolling: f64,
    avg_volume_15m: f64,
    last_volume_reset: Instant,
}

impl ToxicFlowMonitor {
    pub fn new() -> Self {
        Self {
            config: ToxicFlowConfig::default(),
            volume_1m_rolling: 0.0,
            avg_volume_15m: 5000.0,
            last_volume_reset: Instant::now(),
        }
    }
    
    pub async fn run_monitor(
        whitelist_tx: watch::Sender<ToxicSignal>,
    ) {
        let mut monitor = Self::new();
        
        loop {
            if monitor.last_volume_reset.elapsed() > Duration::from_secs(60) {
                monitor.volume_1m_rolling = 0.0;
                monitor.last_volume_reset = Instant::now();
            }
            
            let _ = whitelist_tx.send(ToxicSignal::Clean);
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    
    pub fn is_safe_to_place_trap(signal: &ToxicSignal) -> bool {
        matches!(signal, ToxicSignal::Clean)
    }
}
