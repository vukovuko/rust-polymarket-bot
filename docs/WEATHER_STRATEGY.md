# Weather Strategy — How It Works

## The Opportunity

Polymarket lists daily high temperature markets for 14 cities worldwide. Each market has
~9 buckets (e.g., "31F or below", "32-33F", "34-35F", ..., "46F or higher"). Each bucket
is a binary market: YES pays $1 if the actual temp falls in that range, NO pays $1 otherwise.

The market prices reflect crowd estimates of probability. If the crowd says 15% but our
forecast says 35%, that's a 20% edge. We buy YES at $0.15 and expect $0.35 in return on
average.

## Scan Cycle

WeatherStrategy runs a scan loop:
- **Normal interval**: Every 30 minutes (`WEATHER_SCAN_INTERVAL`)
- **Fast interval**: Every 5 minutes during GFS data windows (03:30, 09:30, 15:30, 21:30 UTC)
- GFS updates every 6 hours; fresh data means potentially new edges

## Edge Detection Flow

```
For each of 14 cities:
  For each date (today + 2 days):
    1. Fetch ensemble forecasts (GFS/ECMWF/ICON) from Open-Meteo
    2. For each bucket in that city-date market:
       a. Compute Gaussian CDF probability (see PROBABILITY_ENGINE.md)
       b. Get market price (WS real-time or Gamma API fallback)
       c. edge = gaussian_prob - market_price
       d. If edge > EDGE_THRESHOLD (10%) AND gaussian_prob > MIN_PROBABILITY (30%)
          AND market_price < MAX_ENTRY_PRICE (65c):
            -> Log to CSV
            -> Calculate Kelly bet size
            -> Send Telegram alert
```

## Kelly Position Sizing

Uses quarter Kelly by default (conservative):

```
edge = gaussian_prob - market_price
odds = 1 / market_price - 1
kelly_fraction_full = edge / (1 - market_price)
bet = kelly_fraction_full * KELLY_FRACTION * BANKROLL
bet = min(bet, MAX_WEATHER_POSITION)
```

Example: prob=0.40, price=0.20, bankroll=$77, kelly_fraction=0.25
- edge = 0.20, kelly_full = 0.20/0.80 = 0.25
- bet = 0.25 * 0.25 * 77 = $4.81

## Price Sources

Two sources, WS preferred:
1. **WebSocket (real-time)**: PolyWs subscribes to weather tokens, updates shared HashMap
2. **Gamma API (cached)**: Market prices fetched during `refresh_weather()`, can be minutes stale

Alerts note the source: `[WS]` or `[GAMMA]`. Set `REQUIRE_WS_PRICE=true` to skip stale prices
(not recommended — first scan runs before WS has data for all tokens).

## Deduplication

HashSet of (city, date, bucket) cleared at midnight UTC. Each bucket gets one alert per day.
This prevents spam but means improved edges won't re-alert. Future improvement: re-alert if
edge increases by >5%.

## Dynamic WS Subscription

Weather tokens are subscribed dynamically after each `refresh_weather()`:
1. Get all weather token IDs from cached markets
2. Diff against currently subscribed set
3. Subscribe new tokens in batches of 100
4. Prune expired tokens (past end_date)

This means the bot automatically picks up new markets as Polymarket lists them.

## CSV Output

Every edge is logged to `logs/weather_edges.csv` with columns:
```
timestamp, city, date, bucket_lower, bucket_upper, market_price, price_source,
gaussian_prob, counting_prob, ensemble_mean, ensemble_std, inflated_std,
corrected_min, corrected_max, model_breakdown, edge, kelly_bet, method
```

This CSV is the input for `check_edges` calibration tool.

## What We Don't Do (Yet)

- **NO-side edges**: Only check YES side. If a bucket is overpriced, we could short it.
- **Multi-bet Kelly**: Kelly doesn't account for correlated positions in same market.
- **Live trading**: Alert-only mode. Need EIP-712 signing + verified relayer first.
