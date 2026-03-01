# CLAUDE.md — Polymarket Trading Bot



## What This Project Is



A Rust bot that runs on a VPS and trades on Polymarket's 5-minute BTC binary markets. It connects to Binance for real-time BTC prices and Polymarket's CLOB API for order placement. It has three modes that run concurrently.



## Architecture



```

Binance WebSocket (BTC/USDT trades)

        │

        ▼

┌─────────────────────────────────────┐

│           Strategy Engine           │

│                                     │

│  Mode 1: Arb Scanner               │

│    - Scans all binary markets       │

│    - Finds YES_ask + NO_ask < 0.98  │

│    - Auto-buys both sides           │

│                                     │

│  Mode 2: Momentum Sniper           │

│    - Detects BTC move >0.15% in 5s  │

│    - Posts maker limit order on     │

│      winning side of current 5-min  │

│      BTC market at 85-95 cents      │

│                                     │

│  Mode 3: Spread Collector           │

│    - Posts limit bids on both YES   │

│      and NO below mid price         │

│    - If both fill: cost < $1.00,    │

│      guaranteed profit at settle    │

│                                     │

├─────────────────────────────────────┤

│           Risk Manager              │

│  - Max $5 per trade                 │

│  - Max $20 daily exposure           │

│  - Kill switch: stop all if daily   │

│    loss exceeds $10                 │

│  - Track P&L per mode separately    │

├─────────────────────────────────────┤

│           Order Executor            │

│  - EIP-712 order signing            │

│  - MUST include feeRateBps field    │

│  - Maker-only orders (zero fees)    │

│  - Cancel/replace loop              │

├─────────────────────────────────────┤

│         Telegram Alerts             │

│  - Every trade: entry, exit, P&L    │

│  - Arb opportunities found          │

│  - Momentum signals detected        │

│  - Daily summary                    │

│  - Errors and kill switch triggers  │

└─────────────────────────────────────┘

```



## Technical Requirements



### Language & Runtime

- Rust (latest stable)

- Tokio async runtime

- Target: Linux x86_64 (VPS)



### External Connections

1. **Binance WebSocket**: `wss://stream.binance.com:9443/ws/btcusdt@trade` — real-time BTC price

2. **Polymarket CLOB REST API**: `https://clob.polymarket.com` — markets, order books, order placement

3. **Polymarket CLOB WebSocket**: `wss://ws-subscriptions-clob.polymarket.com/ws/market` — real-time order book updates

4. **Polygon RPC** (for on-chain settlement): public or private RPC endpoint

5. **Telegram Bot API**: alerts and trade notifications



### Key Dependencies

Using the official Polymarket Rust SDK (`polymarket-client-sdk`), which handles REST API, EIP-712 signing, and order building via `alloy` (not ethers).

```bash

cargo add polymarket-client-sdk --features clob,gamma,ws,tracing

cargo add tokio --features full

cargo add tokio-tungstenite --features rustls-tls-webpki-roots

cargo add reqwest --features json,rustls --no-default-features

cargo add serde --features derive

cargo add serde_json

cargo add futures-util

cargo add tracing

cargo add tracing-subscriber --features env-filter

cargo add dotenvy

cargo add anyhow

cargo add url

```

Do NOT hardcode versions in Cargo.toml — let `cargo add` resolve them.



## Polymarket API Details



### CRITICAL: Post-February 2026 Rules

- The 500ms taker delay was removed Feb 18, 2026

- All orders MUST include `feeRateBps` in the signed payload

- Maker orders pay ZERO fees and earn daily USDC rebates

- Taker fees: `fee = C × 0.25 × (p × (1-p))^2` — max ~1.56% at p=0.50

- ALWAYS use maker/limit orders, NEVER market/taker orders



### Market Structure

- Binary markets: exactly 2 tokens (YES and NO)

- `P(YES) + P(NO) = $1.00` enforced at smart contract level

- Settlement: winning side pays $1.00 per share, losing side pays $0.00

- Built on Gnosis Conditional Token Framework (CTF), ERC-1155 tokens

- Chain: Polygon (chain_id 137)

- Tick sizes: $0.01 or $0.001

- Valid prices: $0.01 to $0.99

- Rate limit: 3,000 order requests per 10-minute window



### REST Endpoints Used

