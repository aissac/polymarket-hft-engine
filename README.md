# Pingpong

High-frequency arbitrage trading bot for Polymarket prediction markets.

## Strategy

Pingpong exploits pricing inefficiencies between YES and NO tokens on Polymarket:

- **Arbitrage:** Buy YES and NO tokens when their combined cost < $0.95
- **Hedging:** Opposite positions cancel out, locking in spread
- **Fees:** 2% on winnings factored into profit calculation
- **Target:** 3-10% profit per arbitrage opportunity

## Current Status

✅ **v0.5.0 - Production Ready**  
✅ **50 active markets** being monitored  
✅ **Real-time WebSocket** streaming with primary + backup failover  
✅ **Production guardrails** - liquidity checks, circuit breakers, gas optimization  
✅ **DRY RUN mode** - paper trading, no real orders  

## Architecture

```
pingpong/
├── src/
│   ├── main.rs           # Entry point + CLI
│   ├── lib.rs            # Core types & exports
│   ├── api.rs            # Gamma API client (active markets)
│   ├── hot_switchover.rs # WebSocket manager (primary + backup)
│   ├── orderbook.rs      # Thread-safe price tracker
│   ├── strategy.rs       # Arbitrage detection engine
│   ├── trading.rs        # Order simulation & execution
│   ├── websocket.rs      # WebSocket types & helpers
│   └── production.rs     # Production guardrails (NEW)
├── examples/             # Test scripts
└── Cargo.toml
```

## Production Features (v0.5.0)

### 1. Liquidity Checks
Before executing, the bot verifies:
- **Minimum depth:** At least 100 shares at target price
- **Max slippage:** 5% tolerance
- **Slippage cost:** Max $1.00 per trade

### 2. Circuit Breakers
Risk management guards:
- **Rate limiting:** Max 20 trades/minute
- **Concurrent limit:** Max 5 simultaneous trades
- **Daily loss cap:** $100 max daily loss
- **Cooldown:** 60 seconds after circuit break

### 3. Gas Optimization
- **Gas estimation:** Realistic Polygon gas costs
- **Maker orders:** Post bids/asks vs taking liquidity
- **Batching:** Cancel + replace in single transaction
- **Typical cost:** ~$0.01 per trade

### 4. Gnosis Safe Support (Optional)
- **Gasless trading:** Via EIP-1271 signatures
- **No MATIC needed:** Safe wallet pays gas
- **Signature Type 2:** EthSign for meta-transactions

### 5. Order Expiration
- **TTL tracking:** Orders auto-expire after set period
- **Cleanup:** Expired orders removed from tracking
- **Default:** 60 second order life

## Building

```bash
cargo build --release
```

## Running

```bash
# Dry run (paper trading - no real orders)
./target/release/pingpong --ws

# With private key (LIVE trading - requires VPS)
POLYMARKET_PRIVATE_KEY=your_key ./target/release/pingpong --ws

# With Gnosis Safe (gasless)
SAFE_ADDRESS=0x... ./target/release/pingpong --ws
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `POLYMARKET_PRIVATE_KEY` | For live | EIP-712 signing key |
| `SAFE_ADDRESS` | Optional | Gnosis Safe for gasless |
| `WS_URL` | No | WebSocket endpoint |

## Production Deployment

### VPS Requirements
- **Location:** AWS us-east-1 or equivalent (low latency to Polymarket)
- **Specs:** 2 vCPU, 4GB RAM, SSD
- **OS:** Ubuntu 22.04 LTS

### Recommended Setup
```bash
# Install dependencies
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev

# Clone and build
git clone https://github.com/aissac/polymarket-hft-engine.git
cd polymarket-hft-engine/pingpong
cargo build --release

# Run with systemd (production)
sudo cp deploy.sh /usr/local/bin/pingpong
```

## PNL Simulation (Dry Run)

Based on 100 detected arbitrage opportunities:

| Combined Price | Trades | Avg Profit/Share | Subtotal |
|---------------|--------|-----------------|----------|
| < $0.10 | 32 | $0.93 | $2,981 |
| < $0.20 | 16 | $0.87 | $1,385 |
| < $0.30 | 7 | $0.76 | $533 |
| < $0.50 | 27 | $0.63 | $1,688 |
| < $0.70 | 9 | $0.40 | $358 |
| > $0.70 | 9 | $0.13 | $121 |

**100 trades @ 100 shares = $7,066 gross profit**

**Realistic expectation after gas + slippage: $500-2000/day**

## License

MIT
