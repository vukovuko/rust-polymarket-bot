# Configuration Reference

All configuration is via environment variables, parsed in `src/config.rs`.
See `.env.example` for a template.

## Required

| Variable | Description |
|----------|-------------|
| `PRIVATE_KEY` | Polymarket wallet private key (0x... , 66 chars) |

## Risk Management

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_TRADE_USD` | 5 | Max USD per individual trade |
| `MAX_DAILY_EXPOSURE` | 20 | Max total daily exposure across all positions |
| `KILL_SWITCH_LOSS` | 10 | Stop all trading if daily loss exceeds this |
| `MAX_ACTIVE_POSITIONS` | 3 | Max simultaneous open positions |

## Strategy — Arb Scanner

| Variable | Default | Description |
|----------|---------|-------------|
| `ARB_THRESHOLD` | 0.02 | Min net edge for complement arb (combined < 1 - threshold) |
| `SPREAD_OFFSET` | 0.03 | Legacy spread collector offset (not actively used) |

## Strategy — Weather

| Variable | Default | Description |
|----------|---------|-------------|
| `EDGE_THRESHOLD` | 0.10 | Min edge (gaussian_prob - market_price) to alert. 10% = conservative |
| `MIN_PROBABILITY` | 0.30 | Ignore buckets where our forecast prob < 30% |
| `MAX_WEATHER_POSITION` | 10 | Max USD per weather trade (Kelly cap) |
| `MAX_ENTRY_PRICE` | 0.65 | Skip buckets priced above 65c (bad risk/reward) |
| `WEATHER_SCAN_INTERVAL` | 1800 | Seconds between normal scans (30 min) |
| `WEATHER_FAST_SCAN_INTERVAL` | 300 | Seconds between scans during GFS windows (5 min) |

## Gaussian CDF Tuning

| Variable | Default | Description |
|----------|---------|-------------|
| `STD_INFLATION` | 1.3 | Multiply ensemble std by this to correct underdispersion |
| `APPLY_BIAS_CORRECTION` | true | Apply per-model temperature bias corrections |

## WebSocket

| Variable | Default | Description |
|----------|---------|-------------|
| `REQUIRE_WS_PRICE` | false | If true, skip edges without live WS price. Leave false — Gamma fallback needed for first scan before WS has all prices |

## Kelly Sizing

| Variable | Default | Description |
|----------|---------|-------------|
| `BANKROLL` | 77 | Total bankroll in USD for Kelly calculation |
| `KELLY_FRACTION` | 0.25 | Fraction of full Kelly to use. 0.25 = quarter Kelly (conservative) |

## API

| Variable | Default | Description |
|----------|---------|-------------|
| `POLY_API_URL` | https://clob.polymarket.com | Polymarket CLOB REST endpoint |

## Telegram

| Variable | Default | Description |
|----------|---------|-------------|
| `TG_BOT_TOKEN` | (none) | Telegram bot token from @BotFather |
| `TG_CHAT_ID` | (none) | Telegram chat ID for alerts |

Both must be set for Telegram alerts. If either is missing, alerts are logged only.

## Mode

| Variable | Default | Description |
|----------|---------|-------------|
| `ALERT_ONLY` | true | Set to false for live trading. Keep true until calibration is proven |

## Logging

| Variable | Default | Description |
|----------|---------|-------------|
| `RUST_LOG` | info | Tracing level (trace/debug/info/warn/error) |

## Tuning Guidelines

**Conservative (current)**: EDGE_THRESHOLD=0.10, KELLY_FRACTION=0.25, MAX_ENTRY_PRICE=0.65
**Moderate**: EDGE_THRESHOLD=0.08, KELLY_FRACTION=0.33, MAX_ENTRY_PRICE=0.70
**Aggressive**: EDGE_THRESHOLD=0.05, KELLY_FRACTION=0.50, MAX_ENTRY_PRICE=0.80

Start conservative. Tighten after check_edges shows good calibration.
Don't go below EDGE_THRESHOLD=0.05 — model error is at least 3-5%.
