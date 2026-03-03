use std::collections::HashMap;
use std::io::Write;

use anyhow::{Context, Result};
use chrono::NaiveDate;

/// City configuration — mirrors WEATHER_CITIES from market_finder.rs.
/// ICAO station codes and country codes match Polymarket's resolution source
/// (Weather Underground / Weather Company API).
struct City {
    slug: &'static str,
    name: &'static str,
    /// ICAO airport station code used by Weather Underground for resolution.
    icao: &'static str,
    /// ISO 2-letter country code for the Weather Company API location format.
    country_code: &'static str,
    fahrenheit: bool,
}

const CITIES: &[City] = &[
    City {
        slug: "nyc",
        name: "NYC",
        icao: "KLGA",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "chicago",
        name: "Chicago",
        icao: "KORD",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "miami",
        name: "Miami",
        icao: "KMIA",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "atlanta",
        name: "Atlanta",
        icao: "KATL",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "dallas",
        name: "Dallas",
        icao: "KDAL",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "seattle",
        name: "Seattle",
        icao: "KSEA",
        country_code: "US",
        fahrenheit: true,
    },
    City {
        slug: "london",
        name: "London",
        icao: "EGLC",
        country_code: "GB",
        fahrenheit: false,
    },
    City {
        slug: "paris",
        name: "Paris",
        icao: "LFPG",
        country_code: "FR",
        fahrenheit: false,
    },
    City {
        slug: "seoul",
        name: "Seoul",
        icao: "RKSI",
        country_code: "KR",
        fahrenheit: false,
    },
    City {
        slug: "toronto",
        name: "Toronto",
        icao: "CYYZ",
        country_code: "CA",
        fahrenheit: false,
    },
    City {
        slug: "ankara",
        name: "Ankara",
        icao: "LTAC",
        country_code: "TR",
        fahrenheit: false,
    },
    City {
        slug: "buenos-aires",
        name: "Buenos Aires",
        icao: "SAEZ",
        country_code: "AR",
        fahrenheit: false,
    },
    City {
        slug: "wellington",
        name: "Wellington",
        icao: "NZWN",
        country_code: "NZ",
        fahrenheit: false,
    },
    City {
        slug: "sao-paulo",
        name: "São Paulo",
        icao: "SBGR",
        country_code: "BR",
        fahrenheit: false,
    },
];

fn city_by_slug(slug: &str) -> Option<&'static City> {
    CITIES.iter().find(|c| c.slug == slug)
}

/// A row from weather_edges.csv.
#[derive(Debug)]
#[allow(dead_code)]
struct EdgeRow {
    timestamp: String,
    city: String,
    date: NaiveDate,
    bucket_label: String,
    bucket_lower: f64,
    bucket_upper: f64,
    fahrenheit: bool,
    forecast_prob: f64,
    market_price: f64,
    price_source: String,
    edge: f64,
    model_breakdown: String,
    condition_id: String,
    yes_token_id: String,
    // New Gaussian columns (optional — may be absent in old-format rows)
    gaussian_prob: Option<f64>,
    counting_prob: Option<f64>,
    ensemble_mean: Option<f64>,
    ensemble_std: Option<f64>,
    inflated_std: Option<f64>,
}

/// Result of checking one edge against actual temperature.
#[derive(Debug)]
struct CheckedEdge {
    row: EdgeRow,
    actual_temp_raw: f64,
    #[allow(dead_code)]
    actual_temp_rounded: f64,
    bucket_hit: bool,
}

fn parse_bound(s: &str) -> f64 {
    match s.trim() {
        "-inf" => f64::NEG_INFINITY,
        "inf" => f64::INFINITY,
        v => v.parse().unwrap_or(0.0),
    }
}

