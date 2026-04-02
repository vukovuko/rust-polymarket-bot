use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate};
use polymarket_client_sdk::gamma::Client as GammaClient;
use polymarket_client_sdk::gamma::types::request::EventBySlugRequest;
use polymarket_client_sdk::types::{Decimal, Utc};
use tokio::sync::RwLock;

use super::client::PolyClient;
use super::types::{BotMarket, WeatherMarket};

/// How many 5-minute windows into the future to fetch.
/// 12 windows = 1 hour of upcoming markets.
const LOOKAHEAD_WINDOWS: u64 = 12;
/// Seconds per window.
const WINDOW_SECS: u64 = 300;

/// City configuration for weather market discovery.
pub struct WeatherCity {
    pub slug: &'static str,
    pub name: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub fahrenheit: bool,
    /// IANA timezone for Open-Meteo daily aggregation.
    /// Critical: without this, daily max is computed over UTC day, not local day.
    pub timezone: &'static str,
    /// Approximate UTC offset (DST-active value for cities with DST, ±1h is fine).
    /// Used to skip past-date markets without pulling in a full tz library.
    pub utc_offset_hours: i32,
}

/// Coordinates match the actual Weather Underground resolution stations (ICAO airports).
/// Polymarket resolves against WU, so we must forecast for the STATION location, not city center.
pub const WEATHER_CITIES: &[WeatherCity] = &[
    WeatherCity {
        slug: "nyc",
        name: "NYC",
        lat: 40.78, // KLGA (LaGuardia Airport)
        lon: -73.87,
        fahrenheit: true,
        timezone: "America/New_York",
        utc_offset_hours: -4,
    },
    WeatherCity {
        slug: "chicago",
        name: "Chicago",
        lat: 41.98, // KORD (O'Hare International)
        lon: -87.90,
        fahrenheit: true,
        timezone: "America/Chicago",
        utc_offset_hours: -5,
    },
    WeatherCity {
        slug: "miami",
        name: "Miami",
        lat: 25.80, // KMIA (Miami International)
        lon: -80.29,
        fahrenheit: true,
        timezone: "America/New_York",
        utc_offset_hours: -4,
    },
    WeatherCity {
        slug: "atlanta",
        name: "Atlanta",
        lat: 33.64, // KATL (Hartsfield-Jackson)
        lon: -84.43,
        fahrenheit: true,
        timezone: "America/New_York",
        utc_offset_hours: -4,
    },
    WeatherCity {
        slug: "dallas",
        name: "Dallas",
        lat: 32.85, // KDAL (Dallas Love Field)
        lon: -96.85,
        fahrenheit: true,
        timezone: "America/Chicago",
        utc_offset_hours: -5,
    },
    WeatherCity {
        slug: "seattle",
        name: "Seattle",
        lat: 47.45, // KSEA (Seattle-Tacoma)
        lon: -122.31,
        fahrenheit: true,
        timezone: "America/Los_Angeles",
        utc_offset_hours: -7,
    },
    WeatherCity {
        slug: "london",
        name: "London",
        lat: 51.50, // EGLC (London City Airport)
        lon: 0.05,
        fahrenheit: false,
        timezone: "Europe/London",
        utc_offset_hours: 1,
    },
    WeatherCity {
        slug: "paris",
        name: "Paris",
        lat: 49.01, // LFPG (Charles de Gaulle)
        lon: 2.55,
        fahrenheit: false,
        timezone: "Europe/Paris",
        utc_offset_hours: 2,
    },
    WeatherCity {
        slug: "seoul",
        name: "Seoul",
        lat: 37.46, // RKSI (Incheon International)
        lon: 126.44,
        fahrenheit: false,
        timezone: "Asia/Seoul",
        utc_offset_hours: 9,
    },
    WeatherCity {
        slug: "toronto",
        name: "Toronto",
        lat: 43.68, // CYYZ (Toronto Pearson)
        lon: -79.63,
        fahrenheit: false,
        timezone: "America/Toronto",
        utc_offset_hours: -4,
    },
    WeatherCity {
        slug: "ankara",
        name: "Ankara",
        lat: 40.12, // LTAC (Esenboga International)
        lon: 32.99,
        fahrenheit: false,
        timezone: "Europe/Istanbul",
        utc_offset_hours: 3,
    },
    WeatherCity {
        slug: "buenos-aires",
        name: "Buenos Aires",
        lat: -34.82, // SAEZ (Ministro Pistarini / Ezeiza)
        lon: -58.54,
        fahrenheit: false,
        timezone: "America/Argentina/Buenos_Aires",
        utc_offset_hours: -3,
    },
    WeatherCity {
        slug: "wellington",
        name: "Wellington",
        lat: -41.33, // NZWN (Wellington International)
        lon: 174.81,
        fahrenheit: false,
        timezone: "Pacific/Auckland",
        utc_offset_hours: 13,
    },
    WeatherCity {
        slug: "sao-paulo",
        name: "São Paulo",
        lat: -23.43, // SBGR (Guarulhos International)
        lon: -46.47,
        fahrenheit: false,
        timezone: "America/Sao_Paulo",
        utc_offset_hours: -3,
    },
    // Verified on Polymarket — stations confirmed against resolution rules
    WeatherCity {
        slug: "los-angeles",
        name: "Los Angeles",
        lat: 33.94, // KLAX (Los Angeles International)
        lon: -118.41,
        fahrenheit: true,
        timezone: "America/Los_Angeles",
        utc_offset_hours: -7,
    },
    WeatherCity {
        slug: "denver",
        name: "Denver",
        lat: 39.70, // KBKF (Buckley Space Force Base, Aurora CO)
        lon: -104.75,
        fahrenheit: true,
        timezone: "America/Denver",
        utc_offset_hours: -6,
    },
    WeatherCity {
        slug: "houston",
        name: "Houston",
        lat: 29.65, // KHOU (William P. Hobby Airport)
        lon: -95.28,
        fahrenheit: true,
        timezone: "America/Chicago",
        utc_offset_hours: -5,
    },
    WeatherCity {
        slug: "tokyo",
        name: "Tokyo",
        lat: 35.55, // RJTT (Haneda Airport)
        lon: 139.78,
        fahrenheit: false,
        timezone: "Asia/Tokyo",
        utc_offset_hours: 9,
    },
    WeatherCity {
        slug: "san-francisco",
        name: "San Francisco",
        lat: 37.62, // KSFO (San Francisco International)
        lon: -122.38,
        fahrenheit: true,
        timezone: "America/Los_Angeles",
        utc_offset_hours: -7,
    },
    WeatherCity {
        slug: "hong-kong",
        name: "Hong Kong",
        lat: 22.31, // VHHH (Hong Kong International)
        lon: 113.92,
        fahrenheit: false,
        timezone: "Asia/Hong_Kong",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "taipei",
        name: "Taipei",
        lat: 25.08, // RCTP (Taiwan Taoyuan International)
        lon: 121.23,
        fahrenheit: false,
        timezone: "Asia/Taipei",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "shanghai",
        name: "Shanghai",
        lat: 31.14, // ZSPD (Shanghai Pudong International)
        lon: 121.81,
        fahrenheit: false,
        timezone: "Asia/Shanghai",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "beijing",
        name: "Beijing",
        lat: 40.08, // ZBAA (Beijing Capital International)
        lon: 116.58,
        fahrenheit: false,
        timezone: "Asia/Shanghai",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "madrid",
        name: "Madrid",
        lat: 40.47, // LEMD (Adolfo Suárez Madrid-Barajas)
        lon: -3.56,
        fahrenheit: false,
        timezone: "Europe/Madrid",
        utc_offset_hours: 2,
    },
    WeatherCity {
        slug: "munich",
        name: "Munich",
        lat: 48.35, // EDDM (Munich Airport)
        lon: 11.79,
        fahrenheit: false,
        timezone: "Europe/Berlin",
        utc_offset_hours: 2,
    },
    WeatherCity {
        slug: "singapore",
        name: "Singapore",
        lat: 1.35, // WSSS (Singapore Changi)
        lon: 103.99,
        fahrenheit: false,
        timezone: "Asia/Singapore",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "istanbul",
        name: "Istanbul",
        lat: 41.26, // LTFM (Istanbul Airport)
        lon: 28.74,
        fahrenheit: false,
        timezone: "Europe/Istanbul",
        utc_offset_hours: 3,
    },
    WeatherCity {
        slug: "austin",
        name: "Austin",
        lat: 30.19, // KAUS (Austin-Bergstrom International)
        lon: -97.67,
        fahrenheit: true,
        timezone: "America/Chicago",
        utc_offset_hours: -5,
    },
    WeatherCity {
        slug: "mexico-city",
        name: "Mexico City",
        lat: 19.44, // MMMX (Benito Juárez International)
        lon: -99.07,
        fahrenheit: false,
        timezone: "America/Mexico_City",
        utc_offset_hours: -6,
    },
    WeatherCity {
        slug: "moscow",
        name: "Moscow",
        lat: 55.60, // UUWW (Vnukovo International)
        lon: 37.27,
        fahrenheit: false,
        timezone: "Europe/Moscow",
        utc_offset_hours: 3,
    },
    WeatherCity {
        slug: "milan",
        name: "Milan",
        lat: 45.63, // LIMC (Malpensa International)
        lon: 8.72,
        fahrenheit: false,
        timezone: "Europe/Rome",
        utc_offset_hours: 2,
    },
    WeatherCity {
        slug: "lucknow",
        name: "Lucknow",
        lat: 26.76, // VILK (Chaudhary Charan Singh International)
        lon: 80.88,
        fahrenheit: false,
        timezone: "Asia/Kolkata",
        utc_offset_hours: 5,
    },
    WeatherCity {
        slug: "chongqing",
        name: "Chongqing",
        lat: 29.72, // ZUCK (Chongqing Jiangbei International)
        lon: 106.64,
        fahrenheit: false,
        timezone: "Asia/Shanghai",
        utc_offset_hours: 8,
    },
    WeatherCity {
        slug: "shenzhen",
        name: "Shenzhen",
        lat: 22.64, // ZGSZ (Shenzhen Bao'an International)
        lon: 113.81,
        fahrenheit: false,
        timezone: "Asia/Shanghai",
        utc_offset_hours: 8,
    },
];

