use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// Fetches ensemble weather forecasts from Open-Meteo API.
pub struct WeatherFetcher {
    client: reqwest::Client,
}

/// Per-model ensemble data, kept separate for weighted Gaussian CDF.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ModelForecast {
    pub name: String,
    pub temps: Vec<f64>,
    /// Model weight for weighted average (e.g. 0.40 for ECMWF).
    pub weight: f64,
    /// Bias correction in degrees (added to each member temp).
    /// Positive = model runs cold, we warm it up.
    pub bias: f64,
}

/// Ensemble forecast data for one city on one date.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CityForecast {
    pub city_slug: String,
    pub date: NaiveDate,
    /// Max temperature from each ensemble member (in Fahrenheit or Celsius).
    pub member_temps: Vec<f64>,
    /// Per-model member counts, e.g. [("GFS", 30), ("ECMWF", 48), ("ICON", 40)].
    pub model_breakdown: Vec<(String, usize)>,
    /// Per-model forecast data for Gaussian CDF calculation.
    pub model_forecasts: Vec<ModelForecast>,
}

impl CityForecast {
    #[allow(dead_code)]
    pub fn member_count(&self) -> usize {
        self.member_temps.len()
    }

    pub fn mean(&self) -> f64 {
        if self.member_temps.is_empty() {
            return 0.0;
        }
        self.member_temps.iter().sum::<f64>() / self.member_temps.len() as f64
    }

    pub fn min(&self) -> f64 {
        self.member_temps
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min)
    }

    pub fn max(&self) -> f64 {
        self.member_temps
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max)
    }
}

/// Model configuration: (name, weight, bias_fahrenheit, bias_celsius).
/// Bias is added to raw member temps to correct systematic errors.
/// GFS runs ~2°F cold on daily highs (NCEP studies show 1.5-1.8°C cold bias).
/// ECMWF runs ~1°F cold (seasonal: ~0 in winter, ~2-3°F in summer).
const MODEL_CONFIGS: &[(&str, f64, f64, f64)] = &[
    //  name     weight  bias_F  bias_C
    ("GFS", 0.35, 2.0, 1.1),
    ("ECMWF", 0.40, 1.0, 0.6),
    ("ICON", 0.25, 0.0, 0.0),
];

fn model_weight(name: &str) -> f64 {
    MODEL_CONFIGS
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map_or(0.33, |(_, w, _, _)| *w)
}

fn model_bias(name: &str, fahrenheit: bool) -> f64 {
    MODEL_CONFIGS
        .iter()
        .find(|(n, _, _, _)| *n == name)
        .map_or(0.0, |(_, _, bf, bc)| if fahrenheit { *bf } else { *bc })
}

