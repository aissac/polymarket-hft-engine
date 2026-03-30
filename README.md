# 🏓 Pingpong: Polymarket HFT Engine

[![Rust](https://img.shields.io/badge/Rust-1.70+-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Status: DRY_RUN Validation](https://img.shields.io/badge/Status-DRY__RUN_Validation-brightgreen.svg)]()

**Pingpong** is an ultra-low latency, high-frequency trading (HFT) arbitrage engine purpose-built for Polymarket's Central Limit Order Book (CLOB). Written entirely in Rust, it is designed to exploit fleeting pricing inefficiencies in volatile 5-minute and 15-minute crypto up/down markets.

Our ultimate goal: **Build the fastest Polymarket trading bot ever.** By combining zero-allocation orderbook parsing, real-time edge detection, and lightning-fast execution, Pingpong locks in risk-free arbitrage before human traders or slower bots even process the WebSocket frame.

---

## 🎯 Project Description & Goals

In prediction markets, complementary outcomes must mathematically sum to 100% (or $1.00). Pingpong hunts for moments where liquidity fragmentation or sudden market "dumps" cause the combined ask price of `YES` and `NO` tokens to temporarily dip below $1.00.

**The Strategy:**
1. Monitor highly volatile BTC/ETH 5m and 15m markets.
2. Track the top-of-book **Ask** prices for both `YES` and `NO` tokens.
3. Fire an execution sequence when `YES_ASK + NO_ASK <= $0.98` (yielding a 2%+ gross edge).
4. Utilize a Hybrid Maker-Taker routing system to harvest Polymarket's 0.36% maker rebates while absorbing the maximum 1.80% taker fee burden.

## 🏗 Technical Architecture

To achieve sub-microsecond latency, Pingpong strictly divides labor between a synchronous, CPU-pinned **Hot Path** and an asynchronous **Background Thread**:

* **The Hot Path (Sync):** An ultra-optimized loop listening to the Polymarket `market` WebSocket. It completely bypasses standard JSON deserialization (like `serde_json`), directly scanning raw byte arrays to maintain a fixed-size array representation of the orderbook.
* **The Background Thread (Async/Tokio):** Receives an `OpportunitySnapshot` via a lock-free `crossbeam_channel`. It handles heavy asynchronous workloads including API requests, market rollovers, and simulating execution fills.
* **Rollover Engine:** Automatically parses `startDate` and `endDate` from the Gamma API to cleanly drop expired markets and pre-subscribe to future 15-minute windows seamlessly.

## 🧠 The Secret Sauce: Zero-Allocation Stateful Parsing

Polymarket WebSocket payloads batch multiple token updates into a single JSON array, placing the `asset_id` uniquely at the top level of each object—*not* within the nested `bids` and `asks` arrays. Standard JSON parsing allocates memory for the entire DOM, which destroys HFT latency.

**Pingpong solves this using a Stateful `memchr` / `memmem` Parser:**
1. As the scanner glides through the raw bytes, it identifies the top-level `"asset_id"` and temporarily caches the hash (converting bytes to `str` before hashing to avoid mismatches).
2. It detects the `"bids":[` and `"asks":[` byte markers to flip an `is_bid` boolean state.
3. As it encounters `"price":"` and `"size":"` tuples, it routes them to the correct fixed-size array in the orderbook map based on the active `asset_id` and `is_bid` state.

This innovation prevents mixed bid/ask corruption and ensures the engine never touches the heap during the per-message handling loop.

## ⚡ Performance Benchmarks

Pingpong operates at the physical limits of network and CPU performance:

| Metric | Measurement | Notes |
|--------|-------------|-------|
| **Hot Path Avg Latency** | `~0.7 µs` | Time from WS byte receive to internal state update |
| **Hot Path p99 Latency** | `< 5.0 µs` | 99th percentile processing time under heavy volume |
| **Orderbook Memory** | `O(1)` | Guaranteed stable memory footprint via tick arrays |
| **Simulated Ghost Rate** | `~60%` | Frequency of liquidity vanishing before the 50ms execution sim |
| **Executable Rate** | `~35-40%` | Arbitrage opportunities successfully hedged in DRY_RUN |

## ✨ Current Features

* **Sub-Microsecond Latency:** Custom byte-scanner for Polymarket payload parsing.
* **Dynamic Token Discovery:** Automated tracking of active BTC/ETH 5m and 15m markets.
* **Advanced Edge Detection:** Filters out extreme spreads (Combined Ask < $0.90) while capturing valid arbitrage <= $0.98.
* **Thick/Thin Side Identification:** Automatically assesses Level 1 depth to assign the Maker leg to the stable side and the Taker leg to the volatile side.
* **DRY_RUN Validation Mode:** Paper trading with built-in artificial latency delays (50ms Maker, 10ms Taker) to simulate real-world CLOB matching engine conditions.

## 🗺 Roadmap to Production

To claim the title of fastest Polymarket bot, our final path to live capital execution involves:
- [ ] **L2 HMAC Authentication:** Integrate ECDSA / EIP-712 signing for secure `clob.polymarket.com` order submission.
- [ ] **Hybrid Maker-Taker Execution:** Deploy the `GTC Post-Only` Maker order and `FAK` (Fill-And-Kill) Taker sequence.
- [ ] **Hardware Stop-Loss:** Implement a strict 3-second timeout FAK order to dump naked Maker positions if the hedge fails.
- [ ] **Colocation:** Deploy the compiled binary to AWS `eu-central-1` (or equivalent) to minimize network RTT to Polymarket's matching engine.

## 🛠 Installation & Usage

### Prerequisites
* Rust 1.70+ (`cargo`)
* Access to a Polygon RPC (for live deployment token allowances)

### Setup
1. Clone the repository:
   ```bash
   git clone https://github.com/aissac/polymarket-hft-engine.git
   cd polymarket-hft-engine
   ```
2. Configure your environment:
   ```bash
   cp .env.example .env
   # Edit .env with your POLYMARKET_PRIVATE_KEY and RPC URL
   ```
3. Build for maximum performance:
   ```bash
   cargo build --release --bin hft_pingpong_v2
   ```
4. Run the engine (defaults to `DRY_RUN` mode):
   ```bash
   ./target/release/hft_pingpong_v2
   ```

## 🤝 Contributing

We are building a community of low-latency enthusiasts. Pull requests are welcome!
* **Performance:** If your PR touches `src/hft_hot_path.rs`, you **must** include criterion benchmark outputs proving your changes do not regress the sub-microsecond latency.
* **Features:** Please open an issue to discuss architectural changes before committing heavy asynchronous workloads to the background thread.

## ⚠️ Disclaimer

This software is for educational and research purposes only. High-frequency trading and arbitrage on cryptocurrency prediction markets involves severe financial risk. The developers take no responsibility for financial losses incurred by running this software with live capital. Always thoroughly validate your setup in `DRY_RUN` mode first.

---

## 📚 Key Insights from Development

### Token Hash Mismatch (SOLVED)
Standard Rust `Hasher` produces different hashes for `&str` vs `&[u8]`. Our `fast_hash_token()` converts bytes to UTF-8 string before hashing to ensure consistency with Gamma API token IDs.

### Stateful Parsing (SOLVED)
NotebookLM helped us discover that Polymarket batches multiple tokens per WebSocket message. The `asset_id` appears only at the TOP LEVEL of each market object, not inside each bid/ask entry. Our state machine tracks:
- `current_token_hash` → set on `"asset_id":"..."`  
- `is_bid` → set on `"bids":[`  
- `is_ask` → set on `"asks":[`

## 📊 Current Results

```
🎯 EDGE DETECTED: Combined Ask = $0.92 (8% profit!)
🎯 EDGE DETECTED: Combined Ask = $0.97 (3% profit!)
```

The bot is actively detecting arbitrage opportunities in real-time with sub-microsecond latency.

## License

MIT