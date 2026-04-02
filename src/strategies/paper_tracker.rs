use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;

use chrono::{NaiveDate, Utc};
use polymarket_client_sdk::types::Decimal;

/// Tracks paper trades for CSV logging, outcome verification, and daily P&L.
pub struct PaperTracker {
    inner: Mutex<Inner>,
}

struct Inner {
    arb_csv: std::fs::File,
    weather_outcome_csv: std::fs::File,
    // Daily arb stats
    daily_arb_count: u32,
    daily_arb_profit: f64,
    daily_arb_verified: u32,
    daily_arb_phantom: u32,
    daily_arb_verified_profit: f64,
    // Daily weather stats
    daily_weather_wins: u32,
    daily_weather_losses: u32,
    daily_weather_pnl: f64,
    // Current day (for reset)
    daily_date: NaiveDate,
    // Pending weather outcomes (condition_id → entry details)
    pending_weather: HashMap<String, PendingWeather>,
}

#[derive(Clone)]
struct PendingWeather {
    added_at: chrono::DateTime<Utc>,
    condition_id: String,
    city_name: String,
    city_slug: String,
    date: NaiveDate,
    bucket_label: String,
    forecast_prob: f64,
    market_price: f64,
    edge: f64,
    kelly_bet: f64,
    model_breakdown: String,
    end_date: Option<chrono::DateTime<Utc>>,
}

impl PaperTracker {
    pub fn new() -> anyhow::Result<Self> {
        std::fs::create_dir_all("logs")?;

        // Open arb CSV
        let arb_path = "logs/paper_arb_trades.csv";
        let arb_exists = std::path::Path::new(arb_path).exists();
        let mut arb_csv = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(arb_path)?;
        if !arb_exists {
            writeln!(
                arb_csv,
                "timestamp,condition_id,question,side_a_price,side_b_price,combined,shares,total_cost,expected_profit,rest_a_price,rest_b_price,depth_a,depth_b,verified"
            )?;
        }

        // Open weather outcomes CSV
        let wx_path = "logs/weather_outcomes.csv";
        let wx_exists = std::path::Path::new(wx_path).exists();
        let mut weather_outcome_csv = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(wx_path)?;
        if !wx_exists {
            writeln!(
                weather_outcome_csv,
                "timestamp,condition_id,city,date,bucket,forecast_prob,market_price,\
                 edge,kelly_bet,models,resolved_at,won,pnl"
            )?;
        }

        Ok(PaperTracker {
            inner: Mutex::new(Inner {
                arb_csv,
                weather_outcome_csv,
                daily_arb_count: 0,
                daily_arb_profit: 0.0,
                daily_arb_verified: 0,
                daily_arb_phantom: 0,
                daily_arb_verified_profit: 0.0,
                daily_weather_wins: 0,
                daily_weather_losses: 0,
                daily_weather_pnl: 0.0,
                daily_date: Utc::now().date_naive(),
                pending_weather: HashMap::new(),
            }),
        })
    }

    /// Record an arb paper trade. Writes to CSV and returns a Telegram message.
    ///
    /// `side_a_price`/`side_b_price` are the two complement token asks.
    /// We don't track which is YES/NO — arb profit is the same regardless.
    pub fn record_arb(
        &self,
        condition_id: &str,
        question: &str,
        side_a_price: Decimal,
        side_b_price: Decimal,
        shares: Decimal,
        total_cost: Decimal,
        expected_profit: Decimal,
        rest_a_price: Option<Decimal>,
        rest_b_price: Option<Decimal>,
        depth_a: Option<Decimal>,
        depth_b: Option<Decimal>,
        verified: bool,
    ) -> String {
        let now = Utc::now();
        let combined = side_a_price + side_b_price;

        if let Ok(mut inner) = self.inner.lock() {
            inner.maybe_reset_daily();

            let rest_a_str = rest_a_price.map_or("-".to_string(), |p| p.to_string());
            let rest_b_str = rest_b_price.map_or("-".to_string(), |p| p.to_string());
            let depth_a_str = depth_a.map_or("-".to_string(), |d| d.to_string());
            let depth_b_str = depth_b.map_or("-".to_string(), |d| d.to_string());

            let _ = writeln!(
                inner.arb_csv,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                now.format("%Y-%m-%dT%H:%M:%SZ"),
                condition_id,
                question.replace(',', ";"),
                side_a_price,
                side_b_price,
                combined,
                shares,
                total_cost,
                expected_profit,
                rest_a_str,
                rest_b_str,
                depth_a_str,
                depth_b_str,
                verified,
            );
            let _ = inner.arb_csv.flush();

            inner.daily_arb_count += 1;
            inner.daily_arb_profit += dec_to_f64(expected_profit);

            if verified {
                inner.daily_arb_verified += 1;
                inner.daily_arb_verified_profit += dec_to_f64(expected_profit);
            } else {
                inner.daily_arb_phantom += 1;
            }
        }

        let edge_pct = (Decimal::ONE - combined) * Decimal::ONE_HUNDRED;

        let (status_label, rest_line) = if let (Some(ra), Some(rb)) = (rest_a_price, rest_b_price) {
            let rest_combined = ra + rb;
            if verified {
                let depth_str = match (depth_a, depth_b) {
                    (Some(da), Some(db)) => format!("\nDepth: {da} / {db} shares"),
                    _ => String::new(),
                };
                (
                    "✅ Verified",
                    format!("REST: ${ra} + ${rb} = ${rest_combined} ✅{depth_str}"),
                )
            } else {
                (
                    "👻 Phantom",
                    format!("REST: ${ra} + ${rb} = ${rest_combined} ❌"),
                )
            }
        } else {
            ("⚠️ Unverified", "REST: fetch failed".to_string())
        };

        format!(
            "📝 <b>Paper Arb Trade</b> ({status_label})\n\
             {question}\n\
             WS: ${side_a_price} + ${side_b_price} = ${combined}\n\
             {rest_line}\n\
             Edge: {edge_pct:.2}% | Shares: {shares} @ ${total_cost}\n\
             Expected profit: <b>${expected_profit}</b>"
        )
    }