impl WeatherFetcher {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        WeatherFetcher { client }
    }

    /// Fetch ensemble forecast for a city. Returns member temperatures for
    /// each date in the forecast range (up to 7 days).
    ///
    /// Uses GFS ensemble (30 members). Kept as fallback for fetch_multi_model_ensemble.
    #[allow(dead_code)]
    pub async fn fetch_ensemble(
        &self,
        city_slug: &str,
        lat: f64,
        lon: f64,
        fahrenheit: bool,
        timezone: &str,
    ) -> Result<HashMap<NaiveDate, CityForecast>> {
        let unit_param = if fahrenheit {
            "&temperature_unit=fahrenheit"
        } else {
            ""
        };

        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble\
             ?latitude={lat}&longitude={lon}\
             &daily=temperature_2m_max\
             &models=gfs_seamless\
             &forecast_days=7\
             &timezone={timezone}\
             {unit_param}",
        );

        let resp: serde_json::Value = self
            .client
            .get(&url)
            .send()
            .await
            .context("Open-Meteo HTTP request failed")?
            .json()
            .await
            .context("Open-Meteo JSON parse failed")?;

        let daily = resp
            .get("daily")
            .context("No 'daily' field in Open-Meteo response")?;

        let times = daily
            .get("time")
            .and_then(|t| t.as_array())
            .context("No 'time' array in daily")?;

        // Parse dates
        let dates: Vec<NaiveDate> = times
            .iter()
            .filter_map(|t| {
                t.as_str()
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            })
            .collect();

        // Collect ensemble member data per date
        let mut forecasts = HashMap::new();

        for (date_idx, date) in dates.iter().enumerate() {
            let mut member_temps = Vec::new();

            // GFS has members 01-30, but check up to 60 for other models
            for i in 1..=60 {
                let key = format!("temperature_2m_max_member{i:02}");
                if let Some(val) = daily
                    .get(&key)
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(date_idx))
                    .and_then(|v| v.as_f64())
                {
                    member_temps.push(val);
                }
            }

            if !member_temps.is_empty() {
                let count = member_temps.len();
                let model_forecast = ModelForecast {
                    name: "GFS".to_string(),
                    temps: member_temps.clone(),
                    weight: model_weight("GFS"),
                    bias: model_bias("GFS", fahrenheit),
                };
                forecasts.insert(
                    *date,
                    CityForecast {
                        city_slug: city_slug.to_string(),
                        date: *date,
                        member_temps,
                        model_breakdown: vec![("GFS".to_string(), count)],
                        model_forecasts: vec![model_forecast],
                    },
                );
            }
        }

        Ok(forecasts)
    }

    /// Fetch a single model's ensemble forecast.
    /// Returns date → member temperatures for that model.
    async fn fetch_single_model(
        &self,
        model: &str,
        max_members: u32,
        city_slug: &str,
        lat: f64,
        lon: f64,
        fahrenheit: bool,
        timezone: &str,
    ) -> Result<HashMap<NaiveDate, Vec<f64>>> {
        let unit_param = if fahrenheit {
            "&temperature_unit=fahrenheit"
        } else {
            ""
        };

        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble\
             ?latitude={lat}&longitude={lon}\
             &daily=temperature_2m_max\
             &models={model}\
             &forecast_days=7\
             &timezone={timezone}\
             {unit_param}",
        );

        let http_resp = self
            .client
            .get(&url)
            .send()
            .await
            .context("Open-Meteo HTTP request failed")?;

        if !http_resp.status().is_success() {
            let status = http_resp.status();
            let body = http_resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Open-Meteo returned {status} for {model}/{city_slug}: {}",
                &body[..body.len().min(200)]
            );
        }

        let resp: serde_json::Value = http_resp
            .json()
            .await
            .context("Open-Meteo JSON parse failed")?;

        let daily = resp
            .get("daily")
            .context("No 'daily' field in Open-Meteo response")?;

        let times = daily
            .get("time")
            .and_then(|t| t.as_array())
            .context("No 'time' array in daily")?;

        let dates: Vec<NaiveDate> = times
            .iter()
            .filter_map(|t| {
                t.as_str()
                    .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
            })
            .collect();

        let mut result = HashMap::new();

        for (date_idx, date) in dates.iter().enumerate() {
            let mut member_temps = Vec::new();

            for i in 1..=(max_members + 10) {
                let key = format!("temperature_2m_max_member{i:02}");
                if let Some(val) = daily
                    .get(&key)
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.get(date_idx))
                    .and_then(|v| v.as_f64())
                {
                    member_temps.push(val);
                }
            }

            if !member_temps.is_empty() {
                result.insert(*date, member_temps);
            }
        }

        tracing::debug!(
            "Weather: {model} returned {} dates for {city_slug}",
            result.len(),
        );

        Ok(result)
    }

    /// Fetch ensemble forecasts from 3 models concurrently (GFS + ECMWF + ICON).
    /// Merges all member temperatures into one vector per date.
    /// Skips any model that fails (bails only if ALL 3 fail).
    pub async fn fetch_multi_model_ensemble(
        &self,
        city_slug: &str,
        lat: f64,
        lon: f64,
        fahrenheit: bool,
        timezone: &str,
    ) -> Result<HashMap<NaiveDate, CityForecast>> {
        let models: &[(&str, &str, u32)] = &[
            ("GFS", "gfs_seamless", 30),
            ("ECMWF", "ecmwf_ifs025", 51),
            ("ICON", "icon_seamless", 40),
        ];

        let (gfs, ecmwf, icon) = tokio::join!(
            self.fetch_single_model(
                models[0].1,
                models[0].2,
                city_slug,
                lat,
                lon,
                fahrenheit,
                timezone
            ),
            self.fetch_single_model(
                models[1].1,
                models[1].2,
                city_slug,
                lat,
                lon,
                fahrenheit,
                timezone
            ),
            self.fetch_single_model(
                models[2].1,
                models[2].2,
                city_slug,
                lat,
                lon,
                fahrenheit,
                timezone
            ),
        );

        let results: Vec<(&str, HashMap<NaiveDate, Vec<f64>>)> =
            [("GFS", gfs), ("ECMWF", ecmwf), ("ICON", icon)]
                .into_iter()
                .filter_map(|(name, r)| match r {
                    Ok(data) => Some((name, data)),
                    Err(e) => {
                        tracing::warn!("Weather: {name} failed for {city_slug}: {e}");
                        None
                    }
                })
                .collect();

        if results.is_empty() {
            anyhow::bail!("All 3 weather models failed for {city_slug}");
        }

        // Collect all dates across all models
        let mut all_dates: std::collections::HashSet<NaiveDate> = std::collections::HashSet::new();
        for (_, data) in &results {
            all_dates.extend(data.keys());
        }

        let mut forecasts = HashMap::new();

        for date in all_dates {
            let mut merged_temps = Vec::new();
            let mut breakdown = Vec::new();
            let mut per_model = Vec::new();

            for &(name, ref data) in &results {
                if let Some(temps) = data.get(&date) {
                    breakdown.push((name.to_string(), temps.len()));
                    merged_temps.extend(temps);
                    per_model.push(ModelForecast {
                        name: name.to_string(),
                        temps: temps.clone(),
                        weight: model_weight(name),
                        bias: model_bias(name, fahrenheit),
                    });
                }
            }

            if !merged_temps.is_empty() {
                forecasts.insert(
                    date,
                    CityForecast {
                        city_slug: city_slug.to_string(),
                        date,
                        member_temps: merged_temps,
                        model_breakdown: breakdown,
                        model_forecasts: per_model,
                    },
                );
            }
        }

        Ok(forecasts)
    }
}