fn parse_edge_row(record: &[String]) -> Option<EdgeRow> {
    if record.len() < 14 {
        return None;
    }
    let date = NaiveDate::parse_from_str(&record[2], "%Y-%m-%d").ok()?;

    // Parse optional Gaussian columns (indices 14-18, new format)
    let gaussian_prob = record.get(14).and_then(|s| s.parse().ok());
    let counting_prob = record.get(15).and_then(|s| s.parse().ok());
    let ensemble_mean = record.get(16).and_then(|s| s.parse().ok());
    let ensemble_std = record.get(17).and_then(|s| s.parse().ok());
    let inflated_std = record.get(18).and_then(|s| s.parse().ok());

    Some(EdgeRow {
        timestamp: record[0].clone(),
        city: record[1].clone(),
        date,
        bucket_label: record[3].clone(),
        bucket_lower: parse_bound(&record[4]),
        bucket_upper: parse_bound(&record[5]),
        fahrenheit: record[6].trim() == "true",
        forecast_prob: record[7].parse().unwrap_or(0.0),
        market_price: record[8].parse().unwrap_or(0.0),
        price_source: record[9].clone(),
        edge: record[10].parse().unwrap_or(0.0),
        model_breakdown: record[11].clone(),
        condition_id: record[12].clone(),
        yes_token_id: record[13].clone(),
        gaussian_prob,
        counting_prob,
        ensemble_mean,
        ensemble_std,
        inflated_std,
    })
}

/// Check if an actual temperature falls within a bucket.
/// Weather Underground reports integer degrees, so resolution is exact:
/// - "≤31°F" → actual_int <= 31
/// - "32-33°F" → actual_int is 32 or 33
/// - "≥46°F" → actual_int >= 46
/// We use half-degree offsets so a continuous comparison works on the integer value.
fn temp_in_bucket(actual: f64, lower: f64, upper: f64) -> bool {
    if lower == f64::NEG_INFINITY {
        actual < upper + 0.5
    } else if upper == f64::INFINITY {
        actual >= lower - 0.5
    } else {
        actual >= lower - 0.5 && actual < upper + 0.5
    }
}

/// Weather Company API key (public, embedded in Weather Underground's frontend).
const WU_API_KEY: &str = "e1f10a1e78da46f5b10a1e78da96f525";

/// Fetch actual high temperature from Weather Underground (Weather Company API).
/// This is the EXACT same data source Polymarket uses for market resolution.
/// Returns the max integer temperature observed at the station on the given date.
async fn fetch_actual_temp(client: &reqwest::Client, city: &City, date: NaiveDate) -> Result<f64> {
    let date_str = date.format("%Y%m%d");
    let units = if city.fahrenheit { "e" } else { "m" };

    let url = format!(
        "https://api.weather.com/v1/location/{}:9:{}/observations/historical.json\
         ?apiKey={WU_API_KEY}&units={units}&startDate={date_str}&endDate={date_str}",
        city.icao, city.country_code,
    );

    let resp: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .context("Weather Company API request failed")?
        .json()
        .await
        .context("Weather Company API JSON parse failed")?;

    let observations = resp
        .get("observations")
        .and_then(|v| v.as_array())
        .context("No 'observations' array in Weather Company response")?;

    if observations.is_empty() {
        anyhow::bail!(
            "No observations for {} on {} (data may not be finalized yet)",
            city.icao,
            date,
        );
    }

    // Find max temperature across all observations for the day.
    // WU reports integer temps; the max is the resolution value.
    let max_temp = observations
        .iter()
        .filter_map(|obs| obs.get("temp").and_then(|t| t.as_f64()))
        .fold(f64::NEG_INFINITY, f64::max);

    if max_temp == f64::NEG_INFINITY {
        anyhow::bail!("No valid temp readings for {} on {}", city.icao, date);
    }

    Ok(max_temp)
}

