# External API Reference

## Open-Meteo (Weather Forecasts)

### What we use it for
Ensemble weather forecasts from 3 models: GFS, ECMWF, ICON.

### Endpoint
```
https://ensemble-api.open-meteo.com/v1/ensemble
```

### Parameters we send
```
?latitude={lat}
&longitude={lon}
&daily=temperature_2m_max
&models={model}
&timezone={iana_timezone}    <- CRITICAL for correct local-day aggregation
&forecast_days=3
```

### Models
| API name | Model | Members | Notes |
|----------|-------|---------|-------|
| `gfs_seamless` | GFS | 30 | Best for US cities |
| `ecmwf_ifs025` | ECMWF IFS | 50 | Best global skill |
| `icon_seamless` | ICON | 40 | Good diversity source |

### Response format
```json
{
  "daily": {
    "time": ["2026-03-02", "2026-03-03", "2026-03-04"],
    "temperature_2m_max_member0": [45.1, 48.3, 50.2],
    "temperature_2m_max_member1": [44.8, 47.9, 49.8],
    ...
  }
}
```

### Rate limits
- Free tier: 10,000 req/day, 5,000/hour, 600/min
- Our usage: ~9,240 req/day (tight — 14 cities * 3 models * 48 scans * ~4.6 calls)
- Commercial: $19/mo, higher limits
- We add 150ms delay between requests to stay under per-minute limits

### Important: timezone parameter
Without `&timezone=America/New_York`, daily max is computed over UTC midnight-to-midnight.
For Seoul (UTC+9), this means the "daily high" mixes two calendar days.
Verified up to 3F difference with vs without timezone parameter.

---

## Polymarket CLOB REST API

### Base URL
```
https://clob.polymarket.com
```

### Endpoints we use
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/markets` | Paginated market list (BTC 5-min discovery fallback) |
| GET | `/book?token_id=X` | Order book for a token |
| GET | `/fee-rate?tokenID=X` | Fee rate in bps for order signing |
| POST | `/order` | Place signed order (not yet implemented) |
| DELETE | `/order/{id}` | Cancel order (not yet implemented) |
| DELETE | `/orders` | Cancel all orders (kill switch) |

### Rate limits
- General: 9,000 req / 10 seconds
- `/book`: 1,500 req / 10 seconds
- `POST /order`: 3,500 req / 10 seconds burst
- Our usage: negligible (startup scan + 300s backup refresh)

---

## Polymarket Gamma API

### Base URL
```
https://gamma-api.polymarket.com
```

### What we use it for
Market discovery by slug (deterministic, no pagination needed).

### Endpoints
| Method | Path | Purpose |
|--------|------|---------|
| GET | `/events?slug=X` | Fetch event by slug (weather markets) |
| GET | `/markets?slug=X` | Fetch market by slug (BTC 5-min) |

### Rate limits
- 9,000 req / 10 seconds
- We add 150ms between calls to be safe

---

## Polymarket WebSocket

### URL
```
wss://ws-subscriptions-clob.polymarket.com/ws/market
```

### Channels we subscribe to
1. **best_bid_ask**: Real-time best bid/ask for subscribed tokens
2. **new_markets**: New market listings (instant discovery)

### Subscription
Via SDK: `subscribe_best_bid_ask(Vec<U256>)` in batches of 100 tokens.
Currently subscribing to ~780 tokens (BTC 5-min + weather).

### Event format (BestBidAsk)
```
market: B256, asset_id: U256, best_bid: Decimal,
best_ask: Decimal, spread: Decimal, timestamp: i64
```

### Limits
- No official rate limits published
- Community reports 200-500 subscriptions per call is safe
- Broadcast buffer: 1024 messages — if processing lags, stream dies with `Lagged`
- Our throughput: ~60-75 events/second (well within capacity)

---

## Polymarket Relayer (Order Execution)

### Transaction limits
| Tier | Limit | How to get |
|------|-------|------------|
| Unverified | 100 tx/day | Default |
| Verified | 3,000 tx/day | Email builder@polymarket.com |
| Partner | Unlimited | Partnership agreement |

We need Verified tier before going live with weather trading.

---

## Weather Company API (Resolution Verification)

### What we use it for
`check_edges` fetches actual temps from the same source Polymarket resolves against.

### Note
This is Weather Underground's backend. Polymarket resolves using WU hourly observations.
The API key used is a public one embedded in check_edges.

### Rate limiting
We add 300ms between requests to avoid throttling.

---

## Binance WebSocket (BTC Price)

### URL
```
wss://stream.binance.com:9443/ws/btcusdt@trade
```

### Purpose
Real-time BTC/USDT trade prices for the BTC 5-minute market arb scanner.

### Format
```json
{"p": "67234.50", "q": "0.001", "T": 1709337600000}
```

We maintain a 5-second rolling VecDeque of prices for momentum detection
(momentum strategy removed, but price feed still used for settlement logging).