/// Result of a bucket probability calculation.
pub struct BucketProb {
    /// Number of ensemble members that fall in this bucket.
    pub count: usize,
    /// Total number of ensemble members.
    pub total: usize,
    /// Probability (count / total).
    pub prob: f64,
}

/// Calculate the probability that a temperature falls within a bucket.
/// Simple member-counting method. Kept for comparison with Gaussian CDF.
///
/// Resolution uses integer temperatures (Weather Underground reports whole degrees),
/// so bucket bounds are inclusive integers:
/// - "≤31°F" → recorded temp ≤ 31 → ensemble temp < 31.5
/// - "32-33°F" → recorded temp is 32 or 33 → 31.5 ≤ ensemble temp < 33.5
/// - "≥46°F" → recorded temp ≥ 46 → ensemble temp ≥ 45.5
pub fn bucket_probability(temps: &[f64], lower: f64, upper: f64) -> BucketProb {
    if temps.is_empty() {
        return BucketProb {
            count: 0,
            total: 0,
            prob: 0.0,
        };
    }

    let count = temps
        .iter()
        .filter(|&&t| {
            if lower == f64::NEG_INFINITY {
                t < upper + 0.5
            } else if upper == f64::INFINITY {
                t >= lower - 0.5
            } else {
                t >= lower - 0.5 && t < upper + 0.5
            }
        })
        .count();

    let total = temps.len();
    BucketProb {
        count,
        total,
        prob: count as f64 / total as f64,
    }
}

/// Standard normal CDF approximation (Abramowitz & Stegun 26.2.17).
/// Max error ~1.5e-7, no external dependencies needed.
fn normal_cdf(x: f64) -> f64 {
    if x < -8.0 {
        return 0.0;
    }
    if x > 8.0 {
        return 1.0;
    }

    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let abs_x = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * abs_x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-abs_x * abs_x).exp();

    0.5 * (1.0 + sign * y)
}

/// Result of Gaussian CDF bucket probability calculation.
pub struct GaussianBucketProb {
    /// Weighted Gaussian CDF probability.
    pub prob: f64,
    /// Weighted ensemble mean (after bias correction).
    pub ensemble_mean: f64,
    /// Weighted ensemble std (before inflation).
    pub ensemble_std: f64,
    /// Inflated std used for CDF calculation.
    pub inflated_std: f64,
    /// Simple counting probability for comparison.
    pub counting_prob: f64,
    pub counting_count: usize,
    pub counting_total: usize,
    /// Min/max of bias-corrected temps (consistent with ensemble_mean).
    pub corrected_min: f64,
    pub corrected_max: f64,
}

