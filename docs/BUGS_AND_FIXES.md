# Bugs Found and Fixed — Audit Log

Three rounds of deep audits revealed progressively subtler issues.
Listed in order of discovery, grouped by severity.

## Round 1 — Initial Implementation Bugs

### [HIGH] Counting probability was broken
**Problem**: With 121 ensemble members across 2F buckets, many valid buckets got
0/121 = 0%. Tail buckets were completely invisible.
**Fix**: Replaced with Gaussian CDF method. Counting kept as comparison metric.

### [HIGH] No per-model data
**Problem**: All 121 members were dumped into one Vec. ECMWF (50 members, higher skill)
got same weight as ICON (40 members, lower skill for US cities).
**Fix**: Added `ModelForecast` struct, per-model mean/std, weighted averaging.

### [HIGH] No bias correction
**Problem**: GFS runs ~2F cold on daily highs. We systematically over-predicted low
buckets and under-predicted high ones.
**Fix**: Added per-model bias correction constants. GFS +2.0F, ECMWF +1.0F, ICON +0.0F.

### [HIGH] No max entry price filter
**Problem**: Would buy a bucket at $0.85 if the edge was there. Terrible risk/reward —
$0.85 in for $1.00 max, with model uncertainty.
**Fix**: Added `MAX_ENTRY_PRICE=0.65` filter. Skip buckets priced above this.

## Round 2 — Subtle Logic Bugs

### [HIGH] Missing timezone in Open-Meteo API
**Problem**: Without `&timezone=<IANA>`, Open-Meteo computes daily max over UTC day,
not local calendar day. Verified up to 3F difference via actual API calls.
A "daily high for March 2 in Seoul" was actually mixing March 2 and 3 UTC hours.
**Fix**: Added IANA timezone to every WeatherCity, threaded through all API calls.

### [MEDIUM] refresh_weather replaced entire market list
**Problem**: If one API call failed during refresh, the entire market list was replaced
with the partial result. Valid cached markets were lost.
**Fix**: Changed to merge-based update: prune expired, update existing, add new.

### [MEDIUM] Scan skipped on refresh failure
**Problem**: If `refresh_weather()` failed, `continue` skipped the entire scan cycle.
But we still had valid cached data to scan against.
**Fix**: Removed `continue`. Log warning, proceed with cached data.

### [MEDIUM] action_rx None caused silent hang
**Problem**: If all strategy tasks died, `action_rx.recv()` returned None, but the
`tokio::select!` match arm didn't handle it. Bot appeared alive but did nothing.
**Fix**: Added None arm that logs error, sends Telegram alert, breaks main loop.

### [MEDIUM] std_floor scaled with bucket width
**Problem**: std_floor = `effective_width * 0.75`. For a 12F-wide bucket, floor was 9F.
This inflated tail probabilities absurdly.
**Fix**: Capped at `2 * tail_floor` (3.0F / 1.6C max).

### [MEDIUM] Raw min/max temps in alerts
**Problem**: Alerts showed `forecast.min()/max()` from raw member temps, but
probabilities used bias-corrected temps. The numbers didn't match.
**Fix**: Added `corrected_min`/`corrected_max` to GaussianBucketProb, used in alerts.

## Round 3 — Hardening

### [MEDIUM] GFS bias too low
**Problem**: Initial GFS bias was +1.5F. NCEP studies show 1.5-1.8C (2.7-3.2F) cold bias.
**Fix**: Bumped to +2.0F / +1.1C. Will calibrate further with check_edges data.

### [MEDIUM] No HTTP timeout on Open-Meteo
**Problem**: A hung API call would block the entire scan cycle indefinitely.
**Fix**: Added 15s timeout to WeatherFetcher reqwest client.

### [MEDIUM] No HTTP timeout on Telegram
**Problem**: A hung Telegram API call would block action processing.
**Fix**: Added 10s timeout to TelegramSender reqwest client.

### [LOW] env_bool didn't trim whitespace
**Problem**: `ALERT_ONLY= true` (with space) would fall through to default.
For ALERT_ONLY (default true), this is safe. For other bools, could be dangerous.
**Fix**: Added `.trim()` before `.to_lowercase()`.

### [LOW] env_bool silent on bad values
**Problem**: `ALERT_ONLY=treu` (typo) silently defaulted to false, enabling live trading.
**Fix**: Added tracing::warn with recognized values listed.

## Mistakes the AI Made

1. **Suggested flipping REQUIRE_WS_PRICE=true** after startup, ignoring that the bot
   runs in Docker with no persistent state or runtime config changes.
2. **Initial GFS bias too conservative** (1.5F vs the 2.0F that studies support).
3. **Didn't catch timezone issue** until second audit round — this was the single
   biggest accuracy bug, causing up to 3F error in daily max computation.
4. **Over-engineering suggestions**: Proposed dynamic bias calibration, mixture models,
   and other features that aren't needed until basic calibration is proven.
