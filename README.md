# Pingpong

High-frequency arbitrage trading bot for Polymarket prediction markets.

## Strategy

Pingpong is an arbitrage strategy that exploits pricing inefficiencies between YES and NO tokens:

- Buy YES and NO tokens when their combined cost < $0.95
- Guaranteed profit after 2% Polymarket fee
- Target: ~3% profit per arbitrage

## Architecture

```
pingpong/
├── src/
│   ├── main.rs        # Entry point
│   ├── lib.rs         # Core types & constants
│   ├── api.rs         # Polymarket REST API client
│   ├── orderbook.rs   # Thread-safe price tracker
│   └── strategy.rs    # Arbitrage detection engine
├── Cargo.toml
└── README.md
```

## Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 1 | ✅ Complete | REST API + Orderbook tracking (read-only) |
| 2 | ⏳ Pending | EIP-712 signing + order execution |
| 3 | ⏳ Pending | Leg risk fallback + circuit breakers |
| 4 | ⏳ Pending | AWS deployment (EC2 + Docker) |

## Building

```bash
cargo build --release
```

## Running

```bash
./target/release/pingpong
```

## License

MIT