/// How many days ahead to fetch weather markets (today + 2 days).
/// Polymarket lists markets up to 3 days out; early entry on D+2 captures edges
/// before the market has fully priced in the forecast.
const WEATHER_LOOKAHEAD_DAYS: i64 = 3;

pub struct MarketFinder {
    client: Arc<PolyClient>,
    gamma: GammaClient,
    all_markets: RwLock<Vec<BotMarket>>,
    weather_markets: RwLock<Vec<WeatherMarket>>,
    last_full_scan: RwLock<Instant>,
}

impl MarketFinder {
    pub fn new(client: Arc<PolyClient>) -> Self {
        MarketFinder {
            client,
            gamma: GammaClient::default(),
            all_markets: RwLock::new(Vec::new()),
            weather_markets: RwLock::new(Vec::new()),
            last_full_scan: RwLock::new(Instant::now()),
        }
    }

    /// Refresh BTC 5-min markets using targeted Gamma API slug lookups.
    /// Fetches the current window + LOOKAHEAD_WINDOWS into the future.
    /// Falls back to full CLOB scan if Gamma fails.
    pub async fn refresh(&self) -> Result<()> {
        match self.refresh_via_gamma().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                tracing::warn!("Gamma refresh failed, falling back to CLOB scan: {e}");
            }
        }
        self.refresh_via_clob().await
    }

    /// Targeted refresh: construct deterministic slugs and fetch only BTC 5-min markets.
    /// Makes ~12 Gamma API requests instead of paginating 33k+ CLOB markets.
    async fn refresh_via_gamma(&self) -> Result<()> {
        let now_unix = Utc::now().timestamp() as u64;
        // Current window start: round down to nearest 300s boundary
        let current_start = (now_unix / WINDOW_SECS) * WINDOW_SECS;

        let mut new_markets = Vec::new();

        for i in 0..LOOKAHEAD_WINDOWS {
            let window_start = current_start + i * WINDOW_SECS;
            let slug = format!("btc-updown-5m-{window_start}");

            // Rate-limit: 150ms between Gamma API calls
            if i > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            }

            match self.fetch_btc_market_by_slug(&slug).await {
                Ok(Some(market)) => new_markets.push(market),
                Ok(None) => {
                    tracing::debug!("No market found for slug {slug}");
                }
                Err(e) => {
                    tracing::debug!("Failed to fetch slug {slug}: {e}");
                }
            }
        }

        if new_markets.is_empty() {
            anyhow::bail!("Gamma returned 0 BTC 5-min markets");
        }

        // Merge with existing markets (keep old ones that are still valid, add new ones)
        let mut markets = self.all_markets.write().await;
        let now = Utc::now();

        // Remove expired markets (end_date in the past)
        markets.retain(|m| m.end_date.is_none_or(|end| end > now));

        // Add new markets that don't already exist
        for m in new_markets {
            if !markets
                .iter()
                .any(|existing| existing.condition_id == m.condition_id)
            {
                markets.push(m);
            }
        }

        let btc_count = markets.iter().filter(|m| is_btc_5min_market(m)).count();
        tracing::info!(
            "Market refresh (Gamma): {} total cached, {} BTC 5-min markets",
            markets.len(),
            btc_count,
        );

        *self.last_full_scan.write().await = Instant::now();
        Ok(())
    }

    /// Fetch a single BTC 5-min event by its deterministic slug via Gamma API.
    async fn fetch_btc_market_by_slug(&self, slug: &str) -> Result<Option<BotMarket>> {
        let request = EventBySlugRequest::builder().slug(slug).build();
        let event = self
            .gamma
            .event_by_slug(&request)
            .await
            .context("Gamma event_by_slug failed")?;

        let gamma_markets = match event.markets {
            Some(m) => m,
            None => return Ok(None),
        };

        // BTC 5-min events have exactly 1 market with 2 outcomes (Up/Down)
        let gm = match gamma_markets.first() {
            Some(m) => m,
            None => return Ok(None),
        };

        let token_ids = match &gm.clob_token_ids {
            Some(ids) if ids.len() == 2 => ids,
            _ => return Ok(None),
        };

        let outcomes = match &gm.outcomes {
            Some(o) if o.len() == 2 => o,
            _ => return Ok(None),
        };

        let condition_id = match gm.condition_id {
            Some(c) => format!("{c:?}"),
            None => return Ok(None),
        };

        let first = outcomes[0].to_lowercase();
        let (yes_idx, no_idx) = if first == "yes" || first == "up" {
            (0, 1)
        } else {
            (1, 0)
        };

        Ok(Some(BotMarket {
            condition_id,
            question: gm.question.clone().unwrap_or_default(),
            market_slug: gm.slug.clone().unwrap_or_default(),
            end_date: gm.end_date,
            yes_token_id: token_ids[yes_idx],
            no_token_id: token_ids[no_idx],
            yes_outcome: outcomes[yes_idx].clone(),
            no_outcome: outcomes[no_idx].clone(),
            minimum_tick_size: Decimal::new(1, 2),
            minimum_order_size: Decimal::ONE,
            neg_risk: false,
            active: gm.active.unwrap_or(true),
            enable_order_book: true,
        }))
    }

    /// Fallback: full CLOB scan (paginated, rate-limit prone).
    /// Only overwrites the market list if we get a reasonable number of BTC markets.
    async fn refresh_via_clob(&self) -> Result<()> {
        let markets = self.client.fetch_all_active_markets().await?;
        let btc_5min_count = markets.iter().filter(|m| is_btc_5min_market(m)).count();

        tracing::info!(
            "Market refresh (CLOB): {} total active, {} BTC 5-min markets",
            markets.len(),
            btc_5min_count,
        );

        // Don't overwrite if we got a truncated result (rate-limited)
        if btc_5min_count == 0 {
            let existing_count = self.all_markets.read().await.len();
            if existing_count > 0 {
                tracing::warn!(
                    "CLOB refresh returned 0 BTC markets but we have {} cached — keeping cache",
                    existing_count,
                );
                return Ok(());
            }
        }

        *self.all_markets.write().await = markets;
        *self.last_full_scan.write().await = Instant::now();
        Ok(())
    }

    pub async fn find_current_btc_5min(&self) -> Option<BotMarket> {
        let markets = self.all_markets.read().await;
        let now = Utc::now();

        markets
            .iter()
            .filter(|m| is_btc_5min_market(m))
            .filter(|m| {
                // Market must end in the future (still active/open)
                m.end_date.is_some_and(|end| end > now)
            })
            .min_by_key(|m| m.end_date)
            .cloned()
    }

    #[allow(dead_code)]
    pub async fn all_markets(&self) -> Vec<BotMarket> {
        self.all_markets.read().await.clone()
    }

    /// Return only BTC 5-minute markets (for WebSocket subscriptions).
    pub async fn btc_5min_markets(&self) -> Vec<BotMarket> {
        self.all_markets
            .read()
            .await
            .iter()
            .filter(|m| is_btc_5min_market(m))
            .cloned()
            .collect()
    }

    /// Add a single market (e.g. from a WebSocket new_market event).
    /// Skips duplicates by condition_id.
    /// Currently unused: NewMarket WS stream was removed (SDK limitation).
    #[allow(dead_code)]
    pub async fn add_market(&self, market: BotMarket) {
        let mut markets = self.all_markets.write().await;
        let already_exists = markets
            .iter()
            .any(|m| m.condition_id == market.condition_id);

        if !already_exists {
            tracing::info!(
                "MarketFinder: added new market \"{}\" (condition_id={})",
                market.question,
                market.condition_id,
            );
            markets.push(market);
        }
    }

    #[allow(dead_code)]
    pub async fn market_count(&self) -> usize {
        self.all_markets.read().await.len()
    }

    // --- Weather market methods ---

    /// Refresh weather markets for all cities × next N days via Gamma API.
    /// Adds 150ms delay between requests to avoid rate limiting.
    /// Total: 22 cities × 3 days = 66 requests, ~10s total.
    pub async fn refresh_weather(&self) -> Result<()> {
        let today = Utc::now().date_naive();
        let mut new_markets = Vec::new();
        let mut found_cities = 0u32;
        let mut api_calls = 0u32;
        let mut api_errors = 0u32;
        let start = std::time::Instant::now();

        for day_offset in 0..WEATHER_LOOKAHEAD_DAYS {
            let date = today + chrono::Duration::days(day_offset);
            let month = month_name(date.month());
            let day = date.day();
            let year = date.year();

            for city in WEATHER_CITIES {
                let event_slug = format!(
                    "highest-temperature-in-{}-on-{}-{}-{}",
                    city.slug, month, day, year,
                );

                // Rate-limit: 150ms between Gamma API calls
                if api_calls > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                }
                api_calls += 1;

                match self.fetch_weather_event(&event_slug, city, date).await {
                    Ok(markets) if !markets.is_empty() => {
                        found_cities += 1;
                        tracing::debug!(
                            "Weather: {} {} — {} buckets",
                            city.name,
                            date,
                            markets.len(),
                        );
                        new_markets.extend(markets);
                    }
                    Ok(_) => {
                        tracing::debug!("Weather: no markets for {event_slug}");
                    }
                    Err(e) => {
                        api_errors += 1;
                        tracing::debug!("Weather: failed {event_slug}: {e}");
                    }
                }
            }
        }

        let elapsed = start.elapsed();

        if new_markets.is_empty() {
            anyhow::bail!(
                "Weather refresh found 0 markets ({api_calls} API calls, {api_errors} errors, {elapsed:.1?})"
            );
        }

        tracing::info!(
            "Weather refresh: {} buckets across {} city-days ({} API calls, {} errors, {:.1?})",
            new_markets.len(),
            found_cities,
            api_calls,
            api_errors,
            elapsed,
        );

        // Merge with existing markets rather than replacing entirely.
        // This prevents partial API failures from dropping valid cached markets.
        let mut markets = self.weather_markets.write().await;

        // Remove markets whose date has passed (expired)
        let today = Utc::now().date_naive();
        markets.retain(|wm| wm.date >= today);

        // Add/update markets from the new scan
        for new_wm in new_markets {
            // Replace if same (city, date, bucket), otherwise add
            if let Some(existing) = markets.iter_mut().find(|m| {
                m.city_slug == new_wm.city_slug
                    && m.date == new_wm.date
                    && m.bucket_lower == new_wm.bucket_lower
                    && m.bucket_upper == new_wm.bucket_upper
            }) {
                *existing = new_wm;
            } else {
                markets.push(new_wm);
            }
        }

        Ok(())
    }

    /// Fetch a single weather event and parse its temperature bucket markets.
    async fn fetch_weather_event(
        &self,
        slug: &str,
        city: &WeatherCity,
        date: NaiveDate,
    ) -> Result<Vec<WeatherMarket>> {
        let request = EventBySlugRequest::builder().slug(slug).build();
        let event = self
            .gamma
            .event_by_slug(&request)
            .await
            .context("Gamma event_by_slug failed")?;

        let gamma_markets = match event.markets {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut results = Vec::new();

        for gm in &gamma_markets {
            let question = gm.question.as_deref().unwrap_or("");

            // Parse temperature bucket from question
            let (lower, upper) = match parse_temp_bucket(question) {
                Some(bounds) => bounds,
                None => {
                    tracing::debug!("Weather: could not parse bucket from: {question}");
                    continue;
                }
            };

            // Extract token IDs (binary market: Yes/No)
            let token_ids = match &gm.clob_token_ids {
                Some(ids) if ids.len() == 2 => ids,
                _ => continue,
            };

            let outcomes = match &gm.outcomes {
                Some(o) if o.len() == 2 => o,
                _ => continue,
            };

            let condition_id = match gm.condition_id {
                Some(c) => format!("{c:?}"),
                None => continue,
            };

            let first = outcomes[0].to_lowercase();
            let (yes_idx, no_idx) = if first == "yes" { (0, 1) } else { (1, 0) };

            let tick_size = gm.order_price_min_tick_size.unwrap_or(Decimal::new(1, 3)); // Default $0.001
            let min_size = gm.order_min_size.unwrap_or(Decimal::new(5, 0)); // Default 5

            let bot_market = BotMarket {
                condition_id,
                question: question.to_string(),
                market_slug: gm.slug.clone().unwrap_or_default(),
                end_date: gm.end_date,
                yes_token_id: token_ids[yes_idx],
                no_token_id: token_ids[no_idx],
                yes_outcome: outcomes[yes_idx].clone(),
                no_outcome: outcomes[no_idx].clone(),
                minimum_tick_size: tick_size,
                minimum_order_size: min_size,
                neg_risk: gm.neg_risk.unwrap_or(true),
                active: gm.active.unwrap_or(true),
                enable_order_book: true,
            };

            // Extract YES price from Gamma outcome_prices
            let gamma_yes_price = gm
                .outcome_prices
                .as_ref()
                .and_then(|p| p.get(yes_idx))
                .and_then(|d| {
                    use std::str::FromStr;
                    f64::from_str(&d.to_string()).ok()
                })
                .unwrap_or(0.0);

            results.push(WeatherMarket {
                market: bot_market,
                city_slug: city.slug.to_string(),
                city_name: city.name.to_string(),
                date,
                bucket_lower: lower,
                bucket_upper: upper,
                fahrenheit: city.fahrenheit,
                gamma_yes_price,
            });
        }

        Ok(results)
    }

    /// Get all cached weather markets.
    pub async fn weather_markets(&self) -> Vec<WeatherMarket> {
        self.weather_markets.read().await.clone()
    }

    /// Get weather city config by slug (for forecast fetching).
    /// Returns (lat, lon, fahrenheit, timezone, utc_offset_hours).
    pub fn weather_city(slug: &str) -> Option<(f64, f64, bool, &'static str, i32)> {
        WEATHER_CITIES
            .iter()
            .find(|c| c.slug == slug)
            .map(|c| (c.lat, c.lon, c.fahrenheit, c.timezone, c.utc_offset_hours))
    }
}

