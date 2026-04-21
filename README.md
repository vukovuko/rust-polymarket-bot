# Polymarket Weather Trading Bot

Rust bot that trades weather temperature markets on Polymarket. Runs 6 ensemble weather models (GFS, ECMWF, ICON, AIFS, AIGEFS, GEM), computes forecast probabilities via Gaussian CDF with bias correction, and places maker limit orders when the forecast disagrees with the market price by more than 10%.

## What it does

Scans Polymarket weather markets for ~30 cities every 30 minutes. For each "will the high be >= X degrees" bucket, it compares the ensemble forecast probability against the market price. When it finds edge (forecast says 65%, market says 45%), it sizes a bet using quarter-Kelly criterion and places a maker order (zero fees). Holds to resolution — winner pays $1/share.

Also runs a cross-market consistency scanner that detects when a higher temperature threshold is priced above a lower one for the same city/date (monotonicity violation), and skips dominated buckets.

BTC 5-minute market scanning and arb detection run in paper-trade mode for data collection only.

## Requirements

- Rust (latest stable)
- Docker + Docker Compose (for production)
- Polymarket account with funded wallet (USDC on Polygon)
- Telegram bot token (optional, for alerts)

## Setup

1. Clone the repo
2. Copy `.env.example` to `.env`
3. Fill in `PRIVATE_KEY` with your Polymarket wallet private key (must be a GnosisSafe/browser wallet, not Magic Link)
4. Fill in `TG_BOT_TOKEN` and `TG_CHAT_ID` for Telegram alerts
5. Set `BANKROLL` to your actual available USDC balance
6. Leave `ALERT_ONLY=true` for the first few days to validate signals

## Run locally

```
cargo build --release
cargo run --release
```

## Run with Docker (production)

```
docker compose up -d --build
docker compose logs -f
```

## Go live

After validating signals in alert-only mode for a few days, set `ALERT_ONLY=false` in `.env` and restart. The bot will place real orders.

## Key config

- `STD_INFLATION` — how much to distrust the forecast models. Higher = fewer but safer bets. Default 2.0.
- `EDGE_THRESHOLD` — minimum edge to bet. Default 10%.
- `MAX_TRADE_USD` — max cost per bet. Default $5.
- `KELLY_FRACTION` — fraction of Kelly criterion. Default 0.25 (quarter Kelly).
- `BANKROLL` — your available balance. Update this as your balance changes.

## Monitoring

The bot sends Telegram alerts for every edge found, every order placed/failed, and a health heartbeat every 2 hours. Position deduplication is persisted in `logs/bet_conditions.txt` — one bet per market, no stacking.

## VPS notes

Runs on a Hetzner VPS (Helsinki). Must NOT be hosted in a Polymarket-geoblocked country (US, Germany, France, etc). Finland, Singapore, and most of Asia/South America work.
