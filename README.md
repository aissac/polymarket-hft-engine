# Pingpong - Polymarket HFT Engine

[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Status: DRY_RUN Validation](https://img.shields.io/badge/Status-DRY__RUN_Validation-brightgreen.svg)]()

**Ultra-low latency arbitrage engine for Polymarket prediction markets.**

---

## Current Status

✅ **Step 1 Complete - Detection Engine Validated**

| Component | Status | Details |
|-----------|--------|---------|
| Latency | ✅ | 0.7µs avg, <5µs p99 |
| Token Hash | ✅ | Fixed bytes→str consistency |
| Stateful Parsing | ✅ | asset_id at TOP LEVEL only |
| Edge Detection | ✅ | Detecting $0.95-$0.98 combined ASK |
| Minimum Size Filter | ✅ | TARGET_SHARES=100 |
| DRY RUN Mode | ✅ | Paper trading, no real orders |

**Live Output:**
```
🎯 EDGE DETECTED: Combined Ask = $0.97 (YES=45.00¢, NO=52.00¢)
```

---

## Roadmap to Production

### Phase 1: Core Infrastructure ⏳ IN PROGRESS
- [ ] Add `polymarket-client-sdk` dependency
- [ ] Create `src/auth.rs` - L1/L2 authentication
- [ ] Create `src/user_ws.rs` - User WebSocket thread
- [ ] Create `src/execution.rs` - Execution thread

### Phase 2: Thread Bridges
- [ ] Create `FillConfirmation` struct
- [ ] Create `OpportunitySnapshot` struct
- [ ] Add `crossbeam_channel::unbounded` between hot path and execution
- [ ] Add `crossbeam_channel` between execution and user_ws

### Phase 3: Order Flow
- [ ] Implement Maker GTC Post-Only order
- [ ] Implement Taker FAK (Fill-And-Kill) order
- [ ] Implement 3-second timeout
- [ ] Implement order cancellation (DELETE /order)

### Phase 4: Error Handling
- [ ] User WS reconnection with exponential backoff
- [ ] PAUSE signal to hot path on WS disconnect
- [ ] Stop-Loss market sell logic
- [ ] Partial fill handling

### Phase 5: Testing
- [ ] Test on Amoy testnet (Chain ID: 80002)
- [ ] Validate edge detection still works
- [ ] Validate Maker → MATCHED → Taker flow
- [ ] Validate Stop-Loss on Taker failure
- [ ] Switch to Polygon Mainnet

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     HOT PATH (Sync, No Tokio)                           │
│  • Sub-microsecond latency (0.7µs)                                       │
│  • Parses WebSocket bytes with memchr                                   │
│  • Detects edge when Combined Ask < $0.98                               │
│  • Calls tx.send(OpportunitySnapshot) - NON-BLOCKING                    │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │ crossbeam_channel::unbounded
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                   EXECUTION THREAD (Async/Tokio)                       │
│  • Waits for edges from rx.recv().await                                 │
│  • Posts Maker GTC Post-Only order via REST                             │
│  • Waits for fill_rx.recv_timeout(3 seconds)                           │
│  • On MATCHED: Fires Taker FAK order                                    │
│  • On TIMEOUT: Cancels Maker, triggers Stop-Loss                        │
└────────────────────────────────┬────────────────────────────────────────┘
                                 │ crossbeam_channel
                                 ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                   USER WS THREAD (Async/Tokio)                         │
│  • Connects to wss://ws-subscriptions-clob.polymarket.com/ws/user      │
│  • Subscribes to orders and trades channels                            │
│  • On order.status == "matched": sends FillConfirmation               │
│  • On disconnect: exponential backoff + PAUSE signal                  │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Key Insights from Development

### Token Hash Mismatch (SOLVED)
Standard Rust `Hasher` produces different hashes for `&str` vs `&[u8]`. Our `fast_hash_token()` converts bytes to UTF-8 string before hashing.

### Stateful Parsing (SOLVED)
Polymarket batches multiple tokens per WebSocket message. The `asset_id` appears only at the TOP LEVEL of each market object, not inside each bid/ask entry.

### Minimum Size Filter (NOTEBOOKLM GUIDANCE)
Depth of "5 shares" = only $2.50 liquidity. Added `TARGET_SHARES=100` minimum to prevent partial fills on thin liquidity.

---

## Monitoring

The bot sends periodic Telegram reports with:

```
[ 🏓 PINGPONG HFT ENGINE ]
MODE: DRY_RUN | TIME: 2026-03-30 12:23:39

=== ⚡ SYSTEM PERFORMANCE ===
Msg rate    : 1,542/sec
Pair Checks : 9,100,000 (1.07k/s)
Total Edges : 91

=== 🎯 EDGE DISTRIBUTION (ASK < $0.98) ===
$0.90-0.92   :            (  0) [High α]
$0.93-0.95   :            (  0) [Mid α]
$0.96-0.98   : ██████████▏ ( 29) [Low α]

=== 💧 LATEST EDGES ===
YES(¢)  NO(¢)   Combined
  45.0    52.0  $ 0.9700

=== 📊 ESTIMATED FILL RATE ===
Ghost Rate: ~60% | Executable: ~40%
Real Opportunities: ~36
```

---

## Documentation

| File | Description |
|------|-------------|
| [PATH_TO_PRODUCTION.md](PATH_TO_PRODUCTION.md) | Complete roadmap for live trading |
| [AUTHENTICATION_CODE.md](AUTHENTICATION_CODE.md) | SDK + EIP-712 + HMAC implementation |
| [USER_WEBSOCKET_CODE.md](USER_WEBSOCKET_CODE.md) | Fill confirmation bridge |
| [FINAL_INTEGRATION.md](FINAL_INTEGRATION.md) | Thread architecture + startup sequence |

---

## Performance

| Metric | Value |
|--------|-------|
| Hot Path Avg Latency | ~0.7 µs |
| Hot Path p99 Latency | < 5.0 µs |
| Orderbook Memory | O(1) |
| Simulated Ghost Rate | ~60% |
| Executable Rate | ~40% |

---

## Repository

**GitHub:** https://github.com/aissac/polymarket-hft-engine

---

## License

MIT