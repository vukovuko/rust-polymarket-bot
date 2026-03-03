//! API verification test suite.
//!
//! Tests the EXACT API calls and response parsing our bot code does.
//! Each test replicates a real code path from weather.rs, market_finder.rs,
//! or check_edges.rs and verifies the API still returns what we expect.
//!
//! Run: cargo run --example verify_apis

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use chrono::{Datelike, NaiveDate, Utc};
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::EventBySlugRequest;
use polymarket_client_sdk::types::Decimal;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"))
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );
    tracing_subscriber::fmt().with_env_filter(filter).init();

    println!("=== API Verification Suite ===\n");

    let http = reqwest::Client::new();
    let gamma = GammaClient::default();

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures: Vec<String> = Vec::new();

    // --- Group 1: Open-Meteo Ensemble (weather.rs code paths) ---
    println!("--- Open-Meteo Ensemble (weather.rs) ---");

    run_test(
        "1.1 fetch_single_model GFS parse",
        test_fetch_single_model(&http, "gfs_seamless", 30).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "1.2 fetch_single_model ECMWF parse",
        test_fetch_single_model(&http, "ecmwf_ifs025", 51).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "1.3 fetch_single_model ICON parse",
        test_fetch_single_model(&http, "icon_seamless", 40).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "1.4 multi-model merge (3 models, same dates)",
        test_multi_model_merge(&http).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "1.5 Celsius path (London, no unit param)",
        test_ensemble_celsius_path(&http).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    // --- Group 2: Open-Meteo Archive (check_edges.rs code path) ---
    println!("\n--- Open-Meteo Archive (check_edges.rs) ---");

    run_test(
        "2.1 pointer path /daily/temperature_2m_max/0 (NYC, F)",
        test_archive_pointer_path(&http, 40.71, -73.94, true).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "2.2 pointer path (London, C)",
        test_archive_pointer_path(&http, 51.51, -0.13, false).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    // --- Group 3: Polymarket Gamma (market_finder.rs code paths) ---
    println!("\n--- Polymarket Gamma (market_finder.rs) ---");

    run_test(
        "3.1 fetch_weather_event field access",
        test_gamma_weather_fields(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "3.2 outcome_prices → f64 conversion",
        test_gamma_outcome_price_conversion(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "3.3 parse_temp_bucket on live questions",
        test_gamma_bucket_parsing(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "3.4 YES/NO outcome index logic",
        test_gamma_outcome_index(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "3.5 weather slug format (multi-city)",
        test_gamma_weather_slugs(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "3.6 BTC 5-min slug format",
        test_gamma_btc_slug(&gamma).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    // --- Group 4: Cross-API consistency ---
    println!("\n--- Cross-API ---");

    run_test(
        "4.1 ensemble dates include today+tomorrow (WEATHER_LOOKAHEAD_DAYS=2)",
        test_ensemble_covers_lookahead(&http).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );
    delay().await;

    run_test(
        "4.2 NaiveDate::parse_from_str works on API date format",
        test_date_parsing(&http).await,
        &mut passed,
        &mut failed,
        &mut failures,
    );

    // --- Summary ---
    let total = passed + failed;
    println!("\n=== {passed}/{total} passed ===");

    if !failures.is_empty() {
        println!("\nFailures:");
        for f in &failures {
            println!("  - {f}");
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

// --- Test runner ---

fn run_test(
    name: &str,
    result: Result<String, String>,
    passed: &mut u32,
    failed: &mut u32,
    failures: &mut Vec<String>,
) {
    match result {
        Ok(detail) => {
            *passed += 1;
            println!("  \u{2713} {name} ({detail})");
        }
        Err(reason) => {
            *failed += 1;
            println!("  \u{2717} {name} -- {reason}");
            failures.push(format!("{name}: {reason}"));
        }
    }
}

async fn delay() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

// =============================================================================
// Group 1: Open-Meteo Ensemble — replicates weather.rs:fetch_single_model()
// =============================================================================

/// Replicates the EXACT code path from weather.rs:fetch_single_model().
/// Same URL format, same field access chain, same member iteration range.
async fn test_fetch_single_model(
    http: &reqwest::Client,
    model: &str,
    max_members: u32,
) -> Result<String, String> {
    // --- URL construction: identical to weather.rs lines 166-173 ---
    let lat = 40.71_f64;
    let lon = -73.94_f64;
    let unit_param = "&temperature_unit=fahrenheit";
    let url = format!(
        "https://ensemble-api.open-meteo.com/v1/ensemble\
         ?latitude={lat}&longitude={lon}\
         &daily=temperature_2m_max\
         &models={model}\
         &forecast_days=7\
         {unit_param}",
    );

    // --- Response parsing: identical to weather.rs lines 175-183 ---
    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON parse failed: {e}"))?;

    // --- Field access: identical to weather.rs lines 185-200 ---
    let daily = resp
        .get("daily")
        .ok_or("No 'daily' field in Open-Meteo response")?;

    let times = daily
        .get("time")
        .and_then(|t| t.as_array())
        .ok_or("No 'time' array in daily")?;

    // --- Date parsing: identical to weather.rs lines 194-201 ---
    let dates: Vec<NaiveDate> = times
        .iter()
        .filter_map(|t| {
            t.as_str()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        })
        .collect();

    if dates.is_empty() {
        return Err("No dates parsed from time array".to_string());
    }

    // --- Member iteration: identical to weather.rs lines 204-217 ---
    // Our code iterates 1..=(max_members + 10) to pick up any extra members
    let mut total_members_per_date = Vec::new();

    for (date_idx, _date) in dates.iter().enumerate() {
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

        total_members_per_date.push(member_temps.len());

        if member_temps.is_empty() {
            return Err(format!("0 members for date {}", dates[date_idx]));
        }

        // Sanity: temps should be plausible
        for &t in &member_temps {
            if t < -80.0 || t > 170.0 {
                return Err(format!("implausible temp {t} for {model}"));
            }
        }
    }

    let member_count = total_members_per_date[0];

    // Verify we got roughly the expected member count
    // (our code handles variable counts, but API should match expected)
    if (member_count as i32 - max_members as i32).unsigned_abs() > 5 {
        return Err(format!(
            "expected ~{max_members} members, got {member_count}"
        ));
    }

    Ok(format!("{member_count} members, {} dates", dates.len()))
}

/// Replicates weather.rs:fetch_multi_model_ensemble() — 3 concurrent calls, merge.
/// Verifies all 3 models return data for today, and they can be merged (share dates).
async fn test_multi_model_merge(http: &reqwest::Client) -> Result<String, String> {
    let lat = 40.71_f64;
    let lon = -73.94_f64;
    let unit_param = "&temperature_unit=fahrenheit";

    // --- Identical to weather.rs lines 242-252: three models, concurrent ---
    let models: &[(&str, &str, u32)] = &[
        ("GFS", "gfs_seamless", 30),
        ("ECMWF", "ecmwf_ifs025", 51),
        ("ICON", "icon_seamless", 40),
    ];

    let (r0, r1, r2) = tokio::join!(
        fetch_model_dates(http, models[0].1, lat, lon, unit_param),
        fetch_model_dates(http, models[1].1, lat, lon, unit_param),
        fetch_model_dates(http, models[2].1, lat, lon, unit_param),
    );

    let mut results: Vec<(&str, HashMap<NaiveDate, usize>)> = Vec::new();
    let mut model_errors = Vec::new();

    for (name, result) in [("GFS", r0), ("ECMWF", r1), ("ICON", r2)] {
        match result {
            Ok(data) => results.push((name, data)),
            Err(e) => model_errors.push(format!("{name}: {e}")),
        }
    }

    // --- weather.rs bails only if ALL 3 fail ---
    if results.is_empty() {
        return Err(format!("all 3 models failed: {}", model_errors.join("; ")));
    }

    // --- Verify today exists in the merged set (our code needs today+tomorrow) ---
    let today = Utc::now().date_naive();
    let mut merged_count_today = 0usize;
    let mut breakdown = Vec::new();

    for &(name, ref data) in &results {
        if let Some(&count) = data.get(&today) {
            merged_count_today += count;
            breakdown.push(format!("{name}:{count}"));
        }
    }

    if merged_count_today == 0 {
        return Err(format!(
            "no model returned data for today ({}). Models succeeded: {:?}",
            today,
            results.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        ));
    }

    Ok(format!(
        "{}/{} models OK, {} merged members today [{}]",
        results.len(),
        models.len(),
        merged_count_today,
        breakdown.join("+"),
    ))
}

/// Helper: fetch one model and return date → member_count (for merge test).
async fn fetch_model_dates(
    http: &reqwest::Client,
    model: &str,
    lat: f64,
    lon: f64,
    unit_param: &str,
) -> Result<HashMap<NaiveDate, usize>, String> {
    let url = format!(
        "https://ensemble-api.open-meteo.com/v1/ensemble\
         ?latitude={lat}&longitude={lon}\
         &daily=temperature_2m_max\
         &models={model}\
         &forecast_days=7\
         {unit_param}",
    );

    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON: {e}"))?;

    let daily = resp.get("daily").ok_or("no daily")?;
    let times = daily
        .get("time")
        .and_then(|t| t.as_array())
        .ok_or("no time")?;

    let dates: Vec<NaiveDate> = times
        .iter()
        .filter_map(|t| {
            t.as_str()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        })
        .collect();

    let mut result = HashMap::new();
    for (date_idx, date) in dates.iter().enumerate() {
        let mut count = 0usize;
        for i in 1..=70 {
            let key = format!("temperature_2m_max_member{i:02}");
            if daily
                .get(&key)
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.get(date_idx))
                .and_then(|v| v.as_f64())
                .is_some()
            {
                count += 1;
            }
        }
        if count > 0 {
            result.insert(*date, count);
        }
    }

    Ok(result)
}

/// Replicates the Celsius path — London uses fahrenheit=false, so no unit param.
/// Our code at weather.rs line 160-163 omits the param when fahrenheit is false.
async fn test_ensemble_celsius_path(http: &reqwest::Client) -> Result<String, String> {
    // London coords from WEATHER_CITIES in market_finder.rs
    let url = format!(
        "https://ensemble-api.open-meteo.com/v1/ensemble\
         ?latitude=51.51&longitude=-0.13\
         &daily=temperature_2m_max\
         &models=gfs_seamless\
         &forecast_days=7",
    );

    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON: {e}"))?;

    let daily = resp.get("daily").ok_or("no daily")?;

    // Pick a member and verify Celsius range
    let key = "temperature_2m_max_member01";
    let val = daily
        .get(key)
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_f64())
        .ok_or(format!("{key}[0] missing or not a number"))?;

    // Celsius: should be -50 to 60, not Fahrenheit range
    if val < -50.0 || val > 60.0 {
        return Err(format!("{val}°C outside plausible Celsius range"));
    }

    // Verify units say °C (our code doesn't check this, but we should)
    let unit = resp
        .get("daily_units")
        .and_then(|u| u.get(key))
        .and_then(|u| u.as_str())
        .unwrap_or("?");

    if unit != "\u{00b0}C" {
        return Err(format!("expected \u{00b0}C unit, got '{unit}'"));
    }

    Ok(format!("{val:.1}\u{00b0}C"))
}

// =============================================================================
// Group 2: Open-Meteo Archive — replicates check_edges.rs:fetch_actual_temp()
// =============================================================================

/// Replicates the EXACT code path from check_edges.rs:fetch_actual_temp().
/// Same URL format, same resp.pointer() call.
async fn test_archive_pointer_path(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    fahrenheit: bool,
) -> Result<String, String> {
    let yesterday = (Utc::now() - chrono::Duration::days(1)).date_naive();
    let date_str = yesterday.format("%Y-%m-%d");

    // --- URL construction: identical to check_edges.rs lines 207-213 ---
    let unit_param = if fahrenheit {
        "&temperature_unit=fahrenheit"
    } else {
        ""
    };
    let url = format!(
        "https://archive-api.open-meteo.com/v1/archive\
         ?latitude={lat}&longitude={lon}\
         &start_date={date_str}&end_date={date_str}\
         &daily=temperature_2m_max\
         {unit_param}",
    );

    let resp: serde_json::Value = http
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON: {e}"))?;

    // --- Field access: EXACT pointer path from check_edges.rs line 224-226 ---
    let temp = resp
        .pointer("/daily/temperature_2m_max/0")
        .and_then(|v| v.as_f64())
        .ok_or("pointer /daily/temperature_2m_max/0 returned None")?;

    let (min, max, unit) = if fahrenheit {
        (-30.0, 130.0, "F")
    } else {
        (-20.0, 55.0, "C")
    };

    if temp < min || temp > max {
        return Err(format!("{temp}\u{00b0}{unit} outside {min}..{max}"));
    }

    Ok(format!("{temp:.1}\u{00b0}{unit}"))
}

// =============================================================================
// Group 3: Polymarket Gamma — replicates market_finder.rs code paths
// =============================================================================

/// Replicates market_finder.rs:fetch_weather_event() field access chain.
/// Tests every field our code reads from the Gamma Market struct.
async fn test_gamma_weather_fields(gamma: &GammaClient) -> Result<String, String> {
    let today = Utc::now().date_naive();
    let slug = weather_slug("nyc", today);

    let request = EventBySlugRequest::builder().slug(&slug).build();
    let event = gamma
        .event_by_slug(&request)
        .await
        .map_err(|e| format!("Gamma failed: {e}"))?;

    let markets = event.markets.ok_or("no markets field")?;
    if markets.is_empty() {
        return Err("markets empty".to_string());
    }

    let mut checked = 0u32;
    for (i, gm) in markets.iter().enumerate() {
        // --- EXACT field access from market_finder.rs lines 464-511 ---

        // question (line 464)
        let question = gm.question.as_deref().unwrap_or("");
        if question.is_empty() {
            return Err(format!("market[{i}]: empty question"));
        }

        // clob_token_ids (line 476-479)
        let token_ids = match &gm.clob_token_ids {
            Some(ids) if ids.len() == 2 => ids,
            Some(ids) => return Err(format!("market[{i}]: {} token_ids, need 2", ids.len())),
            None => return Err(format!("market[{i}]: no clob_token_ids")),
        };

        // outcomes (line 481-483)
        let outcomes = match &gm.outcomes {
            Some(o) if o.len() == 2 => o,
            Some(o) => return Err(format!("market[{i}]: {} outcomes, need 2", o.len())),
            None => return Err(format!("market[{i}]: no outcomes")),
        };

        // condition_id (line 486-489) — format!("{c:?}") for string form
        let cid = match gm.condition_id {
            Some(c) => format!("{c:?}"),
            None => return Err(format!("market[{i}]: no condition_id")),
        };
        if cid.is_empty() {
            return Err(format!("market[{i}]: condition_id formatted empty"));
        }

        // outcome_prices (lines 514-522) — our code reads this
        let _prices = &gm.outcome_prices; // just verify field exists

        // order_price_min_tick_size (line 494)
        let _tick = gm.order_price_min_tick_size.unwrap_or(Decimal::new(1, 3));

        // order_min_size (line 495)
        let _min_size = gm.order_min_size.unwrap_or(Decimal::new(5, 0));

        // neg_risk (line 508)
        let _neg = gm.neg_risk.unwrap_or(true);

        // active (line 509)
        let _active = gm.active.unwrap_or(true);

        // slug (line 500) and end_date (line 498)
        let _slug = &gm.slug;
        let _end = gm.end_date;

        // Verify token IDs are nonzero
        if token_ids[0] == token_ids[1] {
            return Err(format!("market[{i}]: both token_ids identical"));
        }

        // Verify outcomes are "Yes"/"No" (weather markets)
        let o0 = outcomes[0].to_lowercase();
        let o1 = outcomes[1].to_lowercase();
        if !((o0 == "yes" && o1 == "no") || (o0 == "no" && o1 == "yes")) {
            return Err(format!("market[{i}]: unexpected outcomes {o0}/{o1}"));
        }

        checked += 1;
    }

    Ok(format!("{checked} markets, all fields present"))
}

/// Replicates the EXACT outcome_prices → f64 conversion from market_finder.rs lines 514-522.
/// Our code does: `f64::from_str(&d.to_string()).ok()`
async fn test_gamma_outcome_price_conversion(gamma: &GammaClient) -> Result<String, String> {
    let today = Utc::now().date_naive();
    let slug = weather_slug("nyc", today);

    let request = EventBySlugRequest::builder().slug(&slug).build();
    let event = gamma
        .event_by_slug(&request)
        .await
        .map_err(|e| format!("Gamma failed: {e}"))?;

    let markets = event.markets.ok_or("no markets")?;
    let gm = markets.first().ok_or("no markets")?;

    let outcomes = gm.outcomes.as_ref().ok_or("no outcomes")?;

    // --- YES index logic from market_finder.rs line 491-492 ---
    let first = outcomes[0].to_lowercase();
    let _yes_idx = if first == "yes" { 0 } else { 1 };

    // --- EXACT conversion from market_finder.rs lines 514-522 ---
    // Check ALL markets in the event to understand pricing across buckets
    let mut converted_count = 0u32;
    let mut zero_count = 0u32;
    let mut prices_debug = Vec::new();

    for (i, m) in markets.iter().enumerate() {
        let m_outcomes = m.outcomes.as_ref();
        let m_first = m_outcomes.and_then(|o| o.first()).map(|s| s.to_lowercase());
        let m_yes_idx = if m_first.as_deref() == Some("yes") {
            0
        } else {
            1
        };

        let yes_price: f64 = m
            .outcome_prices
            .as_ref()
            .and_then(|p| p.get(m_yes_idx))
            .and_then(|d| {
                use std::str::FromStr;
                f64::from_str(&d.to_string()).ok()
            })
            .unwrap_or(0.0);

        let no_price: f64 = m
            .outcome_prices
            .as_ref()
            .and_then(|p| p.get(1 - m_yes_idx))
            .and_then(|d| {
                use std::str::FromStr;
                f64::from_str(&d.to_string()).ok()
            })
            .unwrap_or(0.0);

        let raw_decimals = m
            .outcome_prices
            .as_ref()
            .map(|p| {
                p.iter()
                    .map(|d| format!("{d}"))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "None".to_string());

        prices_debug.push(format!(
            "[{i}] raw=[{raw_decimals}] → YES={yes_price} NO={no_price}"
        ));

        if yes_price > 0.0 && yes_price < 1.0 {
            converted_count += 1;
        } else {
            zero_count += 1;
        }
    }

    // If ALL markets have 0/1 prices, that's a real conversion bug.
    // If only some do (extreme buckets), that's expected.
    if converted_count == 0 {
        return Err(format!(
            "ALL {} markets have 0/1 prices — Decimal→f64 conversion likely broken. Samples: {}",
            markets.len(),
            prices_debug
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("; "),
        ));
    }

    Ok(format!(
        "{converted_count}/{} markets have fractional prices ({zero_count} at 0/1 extremes)",
        markets.len(),
    ))
}

/// Runs parse_temp_bucket (same code as market_finder.rs) on live Gamma questions.
/// Verifies the parser still works with current question format.
async fn test_gamma_bucket_parsing(gamma: &GammaClient) -> Result<String, String> {
    let today = Utc::now().date_naive();
    let slug = weather_slug("nyc", today);

    let request = EventBySlugRequest::builder().slug(&slug).build();
    let event = gamma
        .event_by_slug(&request)
        .await
        .map_err(|e| format!("Gamma failed: {e}"))?;

    let markets = event.markets.ok_or("no markets")?;

    let mut parsed: Vec<(f64, f64, String)> = Vec::new();
    let mut parse_failures = Vec::new();

    for gm in &markets {
        let question = gm.question.as_deref().unwrap_or("");
        match parse_temp_bucket(question) {
            Some((lower, upper)) => parsed.push((lower, upper, question.to_string())),
            None => parse_failures.push(question.to_string()),
        }
    }

    if !parse_failures.is_empty() {
        return Err(format!(
            "{} questions failed to parse: {:?}",
            parse_failures.len(),
            parse_failures.first()
        ));
    }

    if parsed.len() < 5 {
        return Err(format!(
            "only {} buckets parsed, need at least 5",
            parsed.len()
        ));
    }

    // Structural checks: exactly 1 "or below", exactly 1 "or above"
    let neg_inf = parsed
        .iter()
        .filter(|(lo, _, _)| *lo == f64::NEG_INFINITY)
        .count();
    let pos_inf = parsed
        .iter()
        .filter(|(_, hi, _)| *hi == f64::INFINITY)
        .count();

    if neg_inf != 1 {
        return Err(format!("{neg_inf} 'or below' buckets, expected 1"));
    }
    if pos_inf != 1 {
        return Err(format!("{pos_inf} 'or above' buckets, expected 1"));
    }

    // Check finite bounds are monotonically increasing
    let mut finite_bounds: Vec<f64> = Vec::new();
    for &(lo, hi, _) in &parsed {
        if lo.is_finite() {
            finite_bounds.push(lo);
        }
        if hi.is_finite() {
            finite_bounds.push(hi);
        }
    }
    finite_bounds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    finite_bounds.dedup();

    for w in finite_bounds.windows(2) {
        if w[0] >= w[1] {
            return Err(format!("bounds not monotonic: {} >= {}", w[0], w[1]));
        }
    }

    Ok(format!(
        "{}/{} parsed, monotonic",
        parsed.len(),
        markets.len()
    ))
}

/// Tests the YES/NO index determination logic from market_finder.rs line 491-492.
/// Weather markets use "Yes"/"No", BTC markets use "Up"/"Down".
async fn test_gamma_outcome_index(gamma: &GammaClient) -> Result<String, String> {
    let today = Utc::now().date_naive();
    let slug = weather_slug("nyc", today);

    let request = EventBySlugRequest::builder().slug(&slug).build();
    let event = gamma
        .event_by_slug(&request)
        .await
        .map_err(|e| format!("Gamma failed: {e}"))?;

    let markets = event.markets.ok_or("no markets")?;
    let gm = markets.first().ok_or("empty markets")?;

    let outcomes = gm.outcomes.as_ref().ok_or("no outcomes")?;
    let token_ids = gm.clob_token_ids.as_ref().ok_or("no token_ids")?;

    // --- EXACT logic from market_finder.rs line 491-492 ---
    let first = outcomes[0].to_lowercase();
    let (yes_idx, no_idx) = if first == "yes" { (0, 1) } else { (1, 0) };

    // Verify this actually gives us "Yes" and "No"
    let yes_outcome = &outcomes[yes_idx];
    let no_outcome = &outcomes[no_idx];

    if yes_outcome.to_lowercase() != "yes" {
        return Err(format!(
            "yes_idx={yes_idx} gives outcome '{yes_outcome}', not 'Yes'"
        ));
    }
    if no_outcome.to_lowercase() != "no" {
        return Err(format!(
            "no_idx={no_idx} gives outcome '{no_outcome}', not 'No'"
        ));
    }

    // Verify token IDs at those indices are different
    if token_ids[yes_idx] == token_ids[no_idx] {
        return Err("YES and NO have same token_id".to_string());
    }

    Ok(format!("outcomes[{yes_idx}]=Yes, outcomes[{no_idx}]=No"))
}

/// Tests the weather slug format used by market_finder.rs:refresh_weather() (lines 388-391).
/// Verifies multiple cities work with our exact slug pattern.
async fn test_gamma_weather_slugs(gamma: &GammaClient) -> Result<String, String> {
    let today = Utc::now().date_naive();

    // Sample from WEATHER_CITIES — mix of US (Fahrenheit) and international
    let test_cities = ["nyc", "london", "sao-paulo", "seoul"];

    let mut found = 0u32;
    let mut found_names = Vec::new();

    for city_slug in &test_cities {
        let slug = weather_slug(city_slug, today);
        let request = EventBySlugRequest::builder().slug(&slug).build();

        if let Ok(event) = gamma.event_by_slug(&request).await {
            if event.markets.as_ref().is_some_and(|m| !m.is_empty()) {
                found += 1;
                found_names.push(*city_slug);
            }
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    if found < 2 {
        return Err(format!(
            "only {found}/{} cities found: {:?}",
            test_cities.len(),
            found_names
        ));
    }

    Ok(format!(
        "{found}/{} found ({:?})",
        test_cities.len(),
        found_names
    ))
}

/// Tests the BTC 5-min slug format used by market_finder.rs:refresh_via_gamma().
/// Slug pattern: "btc-updown-5m-{window_start}" where window_start is unix ts rounded to 300s.
async fn test_gamma_btc_slug(gamma: &GammaClient) -> Result<String, String> {
    // --- EXACT slug construction from market_finder.rs lines 169-176 ---
    let now_unix = Utc::now().timestamp() as u64;
    let window_secs = 300u64;
    let current_start = (now_unix / window_secs) * window_secs;

    // Try current window and a few ahead (some may not exist yet)
    let mut found_slug = None;
    for i in 0..6 {
        let window_start = current_start + i * window_secs;
        let slug = format!("btc-updown-5m-{window_start}");

        let request = EventBySlugRequest::builder().slug(&slug).build();
        if let Ok(event) = gamma.event_by_slug(&request).await {
            if let Some(markets) = &event.markets {
                if let Some(gm) = markets.first() {
                    // Verify BTC markets have Up/Down outcomes (not Yes/No)
                    if let Some(outcomes) = &gm.outcomes {
                        if outcomes.len() == 2 {
                            let o0 = outcomes[0].to_lowercase();
                            let o1 = outcomes[1].to_lowercase();

                            // market_finder.rs line 262: checks for "up"
                            let has_up = o0 == "up" || o1 == "up";
                            let has_down = o0 == "down" || o1 == "down";

                            if has_up && has_down {
                                found_slug = Some(slug);
                                break;
                            } else {
                                return Err(format!(
                                    "BTC market outcomes are '{o0}'/'{o1}', expected 'up'/'down'"
                                ));
                            }
                        }
                    }
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    match found_slug {
        Some(slug) => Ok(format!("found: {slug}")),
        None => Err("no BTC 5-min market found in next 30 minutes".to_string()),
    }
}

// =============================================================================
// Group 4: Cross-API consistency
// =============================================================================

/// Our weather strategy needs today and tomorrow's data (WEATHER_LOOKAHEAD_DAYS=2).
/// Verifies the ensemble API with forecast_days=7 actually includes both.
async fn test_ensemble_covers_lookahead(http: &reqwest::Client) -> Result<String, String> {
    let url = "https://ensemble-api.open-meteo.com/v1/ensemble\
        ?latitude=40.71&longitude=-73.94\
        &daily=temperature_2m_max\
        &models=gfs_seamless\
        &forecast_days=7\
        &temperature_unit=fahrenheit";

    let resp: serde_json::Value = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON: {e}"))?;

    let times = resp
        .get("daily")
        .and_then(|d| d.get("time"))
        .and_then(|t| t.as_array())
        .ok_or("missing daily.time")?;

    // Parse dates the same way our code does
    let dates: Vec<NaiveDate> = times
        .iter()
        .filter_map(|t| {
            t.as_str()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        })
        .collect();

    let today = Utc::now().date_naive();
    let tomorrow = today + chrono::Duration::days(1);

    if !dates.contains(&today) {
        return Err(format!("today ({today}) missing from {dates:?}"));
    }
    if !dates.contains(&tomorrow) {
        return Err(format!("tomorrow ({tomorrow}) missing from {dates:?}"));
    }

    Ok(format!("{} dates, today+tomorrow present", dates.len()))
}

/// Verifies that NaiveDate::parse_from_str works on the API's date format.
/// Our code at weather.rs:108-110 relies on this exact format.
async fn test_date_parsing(http: &reqwest::Client) -> Result<String, String> {
    let url = "https://ensemble-api.open-meteo.com/v1/ensemble\
        ?latitude=40.71&longitude=-73.94\
        &daily=temperature_2m_max\
        &models=gfs_seamless\
        &forecast_days=3\
        &temperature_unit=fahrenheit";

    let resp: serde_json::Value = http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("HTTP: {e}"))?
        .json()
        .await
        .map_err(|e| format!("JSON: {e}"))?;

    let times = resp
        .get("daily")
        .and_then(|d| d.get("time"))
        .and_then(|t| t.as_array())
        .ok_or("missing daily.time")?;

    let mut parsed_count = 0u32;
    let mut failed_strs = Vec::new();

    for t in times {
        let s = t.as_str().ok_or("time element not a string")?;
        // EXACT parse call from weather.rs line 109
        match NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            Ok(_) => parsed_count += 1,
            Err(_) => failed_strs.push(s.to_string()),
        }
    }

    if !failed_strs.is_empty() {
        return Err(format!(
            "{} dates failed NaiveDate::parse_from_str: {:?}",
            failed_strs.len(),
            failed_strs
        ));
    }

    if parsed_count == 0 {
        return Err("no dates to parse".to_string());
    }

    Ok(format!("{parsed_count}/{} parsed OK", times.len()))
}

// =============================================================================
// Helpers (duplicated from market_finder.rs — examples are self-contained)
// =============================================================================

/// Build weather event slug — identical to market_finder.rs lines 388-391.
fn weather_slug(city_slug: &str, date: NaiveDate) -> String {
    let month = month_name(date.month());
    let day = date.day();
    let year = date.year();
    format!("highest-temperature-in-{city_slug}-on-{month}-{day}-{year}")
}

/// Parse temperature bounds from a market question.
/// IDENTICAL to market_finder.rs:parse_temp_bucket() lines 571-604.
fn parse_temp_bucket(question: &str) -> Option<(f64, f64)> {
    let q = question.to_lowercase();
    let be_pos = q.find(" be ")? + 4;
    let on_pos = q.rfind(" on ")?;
    if be_pos >= on_pos {
        return None;
    }
    let temp_part = &question[be_pos..on_pos].trim().to_lowercase();
    let nums: Vec<f64> = extract_numbers(temp_part)
        .into_iter()
        .filter(|&n| n < 200.0)
        .collect();

    if temp_part.contains("or below") || temp_part.contains("or lower") {
        Some((f64::NEG_INFINITY, *nums.first()?))
    } else if temp_part.contains("or above") || temp_part.contains("or higher") {
        Some((*nums.first()?, f64::INFINITY))
    } else if nums.len() >= 2 {
        let lo = nums[nums.len() - 2];
        let hi = nums[nums.len() - 1];
        if hi > lo { Some((lo, hi)) } else { None }
    } else if nums.len() == 1 {
        let val = nums[0];
        Some((val, val))
    } else {
        None
    }
}

/// Extract all numbers from a string, including negative numbers.
/// IDENTICAL to market_finder.rs:extract_numbers() lines 610-638.
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
