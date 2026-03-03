//! Weather market test binary.
//!
//! Verifies:
//! 1. Polymarket weather market discovery via Gamma API
//! 2. Open-Meteo ensemble forecast fetching
//! 3. Probability calculation per temperature bucket
//! 4. Edge detection (forecast probability vs market price)
//!
//! Run: cargo run --example weather_test

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, Utc};
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::EventBySlugRequest;
use polymarket_client_sdk::types::Decimal;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );
    tracing_subscriber::fmt().with_env_filter(filter).init();

    println!("=== Polymarket Weather Bot — Live API Test ===\n");

    let gamma = GammaClient::default();
    let http = reqwest::Client::new();

    let cities = all_cities();

    // Test today and tomorrow
    let today = Utc::now().date_naive();
    let tomorrow = today + Duration::days(1);
    let dates = [today, tomorrow];

    // Phase 1: Market Discovery
    println!("--- Phase 1: Market Discovery via Gamma API ---\n");

    let mut found_events: Vec<FoundEvent> = Vec::new();

    for date in &dates {
        let month = month_name(date.month());
        let day = date.day();
        let year = date.year();

        println!("  Date: {month} {day}, {year}");

        for city in &cities {
            let slug = format!(
                "highest-temperature-in-{}-on-{}-{}-{}",
                city.slug, month, day, year,
            );

            let request = EventBySlugRequest::builder().slug(&slug).build();
            match gamma.event_by_slug(&request).await {
                Ok(event) => {
                    let market_count = event.markets.as_ref().map_or(0, |m| m.len());
                    println!("    ✓ {:12} — {} buckets", city.name, market_count);
                    found_events.push(FoundEvent {
                        city,
                        date: *date,
                        event,
                    });
                }
                Err(_) => {
                    println!("    ✗ {:12} — not found", city.name);
                }
            }
        }
        println!();
    }

    println!(
        "Found {} events across {} cities\n",
        found_events.len(),
        found_events
            .iter()
            .map(|e| e.city.slug)
            .collect::<std::collections::HashSet<_>>()
            .len(),
    );

    if found_events.is_empty() {
        println!("No weather markets found. Check slug format or try different dates.");
        return Ok(());
    }

    // Phase 2: Ensemble Forecast + Edge Detection
    println!("--- Phase 2: Ensemble Forecast + Edge Detection ---\n");

    let mut all_edges: Vec<EdgeOpportunity> = Vec::new();

    for fe in &found_events {
        let city = fe.city;
        let date = fe.date;
        let date_str = date.format("%Y-%m-%d").to_string();

        println!("━━━ {} — {} ━━━", city.name, date.format("%B %-d, %Y"),);

        let markets = match &fe.event.markets {
            Some(m) if !m.is_empty() => m,
            _ => {
                println!("  No markets in event\n");
                continue;
            }
        };

        // Parse temperature buckets from market questions
        let mut buckets: Vec<MarketBucket> = Vec::new();
        for m in markets {
            let question = m.question.as_deref().unwrap_or("");
            let yes_price = m
                .outcome_prices
                .as_ref()
                .and_then(|p| p.first())
                .copied()
                .unwrap_or(Decimal::ZERO);
            let yes_token = m
                .clob_token_ids
                .as_ref()
                .and_then(|ids| ids.first())
                .map(|id| format!("{id}"));

            if let Some(bucket) = parse_bucket(question) {
                buckets.push(MarketBucket {
                    label: bucket.label,
                    lower: bucket.lower,
                    upper: bucket.upper,
                    market_price: decimal_to_f64(yes_price),
                    yes_token_id: yes_token,
                });
            } else {
                println!("  ⚠ Could not parse: {question}");
            }
        }

        buckets.sort_by(|a, b| {
            a.lower
                .partial_cmp(&b.lower)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Fetch ensemble forecast
        let unit_param = if city.fahrenheit {
            "&temperature_unit=fahrenheit"
        } else {
            ""
        };
        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble\
             ?latitude={}&longitude={}\
             &daily=temperature_2m_max\
             &models=gfs_seamless\
             &forecast_days=7{}",
            city.lat, city.lon, unit_param,
        );

        let ensemble = fetch_ensemble_temps(&http, &url, &date_str).await;
        match &ensemble {
            Ok(temps) => {
                let min = temps.iter().cloned().fold(f64::INFINITY, f64::min);
                let max = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let mean = temps.iter().sum::<f64>() / temps.len() as f64;
                let unit = if city.fahrenheit { "F" } else { "C" };
                println!(
                    "  Ensemble: {} members, {:.1}–{:.1}°{unit} (mean {:.1}°{unit})",
                    temps.len(),
                    min,
                    max,
                    mean,
                );
            }
            Err(e) => {
                println!("  Ensemble: FAILED — {e}");
            }
        }

        // Compare forecast vs market
        println!(
            "\n  {:24} {:>8} {:>8} {:>8}",
            "Bucket", "Market", "Fcast", "Edge"
        );
        println!("  {:-<24} {:->8} {:->8} {:->8}", "", "", "", "");

        let mut prob_sum = 0.0;
        let mut forecast_sum = 0.0;

        for bucket in &buckets {
            let forecast_prob = ensemble
                .as_ref()
                .map(|temps| bucket_probability(temps, bucket.lower, bucket.upper))
                .unwrap_or(0.0);

            let edge = forecast_prob - bucket.market_price;
            prob_sum += bucket.market_price;
            forecast_sum += forecast_prob;

            let marker = if edge > 0.10 {
                " ◄◄ BUY"
            } else if edge > 0.05 {
                " ◄"
            } else {
                ""
            };

            println!(
                "  {:24} {:>7.1}% {:>7.1}% {:>+7.1}%{}",
                bucket.label,
                bucket.market_price * 100.0,
                forecast_prob * 100.0,
                edge * 100.0,
                marker,
            );

            if edge > 0.05 {
                all_edges.push(EdgeOpportunity {
                    city: city.name,
                    date: date.format("%b %-d").to_string(),
                    bucket: bucket.label.clone(),
                    market_price: bucket.market_price,
                    forecast_prob,
                    edge,
                    yes_token_id: bucket.yes_token_id.clone(),
                });
            }
        }

        println!(
            "  {:24} {:>7.1}% {:>7.1}%",
            "TOTAL",
            prob_sum * 100.0,
            forecast_sum * 100.0,
        );
        println!();
    }

    // Phase 3: Summary of edges
    if !all_edges.is_empty() {
        all_edges.sort_by(|a, b| {
            b.edge
                .partial_cmp(&a.edge)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        println!("━━━ TOP EDGES ━━━\n");
        println!(
            "  {:12} {:8} {:24} {:>8} {:>8} {:>8}",
            "City", "Date", "Bucket", "Market", "Fcast", "Edge"
        );
        println!(
            "  {:-<12} {:->8} {:-<24} {:->8} {:->8} {:->8}",
            "", "", "", "", "", ""
        );

        for e in all_edges.iter().take(15) {
            let marker = if e.edge > 0.10 { " ◄◄" } else { "" };
            println!(
                "  {:12} {:8} {:24} {:>7.1}% {:>7.1}% {:>+7.1}%{}",
                e.city,
                e.date,
                e.bucket,
                e.market_price * 100.0,
                e.forecast_prob * 100.0,
                e.edge * 100.0,
                marker,
            );
        }
        println!();
    } else {
        println!(
            "No edges > 5% found. Markets may be efficiently priced or ensemble data unavailable.\n"
        );
    }

    println!("=== Test complete ===");
    Ok(())
}

// --- Types ---

struct CityConfig {
    slug: &'static str,
    name: &'static str,
    lat: f64,
    lon: f64,
    fahrenheit: bool,
}

struct FoundEvent<'a> {
    city: &'a CityConfig,
    date: chrono::NaiveDate,
    event: polymarket_client_sdk::gamma::types::response::Event,
}

struct MarketBucket {
    label: String,
    lower: f64, // f64::NEG_INFINITY for "X or below"
    upper: f64, // f64::INFINITY for "X or above"
    market_price: f64,
    yes_token_id: Option<String>,
}

#[allow(dead_code)]
struct EdgeOpportunity {
    city: &'static str,
    date: String,
    bucket: String,
    market_price: f64,
    forecast_prob: f64,
    edge: f64,
    yes_token_id: Option<String>,
}

struct ParsedBucket {
    label: String,
    lower: f64,
    upper: f64,
}

// --- City Data ---

fn all_cities() -> Vec<CityConfig> {
    vec![
        CityConfig {
            slug: "nyc",
            name: "NYC",
            lat: 40.71,
            lon: -73.94,
            fahrenheit: true,
        },
        CityConfig {
            slug: "chicago",
            name: "Chicago",
            lat: 41.88,
            lon: -87.63,
            fahrenheit: true,
        },
        CityConfig {
            slug: "miami",
            name: "Miami",
            lat: 25.76,
            lon: -80.19,
            fahrenheit: true,
        },
        CityConfig {
            slug: "atlanta",
            name: "Atlanta",
            lat: 33.75,
            lon: -84.39,
            fahrenheit: true,
        },
        CityConfig {
            slug: "dallas",
            name: "Dallas",
            lat: 32.78,
            lon: -96.80,
            fahrenheit: true,
        },
        CityConfig {
            slug: "seattle",
            name: "Seattle",
            lat: 47.61,
            lon: -122.33,
            fahrenheit: true,
        },
        CityConfig {
            slug: "london",
            name: "London",
            lat: 51.51,
            lon: -0.13,
            fahrenheit: false,
        },
        CityConfig {
            slug: "paris",
            name: "Paris",
            lat: 48.86,
            lon: 2.35,
            fahrenheit: false,
        },
        CityConfig {
            slug: "seoul",
            name: "Seoul",
            lat: 37.57,
            lon: 126.98,
            fahrenheit: false,
        },
        CityConfig {
            slug: "toronto",
            name: "Toronto",
            lat: 43.65,
            lon: -79.38,
            fahrenheit: false,
        },
        CityConfig {
            slug: "ankara",
            name: "Ankara",
            lat: 39.92,
            lon: 32.85,
            fahrenheit: false,
        },
        CityConfig {
            slug: "buenos-aires",
            name: "Buenos Aires",
            lat: -34.60,
            lon: -58.38,
            fahrenheit: false,
        },
        CityConfig {
            slug: "wellington",
            name: "Wellington",
            lat: -41.29,
            lon: 174.78,
            fahrenheit: false,
        },
        CityConfig {
            slug: "sao-paulo",
            name: "São Paulo",
            lat: -23.55,
            lon: -46.64,
            fahrenheit: false,
        },
    ]
}

// --- Parsing ---

/// Parse temperature bucket bounds from market question.
///
/// Examples:
///   "Will the highest temperature in New York City be 31°F or below on March 3?" → (NEG_INF, 31)
///   "Will the highest temperature in New York City be between 32-33°F on March 3?" → (32, 33)
///   "Will the highest temperature in New York City be 46°F or higher on March 3?" → (46, INF)
fn parse_bucket(question: &str) -> Option<ParsedBucket> {
    let q = question.to_lowercase();

    // Extract the part between "be" and "on" (or end of string)
    let be_pos = q.find(" be ")? + 4;
    let on_pos = q.rfind(" on ").unwrap_or(q.len());
    let temp_part = question[be_pos..on_pos].trim();
    let temp_lower = temp_part.to_lowercase();

    // Extract all numbers from the temp part
    let nums: Vec<f64> = extract_numbers(temp_part);

    if temp_lower.contains("or below") || temp_lower.contains("or lower") {
        let val = *nums.first()?;
        let unit = if temp_lower.contains("°f") || temp_lower.contains("f ") {
            "F"
        } else {
            "C"
        };
        Some(ParsedBucket {
            label: format!("≤{:.0}°{unit}", val),
            lower: f64::NEG_INFINITY,
            upper: val,
        })
    } else if temp_lower.contains("or above") || temp_lower.contains("or higher") {
        let val = *nums.first()?;
        let unit = if temp_lower.contains("°f") || temp_lower.contains("f ") {
            "F"
        } else {
            "C"
        };
        Some(ParsedBucket {
            label: format!("≥{:.0}°{unit}", val),
            lower: val,
            upper: f64::INFINITY,
        })
    } else if nums.len() >= 2 {
        let lo = nums[nums.len() - 2];
        let hi = nums[nums.len() - 1];
        let unit = if temp_lower.contains("°f") || temp_lower.contains("f") {
            "F"
        } else {
            "C"
        };
        Some(ParsedBucket {
            label: format!("{:.0}-{:.0}°{unit}", lo, hi),
            lower: lo,
            upper: hi,
        })
    } else if nums.len() == 1 {
        // Single exact degree (Celsius markets: "be 12°C")
        let val = nums[0];
        let unit = if temp_lower.contains("°f") || temp_lower.contains("f") {
            "F"
        } else {
            "C"
        };
        Some(ParsedBucket {
            label: format!("{:.0}°{unit}", val),
            lower: val,
            upper: val,
        })
    } else {
        None
    }
}

/// Extract all numbers from a string, including negative numbers.
///
/// A `-` is treated as a negative sign only when it starts a new number
/// (current accumulator is empty and next char is a digit).
/// In "32-33", the `-` flushes 32 first so current is empty, but it correctly
/// starts a new number -33 — however for Fahrenheit ranges we rely on having
/// two numbers, so the sign doesn't matter for range extraction.
fn extract_numbers(s: &str) -> Vec<f64> {
    let mut nums = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = s.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() || (c == '.' && !current.is_empty()) {
            current.push(c);
        } else if c == '-'
            && current.is_empty()
            && i + 1 < chars.len()
            && chars[i + 1].is_ascii_digit()
        {
            // Negative sign: only when starting a new number
            current.push(c);
        } else if !current.is_empty() {
            if let Ok(n) = current.parse::<f64>() {
                nums.push(n);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(n) = current.parse::<f64>() {
            nums.push(n);
        }
    }
    nums
}

// --- Open-Meteo Ensemble ---

/// Fetch ensemble member temperatures for a specific date.
/// Returns a Vec of max temperatures from each ensemble member.
async fn fetch_ensemble_temps(
    client: &reqwest::Client,
    url: &str,
    target_date: &str,
) -> Result<Vec<f64>> {
    let resp: serde_json::Value = client
        .get(url)
        .send()
        .await
        .context("HTTP request failed")?
        .json()
        .await
        .context("JSON parse failed")?;

    let daily = resp
        .get("daily")
        .context("No 'daily' field in Open-Meteo response")?;

    let times = daily
        .get("time")
        .and_then(|t| t.as_array())
        .context("No 'time' array")?;

    // Find the index of our target date
    let date_idx = times
        .iter()
        .position(|t| t.as_str() == Some(target_date))
        .context(format!("Date {target_date} not in response"))?;

    // Collect all member values for that date
    let mut temps = Vec::new();
    for i in 1..=60 {
        let key = format!("temperature_2m_max_member{:02}", i);
        if let Some(arr) = daily.get(&key).and_then(|v| v.as_array()) {
            if let Some(val) = arr.get(date_idx).and_then(|v| v.as_f64()) {
                temps.push(val);
            }
        }
    }

    if temps.is_empty() {
        anyhow::bail!("No ensemble member data found for {target_date}");
    }

    Ok(temps)
}

/// Calculate probability that temperature falls in a bucket.
///
/// Resolution uses integer temperatures, so:
/// - "≤31°F" resolves YES if recorded temp ≤ 31 → P(temp < 31.5)
/// - "32-33°F" resolves YES if recorded temp is 32 or 33 → P(31.5 ≤ temp < 33.5)
/// - "≥46°F" resolves YES if recorded temp ≥ 46 → P(temp ≥ 45.5)
fn bucket_probability(temps: &[f64], lower: f64, upper: f64) -> f64 {
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

    count as f64 / temps.len() as f64
}

// --- Helpers ---

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "january",
        2 => "february",
        3 => "march",
        4 => "april",
        5 => "may",
        6 => "june",
        7 => "july",
        8 => "august",
        9 => "september",
        10 => "october",
        11 => "november",
        12 => "december",
        _ => "unknown",
    }
}

fn decimal_to_f64(d: Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}
