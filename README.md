# Gabagool Bot

High-frequency arbitrage trading bot for Polymarket prediction markets.

## Strategy

Gabagool is an arbitrage strategy that exploits pricing inefficiencies between YES and NO tokens:

- Buy YES and NO tokens when their combined cost < $0.95
- Guaranteed profit after 2% Polymarket fee
- Target: ~3% profit per arbitrage

## Architecture

```
gabagool-bot/
├── src/
│   ├── main.rs        # Entry point
│   ├── lib.rs         # Core types
│   ├── orderbook.rs   # Order book tracking
│   ├── websocket.rs   # Polymarket WebSocket client
│   └── strategy.rs   # Trading strategy engine
├── Cargo.toml
└── README.md
```

## Phases

| Phase | Status | Description |
|-------|--------|-------------|
| 1 | ✅ Complete | WebSocket + Orderbook (read-only) |
| 2 | ⏳ Pending | EIP-712 signing + REST execution |
| 3 | ⏳ Pending | Strategy + leg risk fallback |
| 4 | ⏳ Pending | AWS deployment |

## Building

```bash
cargo build --release
```

## Running

```bash
./target/release/gabagool-bot
```

## License

MIT
