# Project Overview

## What This Is

A Rust bot that trades weather temperature markets on Polymarket. It fetches ensemble
weather forecasts from 3 models (GFS, ECMWF, ICON), computes bucket probabilities using
Gaussian CDF with bias correction, compares them to market prices, and identifies edges
where the market is mispriced. Currently running in alert-only mode (no real trades).

There's also a real-time arb scanner for BTC 5-minute binary markets, but the main focus
is weather.

## Architecture

```
Open-Meteo API (GFS + ECMWF + ICON ensembles)
        |
        v
+--------------------+     +------------------+
| WeatherFetcher     |     | MarketFinder     |
| - 3 models         |     | - 14 cities      |
| - bias correction  |     | - 3 days ahead   |
| - Gaussian CDF     |     | - 9 buckets/city |
+--------------------+     +------------------+
        |                          |
        v                          v
+------------------------------------------+
|         WeatherStrategy                  |
| - Compare forecast prob vs market price  |
| - Calculate edge + Kelly bet size        |
| - Log to CSV + send Telegram alerts      |
+------------------------------------------+
        |
        v
+------------------------------------------+
|         Main Loop (tokio::select!)       |
| - Process StrategyActions                |
| - Route: Alert / PlaceOrder / ArbExec    |
| - Risk checks before any order           |
+------------------------------------------+
        |
        v
  PolyClient (REST + EIP-712 signing)
  TelegramSender (alerts)
  RiskManager (limits + kill switch)
```

Concurrently, PolyWs subscribes to real-time Polymarket WebSocket for:
- BTC 5-min token prices (arb detection on every price tick)
- Weather token prices (live prices for edge calculation)
- New market discovery (instant, not just REST polling)

## Data Flow

1. **Every 30 min** (or 5 min during GFS update windows): WeatherStrategy triggers a scan
2. **Per city-date**: Fetch ensemble forecasts from Open-Meteo (3 models concurrently)
3. **Per bucket**: Compute Gaussian CDF probability, compare to market price
4. **If edge > threshold**: Log to CSV, send Telegram alert with Kelly-sized bet
5. **Prices**: Prefer real-time WS price, fall back to Gamma API cached price

## Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point, tokio::select! loop, action routing |
| `src/weather.rs` | Forecast fetching, Gaussian CDF probability engine |
| `src/strategies/weather.rs` | Edge detection, Kelly sizing, CSV logging |
| `src/polymarket/market_finder.rs` | Market discovery (BTC + weather), city configs |
| `src/polymarket/ws.rs` | WebSocket manager, real-time arb detection |
| `src/polymarket/client.rs` | REST API wrapper (SDK) |
| `src/bin/check_edges.rs` | Backtesting tool, compares edges vs actual temps |
| `src/config.rs` | All env var parsing |
| `src/telegram.rs` | Telegram alert sender |
| `src/risk.rs` | Position limits, daily exposure, kill switch |
| `src/binance.rs` | BTC/USDT real-time price feed |

## Tech Stack

- **Rust** with Tokio async runtime
- **polymarket-client-sdk** v0.4.3 (official SDK, handles EIP-712 signing)
- **alloy** for crypto (not ethers)
- **rustls** everywhere (no system OpenSSL)
- **Docker** for deployment on VPS
- **Open-Meteo** for weather forecasts (free tier, ~9.2k req/day)
- **Weather Company API** for resolution verification (same source Polymarket uses)

## Current Status

Running in Docker on VPS in **alert-only mode**. Collecting edge data into
`logs/weather_edges.csv`. After 5-7 days of data, run `cargo run --bin check_edges`
to evaluate calibration before enabling live trading.
