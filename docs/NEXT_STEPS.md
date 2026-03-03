# Next Steps

## Phase 1 — Calibration (Current)

**Status**: Bot running in Docker, alert-only mode, collecting edge data.

### What to do:
1. **Wait 5-7 days** for edge data to accumulate in `logs/weather_edges.csv`
2. **Run calibration**: `cargo run --bin check_edges`
3. **Analyze results** (see CALIBRATION.md for interpretation guide)
4. **Tune parameters** based on findings:
   - Adjust bias values per city if forecast bias is non-zero
   - Adjust STD_INFLATION if calibration bins are off-diagonal
   - Adjust EDGE_THRESHOLD if too many false positives or too few edges

### Open-Meteo Rate Limits
Current usage: ~9,240 requests/day vs 10,000 free tier limit.
If hitting limits, either:
- Upgrade to $19/mo commercial plan
- Reduce scan frequency (increase WEATHER_SCAN_INTERVAL)
- Reduce WEATHER_LOOKAHEAD_DAYS from 3 to 2

## Phase 2 — Live Trading Preparation

### Polymarket Verified Relayer
Unverified accounts are limited to **100 transactions/day**.
For weather trading across 14 cities, this is insufficient.

**To get verified (3,000 tx/day)**:
1. Email `builder@polymarket.com`
2. Include: API key, use case description, expected daily volume
3. Expected response time: days to a week

### EIP-712 Order Signing
Not yet implemented. The SDK handles this but needs integration:
- `src/polymarket/client.rs` needs `place_limit_buy()` method
- Orders must include `feeRateBps` from the API
- Use maker orders only (zero fees)
- Test with minimum bet size ($1) first

### Risk Manager Integration
- `src/risk.rs` exists but needs wire-up for weather trades
- Kelly sizing already computed — cap at MAX_WEATHER_POSITION
- Track weather P&L separately from arb P&L

## Phase 3 — Go Live

### Sequence:
1. Set `ALERT_ONLY=false`
2. Start with smallest bets ($1-2 per edge)
3. Monitor first 24h closely via Telegram alerts
4. Compare live fills vs alert prices (slippage check)
5. Gradually increase BANKROLL if profitable

### Safety Checks:
- Kill switch active (KILL_SWITCH_LOSS=10)
- MAX_WEATHER_POSITION caps individual trade
- MAX_DAILY_EXPOSURE caps total exposure
- MAX_ENTRY_PRICE prevents buying overpriced buckets

## Phase 4 — Improvements (After Live Validation)

### Model Improvements
- **Add ECMWF AIFS** (`ecmwf_aifs025`): AI-based ensemble, 50 members, operational since July 2025. Would be 4th model with potentially better skill.
- **Dynamic bias calibration**: Use check_edges historical data to automatically adjust bias per city per season.
- **Mixture models**: Replace single Gaussian with Gaussian mixture for transitional weather (bimodal temp distributions).

### Strategy Improvements
- **NO-side edges**: Currently only check YES. If a bucket is overpriced (market says 60%, we say 30%), shorting is profitable.
- **Multi-bet Kelly**: Account for correlated positions in same market when sizing bets.
- **Re-alerting on improved edges**: Currently one alert per bucket per day. Should re-alert if edge increases significantly.
- **Lead-time weighting**: Weight models differently based on forecast horizon (GFS better at 24h, ECMWF better at 48-72h).

### Infrastructure
- **Persistent storage**: Currently ephemeral Docker. A simple SQLite DB would enable:
  - Tracking live P&L across restarts
  - Historical edge analysis without external CSV
  - Order state recovery after crashes
- **Monitoring**: Health check endpoint, uptime alerting, Grafana dashboard
- **Multi-account**: Run separate strategies on separate wallets for isolation

## Not Worth Doing

- **More than 3 models** (diminishing returns without proper calibration of existing 3)
- **Sub-5-minute scanning** (markets don't move that fast, burns API quota)
- **Momentum strategy for BTC** (institutional bots dominate, removed from codebase)
- **Spread collector** (requires much more capital and faster execution than we have)