/// Calculate bucket probability using per-model Gaussian CDF with bias correction.
///
/// For each model:
///   1. Apply bias correction to member temps
///   2. Compute mean and std of corrected temps
///   3. Inflate std by `std_inflation` (accounts for ensemble underdispersion)
///   4. Use normal CDF to compute P(lower-0.5 ≤ T < upper+0.5)
/// Then take weighted average across models.
pub fn bucket_probability_gaussian(
    forecast: &CityForecast,
    lower: f64,
    upper: f64,
    std_inflation: f64,
    apply_bias: bool,
    fahrenheit: bool,
) -> GaussianBucketProb {
    // Compute counting prob on bias-corrected temps so the comparison is fair.
    // Apply each model's own bias to its members (not a uniform average).
    let counting = if apply_bias && !forecast.model_forecasts.is_empty() {
        let mut corrected: Vec<f64> = Vec::with_capacity(forecast.member_temps.len());
        for model in &forecast.model_forecasts {
            for &t in &model.temps {
                corrected.push(t + model.bias);
            }
        }
        bucket_probability(&corrected, lower, upper)
    } else {
        bucket_probability(&forecast.member_temps, lower, upper)
    };

    if forecast.model_forecasts.is_empty() {
        return GaussianBucketProb {
            prob: counting.prob,
            ensemble_mean: forecast.mean(),
            ensemble_std: 0.0,
            inflated_std: 0.0,
            counting_prob: counting.prob,
            counting_count: counting.count,
            counting_total: counting.total,
            corrected_min: forecast.min(),
            corrected_max: forecast.max(),
        };
    }

    // Normalize weights only across models that have data
    let total_weight: f64 = forecast
        .model_forecasts
        .iter()
        .filter(|m| !m.temps.is_empty())
        .map(|m| m.weight)
        .sum();
    if total_weight <= 0.0 {
        return GaussianBucketProb {
            prob: counting.prob,
            ensemble_mean: forecast.mean(),
            ensemble_std: 0.0,
            inflated_std: 0.0,
            counting_prob: counting.prob,
            counting_count: counting.count,
            counting_total: counting.total,
            corrected_min: forecast.min(),
            corrected_max: forecast.max(),
        };
    }

    // Compute std floor based on bucket width to avoid overconfident probabilities.
    // Effective CDF range = (upper - lower + 1) due to half-degree offsets.
    // Floor = effective_width * 0.5, so:
    //   Fahrenheit "38-39°F" (lower=38, upper=39): effective=2°F → floor=1.0°F
    //   Celsius "12°C" (lower=12, upper=12): effective=1°C → floor=0.5°C... still too tight.
    // Use effective_width * 0.75 to be more conservative:
    //   Fahrenheit: floor=1.5°F, Celsius: floor=0.75°C
    // For tail buckets (infinite bounds), use unit-aware floor.
    // Tail bucket floors (also used as cap for bounded buckets)
    let tail_floor = if fahrenheit { 1.5 } else { 0.8 };
    let std_floor = if lower != f64::NEG_INFINITY && upper != f64::INFINITY {
        let effective_width = upper - lower + 1.0;
        // Cap at 2× tail floor to prevent absurdly wide floors on wide buckets
        (effective_width * 0.75).min(tail_floor * 2.0)
    } else {
        tail_floor
    };

    let mut weighted_prob = 0.0;
    let mut weighted_mean = 0.0;
    let mut weighted_std = 0.0;
    let mut weighted_inflated_std = 0.0;
    let mut corrected_min = f64::INFINITY;
    let mut corrected_max = f64::NEG_INFINITY;

    for model in &forecast.model_forecasts {
        if model.temps.is_empty() {
            continue;
        }

        let norm_weight = model.weight / total_weight;
        let bias = if apply_bias { model.bias } else { 0.0 };

        // Compute mean, std, min, max of bias-corrected temps
        let n = model.temps.len() as f64;
        let mean: f64 = model.temps.iter().map(|&t| t + bias).sum::<f64>() / n;
        for &t in &model.temps {
            let c = t + bias;
            if c < corrected_min {
                corrected_min = c;
            }
            if c > corrected_max {
                corrected_max = c;
            }
        }
        let variance: f64 = model
            .temps
            .iter()
            .map(|&t| {
                let corrected = t + bias;
                (corrected - mean) * (corrected - mean)
            })
            .sum::<f64>()
            / n;
        let std = variance.sqrt();

        // Inflate std to account for ensemble underdispersion.
        // Floor scales with bucket width: 1.0° for 2°F or 1°C buckets.
        let inflated = (std * std_inflation).max(std_floor);

        // CDF for bucket bounds (half-degree offset for integer resolution)
        let p = if lower == f64::NEG_INFINITY {
            normal_cdf((upper + 0.5 - mean) / inflated)
        } else if upper == f64::INFINITY {
            1.0 - normal_cdf((lower - 0.5 - mean) / inflated)
        } else {
            normal_cdf((upper + 0.5 - mean) / inflated)
                - normal_cdf((lower - 0.5 - mean) / inflated)
        };

        weighted_prob += norm_weight * p;
        weighted_mean += norm_weight * mean;
        weighted_std += norm_weight * std;
        weighted_inflated_std += norm_weight * inflated;
    }

    GaussianBucketProb {
        prob: weighted_prob,
        ensemble_mean: weighted_mean,
        ensemble_std: weighted_std,
        inflated_std: weighted_inflated_std,
        counting_prob: counting.prob,
        counting_count: counting.count,
        counting_total: counting.total,
        corrected_min,
        corrected_max,
    }
}