```

GET  /markets?active=true&closed=false&limit=100&next_cursor=X

     Returns paginated list of active markets with token IDs



GET  /book?token_id=TOKEN_ID

     Returns order book: { bids: [{price, size}], asks: [{price, size}] }



GET  /fee-rate?tokenID=TOKEN_ID

     Returns fee rate in bps for this token



POST /order

     Place a signed order. Body must include EIP-712 signature.



DELETE /order/ORDER_ID

     Cancel an order



DELETE /orders

     Cancel all orders (kill switch)

```



### Order Signing (EIP-712)

Orders must be signed according to Polymarket's EIP-712 schema. The signed payload includes:

```json

{

  "salt": "random_number",

  "maker": "0xYOUR_ADDRESS",

  "signer": "0xYOUR_ADDRESS",

  "taker": "0x0000000000000000000000000000000000000000",

  "tokenId": "TOKEN_ID_STRING",

  "makerAmount": "USDC_AMOUNT_IN_RAW_UNITS",

  "takerAmount": "TOKEN_AMOUNT_IN_RAW_UNITS",

  "expiration": "0",

  "nonce": "0",

  "feeRateBps": "FEE_RATE_FROM_API",

  "side": "BUY",

  "signatureType": 2

}

```

- USDC has 6 decimals (1 USDC = 1000000 raw units)

- Conditional tokens have 6 decimals

- The `feeRateBps` field MUST match what the API returns for that token

- Reference the py-clob-client Python SDK or polymarket-rs Rust crate for signing implementation details



### 5-Minute BTC Markets

- 288 markets per day (24h × 12 per hour)

- Each resolves via Chainlink oracle pulling from Binance/CoinGecko

- Market question format: "Will BTC go up or down in the next 5 minutes?"

- New market opens as previous one closes

- To find current 5-min market: filter markets by question containing "BTC" and "5" and check end_date_iso is within next 5 minutes



## Strategy Details



### Mode 1: Complement Arbitrage

```

FOR each active binary market:

    yes_ask = best ask price for YES token

    no_ask = best ask price for NO token

    combined = yes_ask + no_ask



    IF combined < 0.98:  # 0.98 accounts for 2% winner fee

        net_edge = 1.00 - combined - 0.02

        IF net_edge > min_threshold:

            BUY yes_ask amount of YES tokens (maker limit order)

            BUY no_ask amount of NO tokens (maker limit order)

            # One side ALWAYS wins $1.00. Cost was < $0.98.

            # Profit = $1.00 - cost - fee

```

- Scan every 30 seconds

- Use Fill-or-Kill semantics: if one side can't fill, cancel both

- At $5 per side ($10 total), a 2% net edge = $0.20 profit per arb



### Mode 2: Momentum Sniper

```

EVERY 500ms:

    prices_5s = BTC prices from last 5 seconds (from Binance WS)

    change = (newest - oldest) / oldest



    IF abs(change) > 0.15%:

        direction = UP if change > 0 else DOWN



        Find current 5-minute BTC market on Polymarket

        Get order book for the token matching direction



        IF best_ask < 0.90:  # still mispriced

            Post MAKER limit order at best_ask or slightly above

            # If filled: hold to resolution. Winner pays $1.00

            # Profit: $1.00 - entry_price - fee (fee is 0 for makers)

```

- Only enter if Polymarket price hasn't caught up yet

- Max 1 position per 5-min window

- Don't chase: if ask already > $0.90, the edge is gone



### Mode 3: Spread Collector (PBot1 Style)

```

FOR each 5-minute BTC market:

    yes_mid = (best_bid_yes + best_ask_yes) / 2

    no_mid = (best_bid_no + best_ask_no) / 2



    # Post bids below mid on BOTH sides

    post_bid(YES, price = yes_mid - 0.03, size = $5)

    post_bid(NO, price = no_mid - 0.03, size = $5)



    # If BOTH fill:

    #   total_cost = yes_fill + no_fill < $1.00 (because below mid on both)

    #   One side pays $1.00 at resolution

    #   Profit = $1.00 - total_cost - fee



    # If only ONE fills:

    #   You have directional exposure. Hold to resolution.

    #   This is the risk: ~50% chance of loss on single-sided fill.



    # Cancel and replace orders every 5 seconds to track moving mid

```



## Risk Management Rules



- Max $5 per individual trade

- Max $20 total daily exposure

- Kill switch: if daily P&L drops below -$10, cancel ALL orders, close ALL positions, stop trading for the day, send Telegram alert

- Track P&L separately per mode (arb, momentum, spread)

