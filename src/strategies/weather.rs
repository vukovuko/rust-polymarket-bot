use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Timelike;
use polymarket_client_sdk::types::U256;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::health::BotHealth;
use crate::polymarket::market_finder::MarketFinder;
use crate::polymarket::types::WeatherMarket;
use crate::polymarket::ws::{SharedBestAsks, SharedWsClient};
use crate::weather::{self, WeatherFetcher};

use super::StrategyAction;
use super::paper_tracker::PaperTracker;

/// Max tokens per WS subscribe call (same as PolyWs).
const SUBSCRIBE_BATCH_SIZE: usize = 100;

/// Where a market price came from.
enum PriceSource {
    /// Real-time WebSocket best_ask (fresh, <1s old).
    Ws(f64),
    /// Gamma API outcome_price from last market refresh (possibly stale).
    Gamma(f64),
}

impl PriceSource {
    fn price(&self) -> f64 {
        match self {
            PriceSource::Ws(p) | PriceSource::Gamma(p) => *p,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            PriceSource::Ws(_) => "live WS",
            PriceSource::Gamma(_) => "Gamma (stale)",
        }
    }
}

pub struct WeatherStrategy {
    weather: Arc<WeatherFetcher>,
    market_finder: Arc<MarketFinder>,
    best_asks: SharedBestAsks,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
    config: Arc<Config>,
    ws_rx: SharedWsClient,
    health: Arc<BotHealth>,
    paper_tracker: Arc<PaperTracker>,
    /// Token IDs we've already subscribed to on the WS (avoid re-subscribing).
    subscribed_tokens: HashSet<U256>,
}

impl WeatherStrategy {
    pub fn new(
        weather: Arc<WeatherFetcher>,
        market_finder: Arc<MarketFinder>,
        best_asks: SharedBestAsks,
        action_tx: mpsc::UnboundedSender<StrategyAction>,
        config: Arc<Config>,
        ws_rx: SharedWsClient,
        health: Arc<BotHealth>,
        paper_tracker: Arc<PaperTracker>,
    ) -> Self {
        WeatherStrategy {
            weather,
            market_finder,
            best_asks,
            action_tx,
            config,
            ws_rx,
            health,
            paper_tracker,
            subscribed_tokens: HashSet::new(),
        }
    }

    pub async fn run(mut self) {
        tracing::info!("Weather strategy started ({})", if self.config.alert_only { "alert-only" } else { "LIVE" });

        // Ensure logs directory exists
        if let Err(e) = std::fs::create_dir_all("logs") {
            tracing::error!("Failed to create logs directory: {e}");
            return;
        }

        // Open edge CSV file (append mode)
        let csv_path = "logs/weather_edges.csv";
        let file_exists = std::path::Path::new(csv_path).exists();
        let mut csv_file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(csv_path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to open {csv_path}: {e}");
                return;
            }
        };

        // Write CSV header if new file
        if !file_exists {
            let _ = writeln!(
                csv_file,
                "timestamp,city,date,bucket_label,bucket_lower,bucket_upper,\
                 fahrenheit,forecast_prob,market_price,price_source,edge,\
                 model_breakdown,condition_id,yes_token_id,\
                 gaussian_prob,counting_prob,ensemble_mean,ensemble_std,inflated_std,method,\
                 kelly_full,kelly_bet"
            );
        }

        // Track which (city, date, bucket) edges we've already logged to avoid duplicates.
        // Cleared daily when date changes.
        let mut logged_edges: HashSet<(String, chrono::NaiveDate, String)> = HashSet::new();
        let mut logged_date = chrono::Utc::now().date_naive();

        // Subscribe initial weather tokens to WS
        self.subscribe_new_weather_tokens().await;

        // Send startup summary
        self.send_startup_summary().await;

        // Run initial edge scan immediately
        match self.scan_for_edges(&mut csv_file, &mut logged_edges).await {
            Ok(edges) => self.health.weather_scan_complete(edges),
            Err(e) => tracing::warn!("Initial weather edge scan failed: {e}"),
        }

