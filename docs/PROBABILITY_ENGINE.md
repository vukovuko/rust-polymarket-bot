# Probability Engine — Gaussian CDF Method

## Why Not Simple Counting?

The naive approach: count how many ensemble members fall in each bucket.
With 121 total members (GFS 30 + ECMWF 50 + ICON 40) across 2 degree F buckets,
many valid buckets get 0/121 = 0% when the true probability is 3-5%.

Tail buckets are invisible. A bucket with 0 members but true probability of 5% at
market price $0.02 is a massive edge we'd completely miss.

## Gaussian CDF Approach

Instead of counting, fit a Gaussian (normal distribution) to each model's ensemble,
then integrate over the bucket range using the CDF.

### Per-Model Processing

For each model (GFS, ECMWF, ICON):
1. Apply bias correction to each member temperature
2. Compute mean and standard deviation of corrected temps
3. Inflate std by `STD_INFLATION` factor (default 1.3x)
4. Compute P(lower - 0.5 <= T < upper + 0.5) using normal CDF

The half-degree offsets (+-0.5) account for Weather Underground reporting integer temps.
A reported high of 34F could mean the actual was anywhere from 33.5 to 34.5.

### Weighted Average

Models are weighted by skill:
| Model | Weight | Bias (F) | Bias (C) | Members |
|-------|--------|----------|----------|---------|
| ECMWF | 0.40 | +1.0 | +0.6 | 50 |
| GFS | 0.35 | +2.0 | +1.1 | 30 |
| ICON | 0.25 | +0.0 | +0.0 | 40 |

ECMWF gets highest weight (best global skill). GFS is strong for US cities.
ICON is included for diversity but lower weight.

Final probability = weighted average of per-model probabilities.

### Bias Correction

Weather models have systematic biases. GFS runs cold by ~2F for daily highs
(NCEP studies: 1.5-1.8C cold bias). ECMWF runs cold by ~1F.

Bias is **added** to each member temperature before computing mean/std.
Positive bias = model runs cold, we warm it up.

Enable/disable via `APPLY_BIAS_CORRECTION` env var (for A/B testing).

### Std Inflation

Ensemble forecasts are systematically underdispersive — they underestimate
uncertainty. Multiplying std by 1.3x corrects for this.

Without inflation, the Gaussian is too peaked and assigns too-low probability
to tails. This leads to:
- Missed edges in tail buckets
- Overconfident probabilities in central buckets

### Std Floor

Very narrow ensembles (all members agree) would produce near-zero std, making
the Gaussian degenerate. We enforce a minimum:

- Bounded buckets: floor = `effective_width * 0.75`, capped at `2 * tail_floor`
- Tail buckets (unbounded): floor = 1.5F / 0.8C

The cap prevents absurd floors on wide buckets (e.g., 12F-wide bucket would
get 9F floor without the cap).

### Normal CDF Implementation

Uses Abramowitz & Stegun 26.2.17 polynomial approximation. Max error ~1.5e-7.
No external dependencies needed.

```
P(a <= T < b) = normal_cdf((b - mean) / std) - normal_cdf((a - mean) / std)
```

For unbounded tails:
- "31F or below": P(T < 31.5) = normal_cdf((31.5 - mean) / std)
- "46F or higher": P(T >= 45.5) = 1 - normal_cdf((45.5 - mean) / std)

## Comparison: Gaussian vs Counting

Both methods are computed and logged to CSV. The counting method serves as a sanity
check and baseline for calibration. If Gaussian consistently beats counting in
`check_edges` Brier scores, the approach is validated.

## Known Limitations

1. **Gaussian assumption**: Temperature distributions can be skewed, especially in
   transitional weather. A mixture model or skew-normal would be more accurate.
2. **Static bias values**: Bias varies by season, location, and forecast lead time.
   Ideally calibrated dynamically from check_edges results.
3. **Equal-member models**: Within a model, all members get equal weight. Some models
   use control vs perturbed members with different skill levels.
4. **Missing models**: Could add ECMWF AIFS (AI-based, 50 members, operational since
   July 2025) as a 4th model for better diversity.