/// Look up a weather city by slug.
#[allow(dead_code)]
pub fn weather_city_by_slug(slug: &str) -> Option<&'static WeatherCity> {
    WEATHER_CITIES.iter().find(|c| c.slug == slug)
}

pub fn is_btc_5min_market(market: &BotMarket) -> bool {
    let slug = market.market_slug.to_lowercase();
    // Deterministic slug format: btc-updown-5m-{timestamp}
    slug.starts_with("btc-updown-5m")
}

/// Parse temperature bounds from a market question.
///
/// Examples:
///   "... be 31°F or below ..." → Some((NEG_INFINITY, 31.0))
///   "... be between 32-33°F ..." → Some((32.0, 33.0))
///   "... be 46°F or higher ..." → Some((46.0, INFINITY))
fn parse_temp_bucket(question: &str) -> Option<(f64, f64)> {
    let q = question.to_lowercase();

    // Find the temperature clause between "be" and "on"
    let be_pos = q.find(" be ")? + 4;
    let on_pos = q.rfind(" on ")?;
    if be_pos >= on_pos {
        return None;
    }
    let temp_part = &question[be_pos..on_pos].trim().to_lowercase();

    // Extract numbers from the temperature part (skip year-like numbers)
    let nums: Vec<f64> = extract_numbers(temp_part)
        .into_iter()
        .filter(|&n| n < 200.0) // Filter out year (2026) if present
        .collect();

    if temp_part.contains("or below") || temp_part.contains("or lower") {
        Some((f64::NEG_INFINITY, *nums.first()?))
    } else if temp_part.contains("or above") || temp_part.contains("or higher") {
        Some((*nums.first()?, f64::INFINITY))
    } else if nums.len() >= 2 {
        // Range like "between 32-33°F"
        let lo = nums[nums.len() - 2];
        let hi = nums[nums.len() - 1];
        if hi > lo { Some((lo, hi)) } else { None }
    } else if nums.len() == 1 {
        // Single exact degree (Celsius markets: "be 12°C")
        let val = nums[0];
        Some((val, val))
    } else {
        None
    }
}

/// Extract all numbers from a string, including negative numbers.
///
/// A `-` is treated as a negative sign only when it starts a new number
/// (current accumulator is empty and next char is a digit).
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