    /// Add a weather edge for outcome tracking.
    pub fn add_pending_weather(
        &self,
        condition_id: &str,
        city_name: &str,
        city_slug: &str,
        date: NaiveDate,
        bucket_label: &str,
        forecast_prob: f64,
        market_price: f64,
        edge: f64,
        kelly_bet: f64,
        model_breakdown: &str,
        end_date: Option<chrono::DateTime<Utc>>,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.maybe_reset_daily();

            if inner.pending_weather.contains_key(condition_id) {
                return;
            }

            inner.pending_weather.insert(
                condition_id.to_string(),
                PendingWeather {
                    added_at: Utc::now(),
                    condition_id: condition_id.to_string(),
                    city_name: city_name.to_string(),
                    city_slug: city_slug.to_string(),
                    date,
                    bucket_label: bucket_label.to_string(),
                    forecast_prob,
                    market_price,
                    edge,
                    kelly_bet,
                    model_breakdown: model_breakdown.to_string(),
                    end_date,
                },
            );
        }
    }

    /// Check for resolved weather markets via the Gamma API.
    /// Returns Telegram messages for each newly resolved outcome.
    pub async fn check_weather_outcomes(&self) -> Vec<String> {
        // 1. Get pending entries whose end_date has passed (or date is before today)
        let to_check: Vec<PendingWeather> = {
            let inner = match self.inner.lock() {
                Ok(i) => i,
                Err(_) => return Vec::new(),
            };
            let now = Utc::now();
            let today = now.date_naive();
            inner
                .pending_weather
                .values()
                .filter(|pw| pw.end_date.map_or(false, |end| end < now) || pw.date < today)
                .cloned()
                .collect()
        }; // lock released

        if to_check.is_empty() {
            // Prune stale entries that have been pending > 7 days (voided/cancelled/stuck in UMA dispute)
            let stale_cutoff = Utc::now() - chrono::Duration::days(7);
            if let Ok(mut inner) = self.inner.lock() {
                let before = inner.pending_weather.len();
                inner
                    .pending_weather
                    .retain(|_, pw| pw.added_at > stale_cutoff);
                let pruned = before - inner.pending_weather.len();
                if pruned > 0 {
                    tracing::info!(
                        "Paper tracker: pruned {pruned} stale pending weather entries (>7d old)"
                    );
                }
            }
            return Vec::new();
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let mut messages = Vec::new();
        let mut resolved: Vec<(String, bool, f64)> = Vec::new();

        for pw in &to_check {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;

            match check_resolution(&client, &pw.condition_id).await {
                Some(won) => {
                    // P&L for a maker buy at market_price:
                    // If won: payout = kelly_bet / market_price, profit = payout - kelly_bet
                    // If lost: profit = -kelly_bet
                    let pnl = if won {
                        pw.kelly_bet * (1.0 - pw.market_price) / pw.market_price
                    } else {
                        -pw.kelly_bet
                    };

                    let emoji = if won { "✅" } else { "❌" };
                    let result = if won { "WON" } else { "LOST" };
                    let pnl_str = if pnl >= 0.0 {
                        format!("+${:.2}", pnl)
                    } else {
                        format!("-${:.2}", pnl.abs())
                    };
                    let msg = format!(
                        "{emoji} <b>Weather Outcome</b>: {} {} {}\n\
                         Bucket {result} — {}\n\
                         Forecast: {:.1}% | Market: {:.1}% | Edge: +{:.1}%\n\
                         Paper bet: ${:.2} → P&L: <b>{pnl_str}</b>",
                        pw.city_name,
                        pw.date.format("%b %-d"),
                        pw.bucket_label,
                        pw.model_breakdown,
                        pw.forecast_prob * 100.0,
                        pw.market_price * 100.0,
                        pw.edge * 100.0,
                        pw.kelly_bet,
                    );

                    resolved.push((pw.condition_id.clone(), won, pnl));
                    messages.push(msg);
                }
                None => {
                    tracing::info!(
                        "Weather outcome: no resolution yet for {} {} {} (market not closed)",
                        pw.city_name,
                        pw.date,
                        pw.bucket_label,
                    );
                }
            }
        }

        // Update state with resolved outcomes
        if !resolved.is_empty() {
            if let Ok(mut inner) = self.inner.lock() {
                inner.maybe_reset_daily();
                let now = Utc::now();

                for (cid, won, pnl) in &resolved {
                    if let Some(pw) = inner.pending_weather.remove(cid) {
                        let _ = writeln!(
                            inner.weather_outcome_csv,
                            "{},{},{},{},{},{:.4},{:.4},{:.4},{:.2},{},{},{},{:.2}",
                            pw.added_at.format("%Y-%m-%dT%H:%M:%SZ"),
                            cid,
                            pw.city_slug,
                            pw.date,
                            pw.bucket_label,
                            pw.forecast_prob,
                            pw.market_price,
                            pw.edge,
                            pw.kelly_bet,
                            pw.model_breakdown.replace(',', "+"),
                            now.format("%Y-%m-%dT%H:%M:%SZ"),
                            won,
                            pnl,
                        );
                        let _ = inner.weather_outcome_csv.flush();
                    }

                    if *won {
                        inner.daily_weather_wins += 1;
                    } else {
                        inner.daily_weather_losses += 1;
                    }
                    inner.daily_weather_pnl += pnl;
                }
            }
        }

        messages
    }

    /// Format daily P&L summary for Telegram heartbeat.
    pub fn daily_summary(&self) -> String {
        let inner = match self.inner.lock() {
            Ok(i) => i,
            Err(_) => return "Paper tracker unavailable".to_string(),
        };

        let mut parts = Vec::new();

        // Arb summary
        if inner.daily_arb_count > 0 {
            let verified_pnl = if inner.daily_arb_verified_profit >= 0.0 {
                format!("+${:.2}", inner.daily_arb_verified_profit)
            } else {
                format!("-${:.2}", inner.daily_arb_verified_profit.abs())
            };
            parts.push(format!(
                "Arbs: {} trades ({} verified, {} phantom), {} verified profit",
                inner.daily_arb_count,
                inner.daily_arb_verified,
                inner.daily_arb_phantom,
                verified_pnl,
            ));
        } else {
            parts.push("Arbs: 0 trades".to_string());
        }

        // Weather summary
        let wx_resolved = inner.daily_weather_wins + inner.daily_weather_losses;
        if wx_resolved > 0 {
            let pnl_str = if inner.daily_weather_pnl >= 0.0 {
                format!("+${:.2}", inner.daily_weather_pnl)
            } else {
                format!("-${:.2}", inner.daily_weather_pnl.abs())
            };
            parts.push(format!(
                "Weather: {}W/{}L, {}, {} pending",
                inner.daily_weather_wins,
                inner.daily_weather_losses,
                pnl_str,
                inner.pending_weather.len(),
            ));
        } else {
            parts.push(format!(
                "Weather: 0 resolved, {} pending",
                inner.pending_weather.len(),
            ));
        }

        // Total
        let total = inner.daily_arb_profit + inner.daily_weather_pnl;
        let total_str = if total >= 0.0 {
            format!("+${:.2}", total)
        } else {
            format!("-${:.2}", total.abs())
        };
        parts.push(format!("Total paper P&L: {total_str}"));

        parts.join("\n")
    }
}

