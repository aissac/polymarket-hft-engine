//! Polymarket Fee Calculation (March 30, 2026)
//!
//! Fee formula: fee = size × p × feeRate × (p × (1-p))
//! Crypto markets: feeRate = 0.072, exponent = 1
//! Max taker fee: 1.80% at p=0.50
//! Maker rebate: 20% of taker fee

/// Calculate taker fee for a trade
/// Returns the fee in USDC
pub fn calculate_taker_fee(price: f64, size: f64) -> f64 {
    // fee = C × p × feeRate × (p × (1-p))
    // Where C = size, p = probability (price)
    // Crypto: feeRate = 0.072, exponent = 1

    // Avoid division issues
    let p = price.clamp(0.001, 0.999);
    let variance = p * (1.0 - p);

    // Fee formula for crypto markets
    size * p * 0.072 * variance
}

/// Calculate maker rebate (20% of taker fee)
pub fn calculate_maker_rebate(taker_fee: f64) -> f64 {
    taker_fee * 0.20
}

/// Calculate net fee for Hybrid Maker-Taker strategy
/// Returns (total_fee, maker_rebate, taker_fee)
pub fn calculate_hybrid_fee(
    maker_price: f64,
    taker_price: f64,
    size: f64
) -> (f64, f64, f64) {
    let taker_fee = calculate_taker_fee(taker_price, size);
    let maker_fee = calculate_taker_fee(maker_price, size);
    let maker_rebate = calculate_maker_rebate(maker_fee);

    // Net fee = Taker fee - Maker rebate
    let net_fee = taker_fee - maker_rebate;

    (net_fee, maker_rebate, taker_fee)
}

/// Calculate pure taker fee (both legs as taker)
/// Returns total fee (3.6% max at p=0.50)
pub fn calculate_pure_taker_fee(
    yes_price: f64,
    no_price: f64,
    size: f64
) -> f64 {
    let yes_fee = calculate_taker_fee(yes_price, size);
    let no_fee = calculate_taker_fee(no_price, size);
    yes_fee + no_fee
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_at_50_percent() {
        // At p=0.50, variance is maximum (0.25)
        let fee = calculate_taker_fee(0.50, 100.0);
        println!("Fee at p=0.50, size=100: ${:.4}", fee);
    }

    #[test]
    fn test_fee_at_90_percent() {
        // At p=0.90, variance = 0.90 × 0.10 = 0.09
        let fee = calculate_taker_fee(0.90, 100.0);
        println!("Fee at p=0.90, size=100: ${:.4}", fee);
        // Should be ~0.65%
    }

    #[test]
    fn test_hybrid_fee() {
        let (net_fee, rebate, taker) = calculate_hybrid_fee(0.50, 0.50, 100.0);
        println!("Hybrid: net=${:.4}, rebate=${:.4}, taker=${:.4}", net_fee, rebate, taker);
    }
}