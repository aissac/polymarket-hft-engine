# Pingpong

High-frequency arbitrage trading bot for Polymarket prediction markets.

## Strategy

Pingpong exploits pricing inefficiencies between YES and NO tokens on Polymarket:

- **Arbitrage:** Buy YES and NO tokens when their combined cost < $0.95
- **Hedging:** Opposite positions cancel out, locking in spread
- **Fees:** 2% on winnings factored into profit calculation
- **Target:** 3-10% profit per arbitrage opportunity

## Current Status

✅ **v0.4.0 - Gabagool Strategy** running in production  
✅ **50 active markets** being monitored  
✅ **Real-time WebSocket** streaming with primary + backup failover  
✅ **$7,000+ theoretical PnL** per 100 opportunities (dry run)  
✅ **DRY RUN mode** - paper trading, no real orders  

## Architecture

```
pingpong/
├── src/
│   ├── main.rs           # Entry point + CLI
│   ├── lib.rs            # Core types & constants
│   ├── api.rs            # Gamma API client (active markets)
│   ├── hot_switchover.rs # WebSocket manager (primary + backup)
│   ├── orderbook.rs      # Thread-safe price tracker
│   ├── strategy.rs       # Arbitrage detection engine
│   ├── trading.rs        # Order simulation & execution
│   └── websocket.rs      # WebSocket types & helpers
├── examples/             # Test scripts
└── Cargo.toml
```

## Features

### Phase 4.0: Gabagool Strategy
- Expensive-side skew: 3x position size when price > $0.55
- Directional betting: 70%+ confidence triggers larger positions
- Position limits: 150 shares per trade

### Hot Switchover (Phase 3.5)
- Dual WebSocket connections (primary + backup)
- Automatic failover every ~2 minutes
- Fresh snapshot on reconnect
- Halt/resume trading on disconnect

### Real-Time Arbitrage Detection
- WebSocket orderbook streaming
- Noise filtering via price bucket tracking
- In-flight trade locks (prevents duplicate execution)
- Concurrent YES+NO order simulation

## Building

```bash
cargo build --release
```

## Running

```bash
# Dry run (paper trading - no real orders)
./target/release/pingpong --ws

# With private key (LIVE trading)
POLYMARKET_PRIVATE_KEY=your_key ./target/release/pingpong --ws
```

## Environment Variables

| Variable | Required | Description |
|----------|----------|-------------|
| `POLYMARKET_PRIVATE_KEY` | No | EIP-712 signing key for live trading |
| `WS_URL` | No | WebSocket endpoint (default: Polymarket CLOB) |

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

## License

MIT