impl Inner {
    fn maybe_reset_daily(&mut self) {
        let today = Utc::now().date_naive();
        if today != self.daily_date {
            self.daily_arb_count = 0;
            self.daily_arb_profit = 0.0;
            self.daily_arb_verified = 0;
            self.daily_arb_phantom = 0;
            self.daily_arb_verified_profit = 0.0;
            self.daily_weather_wins = 0;
            self.daily_weather_losses = 0;
            self.daily_weather_pnl = 0.0;
            self.daily_date = today;
            // Don't clear pending_weather — they persist until resolved
        }
    }
}

/// Check if a market has resolved via the CLOB API.
/// Returns Some(true) if YES won, Some(false) if NO won, None if not yet resolved.
///
/// Uses CLOB (not Gamma) because:
/// - Gamma `/markets?condition_id=X` doesn't filter correctly and returns no tokens
/// - CLOB `/markets/{condition_id}` returns a single market with tokens[].winner
async fn check_resolution(client: &reqwest::Client, condition_id: &str) -> Option<bool> {
    let url = format!("https://clob.polymarket.com/markets/{}", condition_id,);

    let resp: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;

    // CLOB returns a single market object (not an array)
    if !resp.get("closed")?.as_bool()? {
        return None;
    }

    let tokens = resp.get("tokens")?.as_array()?;
    for token in tokens {
        let outcome = token.get("outcome")?.as_str()?.to_lowercase();
        let winner = token.get("winner")?.as_bool().unwrap_or(false);
        if outcome == "yes" && winner {
            return Some(true);
        }
        if outcome == "no" && winner {
            return Some(false);
        }
    }

    // Market closed but no winner determined yet
    None
}

fn dec_to_f64(d: Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}