#[tokio::main]
async fn main() -> Result<()> {
    let csv_path = "logs/weather_edges.csv";
    let content = std::fs::read_to_string(csv_path).context(format!(
        "Could not read {csv_path}. Is the bot running and finding edges?"
    ))?;

    let mut lines = content.lines();
    let header = lines.next().context("CSV is empty")?;

    // Verify header looks right
    if !header.starts_with("timestamp,") {
        anyhow::bail!("Unexpected CSV header: {header}");
    }

    // Parse all rows
    let mut rows: Vec<EdgeRow> = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<String> = line.split(',').map(|s| s.to_string()).collect();
        if let Some(row) = parse_edge_row(&fields) {
            rows.push(row);
        }
    }

    if rows.is_empty() {
        println!("No edge rows found in {csv_path}.");
        return Ok(());
    }

    let today = chrono::Utc::now().date_naive();

    // Split into resolved (date < today) and pending
    let (resolved_rows, pending_rows): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|r| r.date < today);

    println!("Weather Edge Scorecard");
    println!("======================");

    let all_dates: Vec<NaiveDate> = resolved_rows.iter().map(|r| r.date).collect();
    let min_date = all_dates.iter().min();
    let max_date = all_dates.iter().max();
    if let (Some(min), Some(max)) = (min_date, max_date) {
        println!("Period: {} to {}", min, max);
    }
    println!(
        "Total edges logged: {}",
        resolved_rows.len() + pending_rows.len()
    );
    println!("Resolved (date passed): {}", resolved_rows.len());
    println!("Pending (not yet resolved): {}", pending_rows.len());
    println!();

    if resolved_rows.is_empty() {
        println!("No resolved edges to check yet. Run again tomorrow.");
        return Ok(());
    }

    // Fetch actual temperatures (dedupe by city+date)
    let client = reqwest::Client::new();
    let mut actual_temps: HashMap<(String, NaiveDate), f64> = HashMap::new();
    let mut fetch_keys: Vec<(String, NaiveDate)> = resolved_rows
        .iter()
        .map(|r| (r.city.clone(), r.date))
        .collect();
    fetch_keys.sort();
    fetch_keys.dedup();

    println!(
        "Fetching actual temperatures from Weather Underground for {} city-dates...",
        fetch_keys.len()
    );

    for (i, (city_slug, date)) in fetch_keys.iter().enumerate() {
        let city = match city_by_slug(city_slug) {
            Some(c) => c,
            None => {
                eprintln!("  Unknown city slug: {city_slug}, skipping");
                continue;
            }
        };

        // Rate limit: 300ms between requests
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        match fetch_actual_temp(&client, city, *date).await {
            Ok(temp) => {
                let unit = if city.fahrenheit { "F" } else { "C" };
                println!(
                    "  {} {} [{}] — actual high: {:.0}°{unit}",
                    city.name, date, city.icao, temp
                );
                actual_temps.insert((city_slug.clone(), *date), temp);
            }
            Err(e) => {
                eprintln!(
                    "  {} {} [{}] — failed to fetch: {e}",
                    city.name, date, city.icao
                );
            }
        }
    }
    println!();

    // Check each resolved edge
    let mut checked: Vec<CheckedEdge> = Vec::new();
    let mut skipped = 0u32;

    for row in resolved_rows {
        let actual = match actual_temps.get(&(row.city.clone(), row.date)) {
            Some(&t) => t,
            None => {
                skipped += 1;
                continue;
            }
        };

        let rounded = actual.round();
        let hit = temp_in_bucket(actual, row.bucket_lower, row.bucket_upper);

        checked.push(CheckedEdge {
            row,
            actual_temp_raw: actual,
            actual_temp_rounded: rounded,
            bucket_hit: hit,
        });
    }

    if skipped > 0 {
        println!("({skipped} edges skipped — could not fetch actual temp)");
        println!();
    }

    let total = checked.len();
    let hits: Vec<&CheckedEdge> = checked.iter().filter(|e| e.bucket_hit).collect();
    let misses: Vec<&CheckedEdge> = checked.iter().filter(|e| !e.bucket_hit).collect();
    let hit_count = hits.len();

    let hit_rate = if total > 0 {
        hit_count as f64 / total as f64 * 100.0
    } else {
        0.0
    };

    let avg_forecast_hits = if !hits.is_empty() {
        hits.iter().map(|e| e.row.forecast_prob).sum::<f64>() / hits.len() as f64 * 100.0
    } else {
        0.0
    };

    let avg_forecast_misses = if !misses.is_empty() {
        misses.iter().map(|e| e.row.forecast_prob).sum::<f64>() / misses.len() as f64 * 100.0
    } else {
        0.0
    };

    let avg_market_price = checked.iter().map(|e| e.row.market_price).sum::<f64>() / total as f64;
    let avg_edge = checked.iter().map(|e| e.row.edge).sum::<f64>() / total as f64 * 100.0;

    println!("Results:");
    println!(
        "  Bucket hit (we'd have won):  {} / {} ({:.1}%)",
        hit_count, total, hit_rate
    );
    println!("  Avg forecast prob on hits:   {:.1}%", avg_forecast_hits);
    println!("  Avg forecast prob on misses: {:.1}%", avg_forecast_misses);
    println!("  Avg market price:            ${:.3}", avg_market_price);
    println!("  Avg edge:                    {:.1}%", avg_edge);
    println!();

    // Simulated P&L ($5 per bet)
    let bet_size = 5.0_f64;
    let total_wagered = bet_size * total as f64;
    let total_returned: f64 = hits
        .iter()
        .map(|e| {
            if e.row.market_price > 0.0 {
                bet_size / e.row.market_price
            } else {
                0.0
            }
        })
        .sum();
    let net_profit = total_returned - total_wagered;

    println!("Simulated P&L (if ${:.0}/bet on each):", bet_size);
    println!("  Total wagered: ${:.2}", total_wagered);
    println!(
        "  Total returned: ${:.2}  ({} wins × $5/price per win)",
        total_returned, hit_count
    );
    println!(
        "  Net profit: {}${:.2}",
        if net_profit >= 0.0 { "+" } else { "" },
        net_profit
    );
    println!();

    // By city breakdown
    let mut city_stats: HashMap<String, (u32, u32)> = HashMap::new(); // (hits, total)
    for edge in &checked {
        let entry = city_stats.entry(edge.row.city.clone()).or_insert((0, 0));
        entry.1 += 1;
        if edge.bucket_hit {
            entry.0 += 1;
        }
    }

    let mut city_list: Vec<_> = city_stats.iter().collect();
    city_list.sort_by(|a, b| a.0.cmp(b.0));

    println!("By city:");
    for (slug, (h, t)) in &city_list {
        let name = city_by_slug(slug).map_or(slug.as_str(), |c| c.name);
        let rate = if *t > 0 {
            *h as f64 / *t as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<14} {}/{} hits ({:.1}%)",
            format!("{}:", name),
            h,
            t,
            rate
        );
    }
    println!();

    // Write detailed results CSV
    let results_path = "logs/weather_results.csv";
    let mut out = std::fs::File::create(results_path).context("Failed to create results CSV")?;
    writeln!(
        out,
        "date,city,bucket_label,forecast_prob,market_price,edge,\
         actual_temp_raw,actual_temp_rounded,bucket_hit,would_have_won_usd"
    )?;

    for edge in &checked {
        let won_usd = if edge.bucket_hit && edge.row.market_price > 0.0 {
            bet_size / edge.row.market_price - bet_size
        } else {
            -bet_size
        };
        writeln!(
            out,
            "{},{},{},{:.4},{:.4},{:.4},{:.1},{:.0},{},{}",
            edge.row.date,
            edge.row.city,
            edge.row.bucket_label,
            edge.row.forecast_prob,
            edge.row.market_price,
            edge.row.edge,
            edge.actual_temp_raw,
            edge.actual_temp_rounded,
            edge.bucket_hit,
            format!("{:.2}", won_usd),
        )?;
    }

    println!("Detailed results written to {results_path}");
    println!();

    // === Calibration Analysis ===
    // Only for rows with ensemble_mean data (new Gaussian format)
    let calibration_edges: Vec<&CheckedEdge> = checked
        .iter()
        .filter(|e| e.row.ensemble_mean.is_some())
        .collect();

    if !calibration_edges.is_empty() {
        println!(
            "Calibration Analysis (Gaussian rows: {})",
            calibration_edges.len()
        );
        println!("========================================");

        // Bias per city: average (ensemble_mean - actual)
        let mut city_bias: HashMap<String, Vec<f64>> = HashMap::new();
        for edge in &calibration_edges {
            if let Some(mean) = edge.row.ensemble_mean {
                city_bias
                    .entry(edge.row.city.clone())
                    .or_default()
                    .push(mean - edge.actual_temp_raw);
            }
        }

        let mut bias_list: Vec<_> = city_bias.iter().collect();
        bias_list.sort_by(|a, b| a.0.cmp(b.0));

        println!("\nForecast bias per city (ensemble_mean - actual):");
        for (slug, biases) in &bias_list {
            let name = city_by_slug(slug).map_or(slug.as_str(), |c| c.name);
            let n = biases.len();
            let avg_bias = biases.iter().sum::<f64>() / n as f64;
            let rmse = (biases.iter().map(|b| b * b).sum::<f64>() / n as f64).sqrt();
            let unit = city_by_slug(slug).map_or("?", |c| if c.fahrenheit { "F" } else { "C" });
            println!(
                "  {:<14} bias={:+.1}°{unit}  RMSE={:.1}°{unit}  (n={})",
                format!("{}:", name),
                avg_bias,
                rmse,
                n,
            );
        }

        // Calibration buckets: bin by forecast probability, show actual hit rate
        println!("\nCalibration (forecast prob bin → actual hit rate):");
        let bins: &[(f64, f64)] = &[
            (0.0, 0.20),
            (0.20, 0.30),
            (0.30, 0.40),
            (0.40, 0.50),
            (0.50, 0.60),
            (0.60, 0.80),
            (0.80, 1.01),
        ];

        for &(lo, hi) in bins {
            let in_bin: Vec<&&CheckedEdge> = calibration_edges
                .iter()
                .filter(|e| e.row.forecast_prob >= lo && e.row.forecast_prob < hi)
                .collect();

            if in_bin.is_empty() {
                continue;
            }

            let bin_hits = in_bin.iter().filter(|e| e.bucket_hit).count();
            let bin_total = in_bin.len();
            let actual_rate = bin_hits as f64 / bin_total as f64 * 100.0;
            let expected_mid = (lo + hi) / 2.0 * 100.0;

            let calibration = if (actual_rate - expected_mid).abs() < 10.0 {
                "OK"
            } else if actual_rate > expected_mid {
                "UNDER-confident"
            } else {
                "OVER-confident"
            };

            println!(
                "  {:.0}-{:.0}%:  {}/{} = {:.1}% actual  (expected ~{:.0}%)  [{}]",
                lo * 100.0,
                hi * 100.0,
                bin_hits,
                bin_total,
                actual_rate,
                expected_mid,
                calibration,
            );
        }

        // Gaussian vs counting comparison
        let both_probs: Vec<(f64, f64, bool)> = calibration_edges
            .iter()
            .filter_map(|e| {
                let gp = e.row.gaussian_prob?;
                let cp = e.row.counting_prob?;
                Some((gp, cp, e.bucket_hit))
            })
            .collect();

        if !both_probs.is_empty() {
            let gaussian_hits: usize = both_probs.iter().filter(|(_, _, hit)| *hit).count();
            let gaussian_brier: f64 = both_probs
                .iter()
                .map(|(gp, _, hit)| {
                    let outcome = if *hit { 1.0 } else { 0.0 };
                    (gp - outcome) * (gp - outcome)
                })
                .sum::<f64>()
                / both_probs.len() as f64;
            let counting_brier: f64 = both_probs
                .iter()
                .map(|(_, cp, hit)| {
                    let outcome = if *hit { 1.0 } else { 0.0 };
                    (cp - outcome) * (cp - outcome)
                })
                .sum::<f64>()
                / both_probs.len() as f64;

            println!(
                "\nGaussian vs Counting comparison ({} edges):",
                both_probs.len()
            );
            println!(
                "  Actual hit rate:   {:.1}%",
                gaussian_hits as f64 / both_probs.len() as f64 * 100.0
            );
            println!(
                "  Gaussian Brier:    {:.4}  (lower = better calibrated)",
                gaussian_brier
            );
            println!("  Counting Brier:    {:.4}", counting_brier);
            if gaussian_brier < counting_brier {
                println!("  → Gaussian is better calibrated");
            } else {
                println!("  → Counting is better calibrated (consider tuning std_inflation)");
            }
        }
    } else {
        println!("No Gaussian-format rows found — calibration analysis requires new CSV format.");
        println!("Restart the bot to generate rows with Gaussian probability columns.");
    }

    Ok(())
}