        loop {
            let interval = self.current_scan_interval();
            tracing::debug!("Weather: next scan in {interval}s");
            tokio::time::sleep(Duration::from_secs(interval)).await;

            // Clear dedup set at midnight UTC
            let today = chrono::Utc::now().date_naive();
            if today != logged_date {
                logged_edges.clear();
                logged_date = today;
            }

            // Refresh weather markets from Gamma before scanning
            // On failure, continue scanning with cached market data rather than skipping
            if let Err(e) = self.market_finder.refresh_weather().await {
                tracing::warn!("Weather market refresh failed (using cached data): {e}");
            }

            // Subscribe any newly discovered weather tokens to WS
            self.subscribe_new_weather_tokens().await;

            match self.scan_for_edges(&mut csv_file, &mut logged_edges).await {
                Ok(edges) => self.health.weather_scan_complete(edges),
                Err(e) => tracing::warn!("Weather edge scan failed: {e}"),
            }
        }
    }

    /// Subscribe new weather market tokens and prune expired ones.
    /// Uses the shared WsClient (via watch receiver) — automatically picks up
    /// the latest connection after PolyWs reconnections.
    async fn subscribe_new_weather_tokens(&mut self) {
        // Clear tracked subscriptions to force re-subscribe on current WS connection.
        // After PolyWs reconnects, old subscriptions are lost — clearing ensures
        // we always re-subscribe all active tokens on the latest connection.
        self.subscribed_tokens.clear();

        let markets = self.market_finder.weather_markets().await;
        let current_tokens: HashSet<U256> =
            markets.iter().map(|wm| wm.market.yes_token_id).collect();

        // Get current WsClient from watch channel (latest connection)
        let ws = self.ws_rx.borrow().clone();

        // Prune expired tokens (no longer in active markets)
        let expired: Vec<U256> = self
            .subscribed_tokens
            .iter()
            .filter(|t| !current_tokens.contains(t))
            .copied()
            .collect();

        if !expired.is_empty() {
            // Unsubscribe expired tokens from WS to free broadcast buffer
            if let Err(e) = ws.unsubscribe_orderbook(&expired) {
                tracing::debug!("Weather: failed to unsubscribe expired tokens: {e}");
            }
            for t in &expired {
                self.subscribed_tokens.remove(t);
            }
            tracing::info!(
                "Weather: pruned {} expired tokens from WS (remaining: {})",
                expired.len(),
                self.subscribed_tokens.len(),
            );
        }

        // Subscribe new tokens
        let new_tokens: Vec<U256> = current_tokens
            .into_iter()
            .filter(|t| self.subscribed_tokens.insert(*t))
            .collect();

        if new_tokens.is_empty() {
            return;
        }

        for chunk in new_tokens.chunks(SUBSCRIBE_BATCH_SIZE) {
            // subscribe_best_bid_ask sends SUBSCRIBE to the server and returns a stream.
            // We drop the returned stream — the existing BBA stream in PolyWs will receive
            // events for these tokens because they share the same underlying WS connection.
            match ws.subscribe_best_bid_ask(chunk.to_vec()) {
                Ok(_stream) => { /* drop stream; events flow to shared BBA stream */ }
                Err(e) => {
                    tracing::warn!(
                        "Weather: failed to subscribe {} tokens to WS: {e}",
                        chunk.len()
                    );
                    for t in chunk {
                        self.subscribed_tokens.remove(t);
                    }
                }
            }
        }

        tracing::info!(
            "Weather: subscribed {} new tokens, total active: {}",
            new_tokens.len(),
            self.subscribed_tokens.len(),
        );
    }

    async fn send_startup_summary(&self) {
        let markets = self.market_finder.weather_markets().await;
        if markets.is_empty() {
            let _ = self.action_tx.send(StrategyAction::Alert(
                "🌡️ <b>Weather Bot Started</b>\nNo weather markets found. Will retry every scan."
                    .to_string(),
            ));
            return;
        }

        // Count unique cities and dates
        let mut cities = std::collections::HashSet::new();
        let mut dates = std::collections::HashSet::new();
        for wm in &markets {
            cities.insert(&wm.city_slug);
            dates.insert(wm.date);
        }

        // Check how many have fresh WS price data
        let now = Instant::now();
        let ws_count = markets
            .iter()
            .filter(|wm| {
                self.best_asks
                    .read()
                    .ok()
                    .and_then(|asks| {
                        asks.get(&wm.market.yes_token_id)
                            .filter(|(_, seen_at)| {
                                now.duration_since(*seen_at) <= Self::WS_PRICE_MAX_AGE
                            })
                            .map(|(p, _)| *p)
                    })
                    .is_some()
            })
            .count();

        let mode = if self.config.alert_only { "ALERT ONLY (no orders)" } else { "🔴 LIVE TRADING" };
        let msg = format!(
            "🌡️ <b>Weather Bot Started</b>\n\
             Markets: {} buckets across {} cities, {} dates\n\
             WS prices: {}/{} buckets have live data\n\
             Edge threshold: {:.0}%\n\
             Scan interval: {}s\n\
             Mode: {mode}",
            markets.len(),
            cities.len(),
            dates.len(),
            ws_count,
            markets.len(),
            self.config.edge_threshold * 100.0,
            self.config.weather_scan_interval_secs,
        );

        let _ = self.action_tx.send(StrategyAction::Alert(msg));
    }

    /// Determine scan interval based on GFS data availability windows.
    /// GFS data drops ~3.5h after cycle starts (00/06/12/18 UTC):
    /// approximately 03:30, 09:30, 15:30, 21:30 UTC.
    /// Scan every 5 min for 1 hour after each drop, 30 min otherwise.
    fn current_scan_interval(&self) -> u64 {
        let now = chrono::Utc::now();
        let minutes_since_midnight = now.hour() * 60 + now.minute();

        // GFS availability windows (minutes since midnight UTC)
        const GFS_WINDOWS: [u32; 4] = [210, 570, 930, 1290]; // 3:30, 9:30, 15:30, 21:30

        let in_fast_window = GFS_WINDOWS.iter().any(|&window_start| {
            minutes_since_midnight >= window_start && minutes_since_midnight < window_start + 60
        });

        if in_fast_window {
            tracing::debug!("Weather: in GFS data window — using fast scan interval");
            self.config.weather_fast_scan_interval_secs
        } else {
            self.config.weather_scan_interval_secs
        }
    }

    async fn scan_for_edges(
        &self,
        csv_file: &mut File,
        logged_edges: &mut std::collections::HashSet<(String, chrono::NaiveDate, String)>,
    ) -> anyhow::Result<u32> {
        let weather_markets = self.market_finder.weather_markets().await;

        if weather_markets.is_empty() {
            tracing::warn!("Weather: no markets to scan (cache is empty)");
            return Ok(0);
        }

        tracing::debug!("Weather: scanning {} cached markets", weather_markets.len());

        // Group markets by city_slug + date (one forecast fetch per city-date)
        let mut city_dates: std::collections::HashMap<(String, chrono::NaiveDate), Vec<usize>> =
            std::collections::HashMap::new();

        for (i, wm) in weather_markets.iter().enumerate() {
            city_dates
                .entry((wm.city_slug.clone(), wm.date))
                .or_default()
                .push(i);
        }

        let mut total_edges = 0u32;
        let mut total_scanned = 0u32;
        let mut no_price_count = 0u32;
        let mut forecast_calls = 0u32;
        let mut ws_price_count = 0u32;
        let mut gamma_price_count = 0u32;
        let mut stale_skip_count = 0u32;

        let mut rate_limited = false;

        for ((city_slug, date), market_indices) in &city_dates {
            if rate_limited {
                continue;
            }

            let (lat, lon, fahrenheit, timezone, utc_offset) =
                match MarketFinder::weather_city(city_slug) {
                    Some(c) => c,
                    None => continue,
                };

            // Skip dates already past in the city's local timezone
            let utc_now = chrono::Utc::now();
            let local_approx = utc_now + chrono::Duration::hours(utc_offset as i64);
            let local_date = local_approx.date_naive();
            if *date < local_date {
                tracing::debug!(
                    "Weather: skipping {city_slug} {date} — past in local time (UTC{utc_offset:+})"
                );
                continue;
            }

            // Rate-limit: 500ms between city-date calls.
            if forecast_calls > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            forecast_calls += 1;

            // Fetch multi-model ensemble forecast (single combined API call, all models)
            let forecasts = match self
                .weather
                .fetch_combined_models(city_slug, lat, lon, fahrenheit, timezone)
                .await
            {
                Ok(f) => f,
                Err(e) if e.to_string().contains("429") => {
                    let remaining = city_dates.len() as u32 - forecast_calls;
                    tracing::warn!(
                        "Weather: Open-Meteo rate limit hit — skipping remaining {remaining} cities"
                    );
                    rate_limited = true;
                    continue;
                }
                Err(e) => {
                    tracing::warn!("Weather: forecast failed for {city_slug} {date}: {e}");
                    continue;
                }
            };

            let forecast = match forecasts.get(date) {
                Some(f) => f,
                None => {
                    let available: Vec<_> = forecasts.keys().collect();
                    tracing::warn!(
                        "Weather: no forecast data for {city_slug} {date} (API returned dates: {available:?})"
                    );
                    continue;
                }
            };

            for &idx in market_indices {
                let wm = &weather_markets[idx];
                total_scanned += 1;

                // Get market price with source tracking
                let price_source = match self.get_market_price(wm) {
                    Some(src) => src,
                    None => {
                        no_price_count += 1;
                        continue;
                    }
                };

                match &price_source {
                    PriceSource::Ws(_) => ws_price_count += 1,
                    PriceSource::Gamma(_) => gamma_price_count += 1,
                }

                // Skip stale Gamma prices when require_ws_price is enabled
                if self.config.require_ws_price && matches!(price_source, PriceSource::Gamma(_)) {
                    stale_skip_count += 1;
                    continue;
                }

                let market_price = price_source.price();
                if market_price <= 0.0 || market_price >= 1.0 {
                    continue;
                }

                // Only bet on "at least X" tail buckets (bucket_upper == INFINITY).
                // Range buckets (0W/10) and "at most" tails (0W/6) have 0% win rate
                // in paper trading. This filter alone turns -$5 into +$55.
                if wm.bucket_upper != f64::INFINITY {
                    continue;
                }

                // Skip phantom/dead markets — $0.001 means empty order book, not a real ask.
                // Anything below ~$0.03 is either no liquidity or the market genuinely
                // sees near-zero probability (in which case our model shouldn't override it).
                if market_price < self.config.min_entry_price {
                    continue;
                }

                // Skip overpriced buckets — bad risk/reward above threshold
                if market_price > self.config.max_entry_price {
                    continue;
                }

                // Adjust std inflation by lead time: longer forecasts need wider spread.
                // Day+0: 0.83× base (≈1.5 if base=1.8) — freshest data, least uncertainty
                // Day+1: 1.0× base (≈1.8) — standard
                // Day+2: 1.22× base (≈2.2) — high uncertainty, penalize false edges
                let lead_days = (*date - local_date).num_days();
                let lead_factor = match lead_days {
                    0 => 0.83,
                    1 => 1.0,
                    _ => 1.22,
                };
                let adjusted_std_inflation = self.config.std_inflation * lead_factor;

                // Calculate forecast probability using Gaussian CDF with bias correction
                let gbp = weather::bucket_probability_gaussian(
                    forecast,
                    wm.bucket_lower,
                    wm.bucket_upper,
                    adjusted_std_inflation,
                    self.config.apply_bias_correction,
                    wm.fahrenheit,
                );

                // Subtract slippage estimate from raw edge.
                // Maker orders must post below best_ask (at least 1 tick).
                // Additional slippage from thin books and price movement.
                let edge = gbp.prob - market_price - self.config.slippage_estimate;

                // Skip unless OUR forecast says it's likely to happen.
                // We don't care what the market thinks — if they're wrong, that's our edge.
                // e.g. market says 3% but we say 63% → huge opportunity, don't skip it.
                if gbp.prob < self.config.min_probability {
                    continue;
                }

                if edge > self.config.edge_threshold {
                    // Deduplicate: only log/alert each (city, date, bucket) once per day
                    let dedup_key = (wm.city_slug.clone(), wm.date, wm.bucket_label());
                    if !logged_edges.insert(dedup_key) {
                        continue; // already logged this edge today
                    }

                    total_edges += 1;

                    // Kelly criterion: optimal bet size for binary outcomes
                    // full_kelly = edge / (1 - market_price)
                    // Fractional Kelly (default 25%) reduces variance
                    let full_kelly = edge / (1.0 - market_price);
                    let kelly_bet =
                        (full_kelly * self.config.kelly_fraction * self.config.bankroll).max(0.0);
                    // Cap at max weather position size AND max trade size (risk manager limit)
                    let max_pos = decimal_to_f64(self.config.max_weather_position);
                    let max_trade = decimal_to_f64(self.config.max_trade_usd);
                    let kelly_bet = kelly_bet.min(max_pos).min(max_trade);

                    let unit = if wm.fahrenheit { "F" } else { "C" };
                    let model_info = forecast
                        .model_breakdown
                        .iter()
                        .map(|(name, count)| format!("{name}:{count}"))
                        .collect::<Vec<_>>()
                        .join("+");
                    let msg = format!(
                        "🌡️ <b>Weather Edge</b>: {} {}\n\
                         Bucket: {}\n\
                         Gaussian: {:.1}% (counting: {:.1}%, {}/{} members [{}])\n\
                         Market: ${:.3} ({:.1}%) [{}]\n\
                         Edge: <b>+{:.1}%</b>\n\
                         Kelly: ${:.2} ({:.0}% Kelly × {:.0}% bankroll)\n\
                         Ensemble: mean {:.1}°{unit}, std {:.1}° (inflated {:.1}°)\n\
                         Range: {:.1}–{:.1}°{unit}\n\
                         {}",

                        wm.city_name,
                        wm.date.format("%b %-d"),
                        wm.bucket_label(),
                        gbp.prob * 100.0,
                        gbp.counting_prob * 100.0,
                        gbp.counting_count,
                        gbp.counting_total,
                        model_info,
                        market_price,
                        market_price * 100.0,
                        price_source.label(),
                        edge * 100.0,
                        kelly_bet,
                        full_kelly * 100.0,
                        self.config.kelly_fraction * 100.0,
                        gbp.ensemble_mean,
                        gbp.ensemble_std,
                        gbp.inflated_std,
                        gbp.corrected_min,
                        gbp.corrected_max,
                        if self.config.alert_only { "⚠️ ALERT ONLY — no order placed" } else { "🔴 LIVE — placing order" },
                    );

                    tracing::info!(
                        "Weather edge: {} {} {} — gaussian={:.1}% market={:.1}% [{}] edge=+{:.1}% kelly=${:.2}",
                        wm.city_name,
                        wm.date,
                        wm.bucket_label(),
                        gbp.prob * 100.0,
                        market_price * 100.0,
                        price_source.label(),
                        edge * 100.0,
                        kelly_bet,
                    );

                    let _ = self.action_tx.send(StrategyAction::Alert(msg));

                    // Place real order if not in alert-only mode
                    if !self.config.alert_only && kelly_bet > 0.0 && market_price > 0.01 {
                        let price_dec = f64_to_decimal(market_price);
                        // Floor shares to 2 decimals to avoid rounding up past budget
                        let size_dec = f64_to_decimal_floor(kelly_bet / market_price);
                        let reason = format!(
                            "Weather: {} {} {} (f={:.0}% m={:.0}% edge=+{:.1}%)",
                            wm.city_name,
                            wm.date.format("%b %-d"),
                            wm.bucket_label(),
                            gbp.prob * 100.0,
                            market_price * 100.0,
                            edge * 100.0,
                        );
                        let _ = self.action_tx.send(StrategyAction::PlaceOrder {
                            token_id: wm.market.yes_token_id,
                            price: price_dec,
                            size: size_dec,
                            reason,
                        });
                    }

                    // Track for outcome verification
                    self.paper_tracker.add_pending_weather(
                        &wm.market.condition_id,
                        &wm.city_name,
                        &wm.city_slug,
                        wm.date,
                        &wm.bucket_label(),
                        gbp.prob,
                        market_price,
                        edge,
                        kelly_bet,
                        &model_info,
                        wm.market.end_date,
                    );

                    // Log edge to CSV
                    let _ = writeln!(
                        csv_file,
                        "{},{},{},{},{},{},{},{:.4},{:.4},{},{:.4},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.2},gaussian,{:.4},{:.2}",
                        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                        wm.city_slug,
                        wm.date,
                        wm.bucket_label(),
                        format_bound(wm.bucket_lower),
                        format_bound(wm.bucket_upper),
                        wm.fahrenheit,
                        gbp.prob,
                        market_price,
                        price_source.label(),
                        edge,
                        model_info,
                        wm.market.condition_id,
                        wm.market.yes_token_id,
                        gbp.prob,
                        gbp.counting_prob,
                        gbp.ensemble_mean,
                        gbp.ensemble_std,
                        gbp.inflated_std,
                        full_kelly,
                        kelly_bet,
                    );
                }
            }
        }

        // Flush CSV after each scan cycle
        let _ = csv_file.flush();

        tracing::info!(
            "Weather scan: {} edges, {}/{} buckets with prices (WS: {}, Gamma: {}, skipped stale: {}), {} no price, {} forecast API calls, {} cached models",
            total_edges,
            total_scanned - no_price_count,
            total_scanned,
            ws_price_count,
            gamma_price_count,
            stale_skip_count,
            no_price_count,
            forecast_calls,
            self.weather.cache_size(),
        );

        Ok(total_edges)
    }

    /// Max age for a WS price to be considered fresh for weather edge detection.
    const WS_PRICE_MAX_AGE: Duration = Duration::from_secs(300);

    /// Get market price for a weather bucket.
    /// Returns the price and its source, or None if no price is available.
    fn get_market_price(&self, wm: &WeatherMarket) -> Option<PriceSource> {
        // Try real-time WS data first
        if let Ok(asks) = self.best_asks.read() {
            if let Some(&(ask, seen_at)) = asks.get(&wm.market.yes_token_id) {
                if seen_at.elapsed() <= Self::WS_PRICE_MAX_AGE {
                    let price = decimal_to_f64(ask);
                    if price > 0.0 {
                        return Some(PriceSource::Ws(price));
                    }
                }
                // WS price exists but is stale — fall through to Gamma
            }
        }

        // Fall back to Gamma API price from discovery time
        if wm.gamma_yes_price > 0.0 {
            return Some(PriceSource::Gamma(wm.gamma_yes_price));
        }

        None
    }
}

fn decimal_to_f64(d: polymarket_client_sdk::types::Decimal) -> f64 {
    use std::str::FromStr;
    f64::from_str(&d.to_string()).unwrap_or(0.0)
}

fn f64_to_decimal(v: f64) -> polymarket_client_sdk::types::Decimal {
    use std::str::FromStr;
    // 2 decimal places: respects 0.01 tick size and LOT_SIZE_SCALE=2
    polymarket_client_sdk::types::Decimal::from_str(&format!("{:.2}", v)).unwrap_or_default()
}

/// Like f64_to_decimal but floors instead of rounding.
/// Used for share sizes to avoid rounding up past the budget.
fn f64_to_decimal_floor(v: f64) -> polymarket_client_sdk::types::Decimal {
    use std::str::FromStr;
    let floored = (v * 100.0).floor() / 100.0;
    polymarket_client_sdk::types::Decimal::from_str(&format!("{:.2}", floored)).unwrap_or_default()
}

/// Format a bucket bound for CSV (infinity values as -inf/inf).
fn format_bound(v: f64) -> String {
    if v == f64::NEG_INFINITY {
        "-inf".to_string()
    } else if v == f64::INFINITY {
        "inf".to_string()
    } else {
        format!("{v:.1}")
    }
}
