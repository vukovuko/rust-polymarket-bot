# Calibration — check_edges Tool

## What It Does

`cargo run --bin check_edges` reads `logs/weather_edges.csv`, fetches actual temperatures
from the Weather Company API (same source Polymarket resolves against), and evaluates
how well our probability estimates match reality.

## How to Run

```bash
# After collecting 5-7 days of edge data:
cargo run --bin check_edges

# Or from Docker:
docker exec polymarket-bot cargo run --bin check_edges
```

Output goes to stdout + `logs/weather_results.csv`.

## What It Reports

### 1. Overall Hit Rate
What fraction of edges we flagged actually hit (actual temp in the bucket)?

Target: should be close to the average forecast probability.
If we flag edges at ~35% average prob, hit rate should be ~35%.
Higher = we're underestimating (making money). Lower = overestimating (losing).

### 2. Per-City Breakdown
Hit rate and P&L per city. Useful for finding:
- Cities where our bias correction is wrong
- Cities where WU station differs from our forecast coordinates
- Cities that are consistently profitable vs unprofitable

### 3. Simulated P&L
Assumes $5 bet per edge at the logged market price.
- Hit: return = $5 / market_price (e.g., buy at $0.20, win $1.00 = 5x return)
- Miss: lose $5
- Net P&L across all edges

### 4. Forecast Bias (Gaussian format only)
`average(ensemble_mean - actual_temp)` grouped by city.

Positive = we forecast too hot (model bias overcorrection).
Negative = we forecast too cold (need more bias correction).
Target: close to 0 for each city.

### 5. RMSE (Gaussian format only)
Root mean squared error of ensemble mean vs actual temp.
Lower is better. Compare against bucket width (2F/1C) — if RMSE > bucket width,
our forecasts are too imprecise for the resolution we're trading at.

### 6. Calibration Buckets (Gaussian format only)
Bin edges by forecast probability (30-40%, 40-50%, 50-60%, etc.).
For each bin, show actual hit rate.

**Well-calibrated**: 30-40% bin hits ~35%, 40-50% bin hits ~45%, etc.
**Overconfident**: 50-60% bin hits <45% (we're too sure).
**Underconfident**: 30-40% bin hits >45% (we're too conservative — good for profit).

### 7. Brier Score Comparison
Compares Gaussian CDF vs counting method accuracy:
```
Brier score = mean((forecast_prob - actual_outcome)^2)
```
Lower is better. If Gaussian consistently beats counting, the approach is validated.

## Data Source

Uses Weather Company API (`api.weather.com`) with a public API key.
This is the **exact same data source** Polymarket uses for resolution.
Rate limited at 300ms between requests to avoid throttling.

Cities are mapped to ICAO codes matching our WeatherCity configuration.

## Interpreting Results

### Good Signs
- Hit rate within 5% of average forecast probability
- Positive simulated P&L
- Bias near zero per city
- Calibration bins track the diagonal
- Gaussian Brier score < counting Brier score

### Bad Signs
- Hit rate >15% below average forecast probability → overconfident
- Consistent bias in one direction for a city → wrong coordinates or bias value
- One city dominating losses → investigate WU station mismatch
- RMSE > 2x bucket width → forecasts too imprecise for this resolution

### Actions Based on Results
| Finding | Action |
|---------|--------|
| GFS bias still cold for NYC | Increase GFS bias by 0.5F |
| ECMWF bias warm for London | Decrease ECMWF bias by 0.3C |
| Overconfident at 50%+ range | Increase STD_INFLATION from 1.3 to 1.5 |
| Underconfident at 30% range | Could decrease EDGE_THRESHOLD or STD_INFLATION |
| One city all misses | Check airport coordinates match WU station |
| Counting beats Gaussian | Something wrong with Gaussian — investigate |

## Minimum Data Needed

- **50+ edges**: Enough for overall hit rate to be meaningful
- **100+ edges**: Enough for per-city and calibration bin analysis
- **10+ edges per city**: Needed for per-city bias to be useful
- At current rate (~10 edges/scan, 48 scans/day), expect 50+ edges in 1-2 days
