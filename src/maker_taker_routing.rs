//! Maker/Taker Routing Logic
//!
//! Identifies which leg is THICK (stable) for Maker order
//! and which is THIN (volatile) for Taker order.
//!
//! Strategy: Post Maker on thick side (GTC Post-Only)
//!           Take Taker on thin side (FAK)

use serde::{Deserialize, Serialize};

/// Maker leg (thick side)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MakerLeg {
    pub token: String,
    pub price: f64,
    pub size: f64,
    pub side: String, // "YES" or "NO"
}

/// Taker leg (thin side)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TakerLeg {
    pub token: String,
    pub price: f64,
    pub size: f64,
    pub side: String, // "YES" or "NO"
}

/// Determine which leg is thick (Maker) and which is thin (Taker)
///
/// The side with MORE resting ask liquidity is THICK (stable).
/// The side with LESS ask liquidity is THIN (volatile).
pub fn determine_maker_taker(
    yes_token: &str,
    no_token: &str,
    yes_ask_size: f64,
    no_ask_size: f64,
    yes_ask_price: f64,
    no_ask_price: f64,
    size: f64, // Position size
) -> (MakerLeg, TakerLeg) {
    // The side with MORE resting ask liquidity is THICK (stable)
    if yes_ask_size > no_ask_size {
        // YES is thick -> Rest Maker order on YES
        let maker = MakerLeg {
            token: yes_token.to_string(),
            price: yes_ask_price,
            size,
            side: "YES".to_string(),
        };
        let taker = TakerLeg {
            token: no_token.to_string(),
            price: no_ask_price,
            size,
            side: "NO".to_string(),
        };
        (maker, taker)
    } else {
        // NO is thick -> Rest Maker order on NO
        let maker = MakerLeg {
            token: no_token.to_string(),
            price: no_ask_price,
            size,
            side: "NO".to_string(),
        };
        let taker = TakerLeg {
            token: yes_token.to_string(),
            price: yes_ask_price,
            size,
            side: "YES".to_string(),
        };
        (maker, taker)
    }
}

/// DRY_RUN simulation sequence
///
/// Simulates the Maker-Taker execution sequence:
/// 1. Post Maker order (GTC Post-Only)
/// 2. Wait for Maker MATCHED
/// 3. Fire Taker order (FAK)
/// 4. Wait for Taker MATCHED
pub async fn execute_dry_run_sequence(
    maker: &MakerLeg,
    taker: &TakerLeg,
    net_fee: f64,
    combined_ask: f64,
) {
    println!();
    println!("══════════════════════════════════════════════════════════════");
    println!("🎯 EDGE DETECTED: Combined Ask = ${:.4}", combined_ask);
    println!("══════════════════════════════════════════════════════════════");
    println!();
    println!("📊 Thick/Thin Analysis:");
    println!("  Thick side: {} (depth: {:.0})", maker.side, maker.size);
    println!("  Thin side:   {} (depth: {:.0})", taker.side, taker.size);
    println!();
    println!("💰 Fee Analysis (March 30 formula):");
    println!("  Maker fee:   $0.00 (0% + rebate)");
    println!("  Taker fee:   ${:.4} (~1.78%)", net_fee);
    println!("  Net cost:    ${:.4}", combined_ask + net_fee / maker.size);
    println!();
    println!("🔄 Execution Sequence:");
    println!("  [DRY_RUN] Would POST MAKER: BUY {} @ ${:.4} (GTC Post-Only)", maker.side, maker.price);

    // Simulate wait for Maker to be hit
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    println!("  ✅ [DRY_RUN] 🟢 Simulated MAKER MATCHED!");

    // Fire Taker instantly after Maker fills
    println!("  [DRY_RUN] Would TAKE: BUY {} @ ${:.4} (FAK)", taker.side, taker.price);

    // Simulate matching engine for Taker
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    println!("  ✅ [DRY_RUN] 🟢 Simulated TAKER MATCHED!");
    println!();
    println!("══════════════════════════════════════════════════════════════");
    println!("✅ ARBITRAGE COMPLETE (DRY_RUN)");
    println!("══════════════════════════════════════════════════════════════");
    println!();
}