- Never hold more than 3 active positions simultaneously

- Log every order placement, fill, cancel, and P&L event



## Project Structure



```

polymarket-bot/

├── Cargo.toml

├── .env                    # secrets (git-ignored)

├── .env.example            # template (committed)

├── CLAUDE.md               # this file

├── Dockerfile

├── docker-compose.yml

├── src/

│   ├── main.rs             # entrypoint, main loop, tokio::select

│   ├── config.rs           # Config struct from env vars

│   ├── binance.rs          # Binance WebSocket feed

│   ├── polymarket/

│   │   ├── mod.rs

│   │   ├── client.rs       # Thin wrapper around polymarket-client-sdk

│   │   ├── market_finder.rs # BTC 5-min market discovery

│   │   └── types.rs        # BotMarket, SimpleBook, Direction

│   ├── strategies/

│   │   ├── mod.rs

│   │   ├── arb.rs          # Mode 1: complement arbitrage

│   │   ├── momentum.rs     # Mode 2: momentum sniper

│   │   └── spread.rs       # Mode 3: spread collector

│   ├── risk.rs             # Risk manager, position tracking, kill switch

│   └── telegram.rs         # Alert sender

├── tests/

│   ├── test_arb.rs

│   └── test_momentum.rs

└── logs/                   # runtime logs (git-ignored)

```



## Environment Variables



```bash

# Required

PRIVATE_KEY=0x...              # Polymarket wallet private key



# Risk

MAX_TRADE_USD=5                # Max per trade

MAX_DAILY_EXPOSURE=20          # Max total daily

KILL_SWITCH_LOSS=10            # Stop trading if daily loss exceeds this



# Strategy

ARB_THRESHOLD=0.02             # Min net edge for arb (after fees)

MOMENTUM_THRESHOLD=0.0015     # Min BTC % move in 5s

SPREAD_OFFSET=0.03            # How far below mid to post bids



# API

POLY_API_URL=https://clob.polymarket.com

POLY_WS_URL=wss://ws-subscriptions-clob.polymarket.com/ws/market



# Telegram (optional but recommended)

TG_BOT_TOKEN=your_bot_token

TG_CHAT_ID=your_chat_id



# Mode

ALERT_ONLY=true                # Set to false when ready for live execution



# Logging

RUST_LOG=info

```



## Build & Run



### Development

```bash

cargo build

cargo run

```



### Production (Docker)

```bash

docker compose up -d

docker compose logs -f

```



## Implementation Order



Build and test in this exact order:



1. **Config + logging** — parse .env, set up tracing

2. **Binance WebSocket** — connect, parse trades, maintain 5s rolling price buffer

3. **Polymarket REST client** — fetch markets, fetch order books, fetch fee rates

4. **Arb scanner (Mode 1, alert only)** — scan markets, detect arbs, send Telegram alerts

5. **Momentum detector (Mode 2, alert only)** — detect BTC moves, send Telegram alerts

6. **Run alert-only for 3-5 days** — validate signals match reality

7. **EIP-712 order signing** — implement Polymarket order signing

8. **Order placement and cancellation** — place maker orders, cancel stale orders

9. **Arb auto-execution (Mode 1)** — buy both sides when arb found

10. **Momentum auto-execution (Mode 2)** — post maker order on momentum signal

11. **Spread collector (Mode 3)** — post both-side bids, manage fills

12. **Risk manager with kill switch** — enforce all limits, emergency shutdown



Do NOT skip to auto-execution before the alert-only validation phase.



## Testing Approach



- Unit test arb detection with mock order book data

- Unit test momentum detection with synthetic price sequences

- Integration test Polymarket client against live API (read-only, no orders)

- Paper trade: log what WOULD have been traded, track hypothetical P&L

- Live trade: start with $3-5 per trade, Mode 1 only, then add modes one at a time



## Common Pitfalls



1. **Forgetting `feeRateBps` in order signing** — orders get rejected silently

2. **Using REST polling instead of WebSocket for Binance** — too slow, stale prices

3. **Not handling Polymarket API rate limits** — 3,000 requests per 10 minutes

4. **Not canceling stale orders** — if market moves, your old limit order gets adversely filled

5. **Single-sided fill on spread mode** — you now have directional exposure, must handle this

6. **Polygon RPC failures** — always have fallback RPC endpoint

7. **WebSocket disconnects** — implement reconnection with exponential backoff on ALL WS connections

