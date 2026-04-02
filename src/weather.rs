use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// Cache TTL: 4 hours. GFS runs every 6h, ECMWF every 6-12h.
/// Between model runs the API returns identical data, so re-fetching is waste.
const CACHE_TTL: Duration = Duration::from_secs(4 * 3600);

struct CacheEntry {
    fetched_at: Instant,
    data: HashMap<NaiveDate, CityForecast>,
}

/// Fetches ensemble weather forecasts from Open-Meteo API.
pub struct WeatherFetcher {
    client: reqwest::Client,
    /// In-memory cache: key = city_slug, value = assembled multi-model forecast.
    cache: Mutex<HashMap<String, CacheEntry>>,
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

struct ModelConfig {
    name: &'static str,
    api_model: &'static str,
    /// Key suffix in the combined multi-model API response.
    response_suffix: &'static str,
    max_members: u32,
    weight: f64,
    bias_f: f64,
    bias_c: f64,
}

/// Model configuration for combined Open-Meteo ensemble API call.
/// Bias is added to raw member temps to correct systematic errors.
/// GFS runs ~2°F cold on daily highs (NCEP studies show 1.5-1.8°C cold bias).
/// ECMWF runs ~1°F cold (seasonal: ~0 in winter, ~2-3°F in summer).
const MODEL_CONFIGS: &[ModelConfig] = &[
    ModelConfig {
        name: "GFS",
        api_model: "gfs_seamless",
        response_suffix: "ncep_gefs_seamless",
        max_members: 30,
        weight: 0.25,
        bias_f: 2.0,
        bias_c: 1.1,
    },
    ModelConfig {
        name: "ECMWF",
        api_model: "ecmwf_ifs025",
        response_suffix: "ecmwf_ifs025_ensemble",
        max_members: 51,
        weight: 0.30,
        bias_f: 1.0,
        bias_c: 0.6,
    },
    ModelConfig {
        name: "ICON",
        api_model: "icon_seamless",
        response_suffix: "icon_seamless_eps",
        max_members: 40,
        weight: 0.15,
        bias_f: 0.0,
        bias_c: 0.0,
    },
    // AI-enhanced models — share initial conditions with parent models,
    // so lower weight to avoid double-counting correlated forecasts.
    ModelConfig {
        name: "AIFS",
        api_model: "ecmwf_aifs025",
        response_suffix: "ecmwf_aifs025_ensemble",
        max_members: 51,
        weight: 0.15,
        bias_f: 0.0,
        bias_c: 0.0,
    },
    ModelConfig {
        name: "AIGEFS",
        api_model: "ncep_aigefs025",
        response_suffix: "ncep_aigefs025",
        max_members: 31,
        weight: 0.10,
        bias_f: 0.0,
        bias_c: 0.0,
    },
    // Fully independent model — Canadian Meteorological Centre.
    ModelConfig {
        name: "GEM",
        api_model: "gem_global",
        response_suffix: "gem_global_ensemble",
        max_members: 21,
        weight: 0.15,
        bias_f: 0.0,
        bias_c: 0.0,
    },
];

fn model_weight(name: &str) -> f64 {
    MODEL_CONFIGS
        .iter()
        .find(|m| m.name == name)
        .map_or(0.33, |m| m.weight)
}

fn model_bias(name: &str, fahrenheit: bool) -> f64 {
    MODEL_CONFIGS
        .iter()
        .find(|m| m.name == name)
        .map_or(0.0, |m| if fahrenheit { m.bias_f } else { m.bias_c })
}

impl WeatherFetcher {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        WeatherFetcher {
            client,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// How many entries are currently cached.
    pub fn cache_size(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
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

    /// Fetch ensemble forecasts from all models in a single API call.
    /// Uses `&models=...` with all MODEL_CONFIGS — zero additional HTTP requests.
    /// The combined response uses model-suffixed member keys.
    pub async fn fetch_combined_models(
        &self,
        city_slug: &str,
        lat: f64,
        lon: f64,
        fahrenheit: bool,
        timezone: &str,
    ) -> Result<HashMap<NaiveDate, CityForecast>> {
        // Check cache first
        if let Ok(cache) = self.cache.lock() {
            if let Some(entry) = cache.get(city_slug) {
                if entry.fetched_at.elapsed() < CACHE_TTL {
                    tracing::debug!(
                        "Weather: cache hit for {city_slug} (age: {:.0}m)",
                        entry.fetched_at.elapsed().as_secs_f64() / 60.0,
                    );
                    return Ok(entry.data.clone());
                }
            }
        }

        let unit_param = if fahrenheit {
            "&temperature_unit=fahrenheit"
        } else {
            ""
        };

        let models_param = MODEL_CONFIGS
            .iter()
            .map(|m| m.api_model)
            .collect::<Vec<_>>()
            .join(",");

        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble\
             ?latitude={lat}&longitude={lon}\
             &daily=temperature_2m_max\
             &models={models_param}\
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
                "Open-Meteo returned {status} for {city_slug}: {}",
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

        // Parse per-model member data from combined response.
        // Keys are: temperature_2m_max_member{i:02}_{response_suffix}
        let mut model_data: Vec<(&ModelConfig, HashMap<NaiveDate, Vec<f64>>)> = Vec::new();

        for mc in MODEL_CONFIGS {
            let mut per_date: HashMap<NaiveDate, Vec<f64>> = HashMap::new();

            for i in 1..=(mc.max_members + 10) {
                let key = format!("temperature_2m_max_member{i:02}_{}", mc.response_suffix);
                let arr = match daily.get(&key).and_then(|v| v.as_array()) {
                    Some(a) => a,
                    None => continue,
                };
                for (date_idx, date) in dates.iter().enumerate() {
                    if let Some(val) = arr.get(date_idx).and_then(|v| v.as_f64()) {
                        per_date.entry(*date).or_default().push(val);
                    }
                }
            }

            if !per_date.is_empty() {
                tracing::debug!(
                    "Weather: {} returned {} dates for {city_slug} (combined)",
                    mc.name,
                    per_date.len(),
                );
                model_data.push((mc, per_date));
            } else {
                tracing::warn!("Weather: {} returned 0 members for {city_slug}", mc.name);
            }
        }

        if model_data.is_empty() {
            anyhow::bail!(
                "All {} weather models returned 0 members for {city_slug}",
                MODEL_CONFIGS.len()
            );
        }

        // Collect all dates across all models
        let mut all_dates: std::collections::HashSet<NaiveDate> = std::collections::HashSet::new();
        for (_, data) in &model_data {
            all_dates.extend(data.keys());
        }

        let mut forecasts = HashMap::new();

        for date in all_dates {
            let mut merged_temps = Vec::new();
            let mut breakdown = Vec::new();
            let mut per_model = Vec::new();

            for &(mc, ref data) in &model_data {
                if let Some(temps) = data.get(&date) {
                    breakdown.push((mc.name.to_string(), temps.len()));
                    merged_temps.extend(temps);
                    per_model.push(ModelForecast {
                        name: mc.name.to_string(),
                        temps: temps.clone(),
                        weight: mc.weight,
                        bias: if fahrenheit { mc.bias_f } else { mc.bias_c },
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

        // Cache the assembled forecast
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(
                city_slug.to_string(),
                CacheEntry {
                    fetched_at: Instant::now(),
                    data: forecasts.clone(),
                },
            );
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
    // Use effective_width * 0.75, capped at 2× tail_floor, but never below tail_floor.
    // Tail bucket floors: Fahrenheit 1.5°F (~0.8°C), Celsius 1.2°C (~2.2°F).
    // Previous Celsius tail_floor of 0.8 was too tight — both losing weather trades
    // were Celsius cities (Seoul, London). Increased to 1.2 for consistency with Fahrenheit.
    let tail_floor = if fahrenheit { 1.5 } else { 1.2 };
    let std_floor = if lower != f64::NEG_INFINITY && upper != f64::INFINITY {
        let effective_width = upper - lower + 1.0;
        let width_floor = (effective_width * 0.75).min(tail_floor * 2.0);
        // Never go below tail_floor — single-degree buckets shouldn't be tighter than tails
        width_floor.max(tail_floor)
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
        // Bessel's correction: divide by (n-1) for sample variance.
        // With 30 GFS members, population variance understates std by ~1.7%.
        let variance: f64 = model
            .temps
            .iter()
            .map(|&t| {
                let corrected = t + bias;
                (corrected - mean) * (corrected - mean)
            })
            .sum::<f64>()
            / if n > 1.0 { n - 1.0 } else { 1.0 };
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
