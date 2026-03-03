# Competitive Landscape

## Who Else Trades Weather on Polymarket

### gopfan2 — The Whale
- **P&L**: $2M+ profit on weather markets alone
- **Strategy**: Price rules + forecast integration, suspected to use 10+ models
- **Bet size**: $1 per trade (high volume, low per-trade risk)
- **Presence**: Dominates liquidity on most weather markets
- **Takeaway**: Proof that weather edge trading works at scale

### Degen Doppler — Open Tool
- **Type**: Free website, not a bot (generates buy/sell signals)
- **Models**: 13 weather models
- **Method**: Normal distribution with sigma=2.0F (similar to our approach)
- **Coverage**: US cities only
- **URL**: degendoppler.com
- **Takeaway**: Our approach (Gaussian CDF) is validated by this tool's existence

### Wethr.net — Professional Tool
- **Type**: Paid subscription ($15-99/mo)
- **Models**: 16+ weather models
- **Features**: WU vs NWS resolution toggle, historical calibration
- **Coverage**: All Polymarket cities
- **Takeaway**: Professional-grade tool exists, suggesting the market is deep enough

### Climate Sight
- **Type**: Analytics platform
- **Models**: 40+ cities, 5 data sources
- **Features**: ML-based buy signals, historical accuracy tracking
- **Takeaway**: More sophisticated ML approaches exist (we're simpler but focused)

### suislanchez bot — Open Source Reference
- **Type**: Open source Python bot on GitHub
- **Models**: GFS 31-member ensemble only
- **Method**: Quarter Kelly sizing, 8% edge threshold
- **Coverage**: Limited cities
- **Takeaway**: Our approach is very similar but with 3 models vs 1,
  bias correction, and Gaussian CDF vs counting

## How We Compare

| Feature | Us | Degen Doppler | suislanchez | gopfan2 |
|---------|-----|---------------|-------------|---------|
| Models | 3 (GFS+ECMWF+ICON) | 13 | 1 (GFS) | ~10+ |
| Method | Gaussian CDF | Normal dist | Counting | Unknown |
| Bias correction | Yes (per-model) | Unknown | No | Unknown |
| Std inflation | 1.3x | ~2.0 sigma | No | Unknown |
| Kelly sizing | Quarter Kelly | N/A (signals) | Quarter Kelly | $1 flat |
| Cities | All 14 | US only | Limited | All |
| Resolution source | WU (correct) | Unknown | NWS? | WU |

## Our Advantages

1. **3-model ensemble** with proper weighting (most open-source bots use GFS only)
2. **Bias correction** per model (most bots don't correct for systematic model bias)
3. **Gaussian CDF** instead of member counting (handles tails correctly)
4. **All 14 cities** including under-served international ones
5. **Real-time WS prices** (many bots poll REST API, getting stale prices)
6. **Correct coordinates** (airport stations matching WU resolution, not city centers)
7. **Correct timezone** handling in API calls (verified critical for accuracy)

## Our Disadvantages

1. **Only 3 models** vs 13+ for Degen Doppler, 16+ for Wethr
2. **Static bias values** — competitors may calibrate dynamically
3. **No historical calibration yet** — need check_edges data first
4. **Alert-only** — not trading yet, while competitors have been live for months
5. **Free tier API** — Open-Meteo 10k/day limit is tight

## Market Dynamics

- Weather markets are **not zero-sum against the house** — they resolve to truth
- The edge comes from **better probability estimation** than the crowd
- Markets are most mispriced on: tail buckets, transitional weather days,
  international cities (less attention from US-centric traders)
- gopfan2's $2M+ proves there's real money to be made
- Market efficiency is improving — need to trade soon while edges exist
