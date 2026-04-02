use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use polymarket_client_sdk::clob::ws::{BestBidAsk, Client as WsClient, NewMarket};
use polymarket_client_sdk::types::{Decimal, U256};
use tokio::sync::mpsc;

/// Shared best-ask prices with timestamps, readable by other components (e.g. settlement logger).
pub type SharedBestAsks = Arc<RwLock<HashMap<U256, (Decimal, Instant)>>>;

/// Shared WsClient via watch channel. Other components (weather strategy, market refresh)
/// read the latest WsClient after reconnections.
pub type SharedWsClient = tokio::sync::watch::Receiver<WsClient>;

use crate::config::Config;
use crate::health::BotHealth;
use crate::polymarket::market_finder::MarketFinder;
use crate::polymarket::types::BotMarket;
use crate::strategies::StrategyAction;

const ALERT_COOLDOWN: Duration = Duration::from_secs(300);
/// Max tokens per subscribe call. Community reports suggest ~200-500 is safe;
/// we stay conservative. The SDK ref-counts, so multiple calls just add more assets.
const SUBSCRIBE_BATCH_SIZE: usize = 100;

/// Force reconnection after this duration to prevent gradual degradation
/// from subscription bloat and broadcast buffer saturation.
const MAX_CONNECTION_AGE: Duration = Duration::from_secs(2 * 3600); // 2 hours

/// Reconnect if no events received for this long (dead connection).
const NO_EVENT_TIMEOUT: Duration = Duration::from_secs(120); // 2 minutes

/// Time to observe peak event rate before enabling degradation detection.
const RATE_CALIBRATION_PERIOD: Duration = Duration::from_secs(600); // 10 minutes

/// Reconnect if event rate falls below this fraction of peak for DEGRADATION_MINUTES.
const DEGRADATION_RATIO: f64 = 0.30;

/// Number of consecutive low-rate minutes before triggering reconnect.
const DEGRADATION_MINUTES: u32 = 3;

/// Minimum peak rate (events/min) to enable degradation detection.
/// Below this, the market is just quiet and we shouldn't reconnect.
const MIN_PEAK_FOR_DEGRADATION: u32 = 300; // ~5 events/sec

/// Max age for a price to be considered fresh in arb detection.
/// After this, the token is treated as "not yet seen" for arb purposes.
const ARB_PRICE_MAX_AGE: Duration = Duration::from_secs(120); // 2 minutes

/// Minimum price for arb detection. Prices below this are likely phantom/empty books.
const ARB_MIN_PRICE: &str = "0.03";

/// Maps a token_id to its market context.
#[derive(Debug, Clone)]
struct TokenInfo {
    condition_id: String,
    question: String,
    complement_token_id: U256,
}

/// Why the BBA stream exited.
enum StreamExit {
    /// Stream returned None (connection closed).
    Ended,
    /// No events received for NO_EVENT_TIMEOUT.
    Stale,
    /// Connection exceeded MAX_CONNECTION_AGE.
    MaxAge,
    /// Event rate degraded below threshold.
    Degraded,
}

/// Real-time WebSocket manager for Polymarket.
///
/// Subscribes to `best_bid_ask` for all active markets and detects
/// arb opportunities on every price update (sub-millisecond).
///
/// Includes automatic reconnection with health monitoring:
/// - Forced reconnect every 2 hours (prevents subscription bloat)
/// - Dead connection detection (no events for 2 minutes)
/// - Degradation detection (event rate drops below 30% of peak)
pub struct PolyWs {
    ws_tx: tokio::sync::watch::Sender<WsClient>,
    ws_rx: tokio::sync::watch::Receiver<WsClient>,
    best_asks: SharedBestAsks,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
    config: Arc<Config>,
    market_finder: Arc<MarketFinder>,
    health: Arc<BotHealth>,
}

impl PolyWs {
    /// Build the WS manager. Does not connect yet — connection happens in `run()`.
    pub fn new(
        action_tx: mpsc::UnboundedSender<StrategyAction>,
        config: Arc<Config>,
        market_finder: Arc<MarketFinder>,
        health: Arc<BotHealth>,
    ) -> Self {
        let ws_client = WsClient::default();
        let (ws_tx, ws_rx) = tokio::sync::watch::channel(ws_client);

        PolyWs {
            ws_tx,
            ws_rx,
            best_asks: Arc::new(RwLock::new(HashMap::new())),
            action_tx,
            config,
            market_finder,
            health,
        }
    }

    /// Get a handle to the shared best_asks map.
    /// Call this BEFORE `run()` (which consumes self).
    pub fn best_asks(&self) -> SharedBestAsks {
        self.best_asks.clone()
    }

    /// Get a watch receiver for the current WsClient.
    /// Other components can read the latest WsClient after reconnections.
    /// Call this BEFORE `run()` (which consumes self).
    pub fn ws_receiver(&self) -> SharedWsClient {
        self.ws_rx.clone()
    }

    /// Run the WebSocket streams with automatic reconnection.
    /// This method runs forever — it reconnects on stream death, degradation, or max age.
    pub async fn run(self) {
        let mut reconnect_count = 0u32;
        let mut backoff = Duration::from_secs(5);
        // Arb cooldown survives reconnections to avoid duplicate alerts
        let mut known_opps: HashMap<String, Instant> = HashMap::new();

        loop {
            // Get current markets from market_finder (fresh on each reconnect)
            let mut markets = self.market_finder.btc_5min_markets().await;
            let weather_markets: Vec<_> = self
                .market_finder
                .weather_markets()
                .await
                .into_iter()
                .map(|wm| wm.market)
                .collect();
            let btc_count = markets.len();
            let weather_count = weather_markets.len();
            markets.extend(weather_markets);

            let token_info = Arc::new(build_token_info(&markets));
            let token_ids: Vec<U256> = token_info.keys().copied().collect();

            if token_ids.is_empty() {
                tracing::warn!("PolyWs: no tokens to subscribe to — retrying in 30s");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }

            // Clear stale prices from previous connection
            if let Ok(mut asks) = self.best_asks.write() {
                asks.clear();
            }

            // Create fresh WS client (new connection, clean subscription list)
            let ws_client = WsClient::default();

            // Update shared client so weather strategy + market refresh use new connection
            let _ = self.ws_tx.send(ws_client.clone());

            // Subscribe in batches
            let total = token_ids.len();
            let mut subscribe_failed = false;
            for chunk in token_ids.chunks(SUBSCRIBE_BATCH_SIZE) {
                if let Err(e) = ws_client.subscribe_best_bid_ask(chunk.to_vec()) {
                    tracing::error!("PolyWs: subscribe batch failed: {e}");
                    subscribe_failed = true;
                    break;
                }
            }

            if subscribe_failed {
                tracing::warn!("PolyWs: subscribe failed, retrying in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(120));
                continue;
            }

            // Final subscribe call to get the stream
            let bba_stream = match ws_client.subscribe_best_bid_ask(token_ids.clone()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("PolyWs: stream creation failed: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(120));
                    continue;
                }
            };

            // Connection succeeded — reset backoff
            backoff = Duration::from_secs(5);

            if reconnect_count > 0 {
                self.health.ws_reconnected();
                tracing::info!(
                    "PolyWs: reconnected (#{reconnect_count}) — {btc_count} BTC + {weather_count} weather = {total} tokens"
                );
                let _ = self.action_tx.send(StrategyAction::Alert(format!(
                    "🔄 <b>WS Reconnected</b> (#{reconnect_count})\n\
                     {btc_count} BTC + {weather_count} weather = {total} tokens"
                )));
            } else {
                self.health.ws_connected();
                tracing::info!(
                    "PolyWs: connected — {btc_count} BTC + {weather_count} weather = {total} tokens"
                );
            }

            // NOTE: NewMarket WS stream removed — the SDK's subscribe_new_markets()
            // filters events by provided asset_ids, but new markets have NEW asset_ids
            // not in the set, so zero events ever pass through. New market discovery
            // relies on the REST refresh loop instead (market_refresh_interval_secs).

            // Run BBA stream until it exits or degrades
            let exit_reason = run_bba_stream(
                bba_stream,
                token_info,
                self.config.clone(),
                self.action_tx.clone(),
                self.best_asks.clone(),
                &mut known_opps,
                &self.health,
            )
            .await;

            reconnect_count += 1;

            match exit_reason {
                StreamExit::MaxAge => {
                    tracing::info!(
                        "PolyWs: max connection age (2h) reached, reconnecting with fresh token list..."
                    );
                }
                StreamExit::Stale => {
                    tracing::warn!("PolyWs: no events for 2min — reconnecting in 5s...");
                    let _ = self.action_tx.send(StrategyAction::Alert(
                        "⚠️ WS stale (no events 2min) — reconnecting".to_string(),
                    ));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                StreamExit::Degraded => {
                    tracing::warn!("PolyWs: event rate degraded — reconnecting in 5s...");
                    let _ = self.action_tx.send(StrategyAction::Alert(
                        "⚠️ WS degraded (low event rate) — reconnecting".to_string(),
                    ));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
                StreamExit::Ended => {
                    tracing::warn!("PolyWs: stream ended — reconnecting in 5s...");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}

/// Process the best_bid_ask stream with health monitoring.
/// Returns the reason the stream exited so the caller can reconnect.
async fn run_bba_stream(
    stream: impl futures_util::Stream<Item = Result<BestBidAsk, polymarket_client_sdk::error::Error>>,
    token_info: Arc<HashMap<U256, TokenInfo>>,
    config: Arc<Config>,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
    shared_best_asks: SharedBestAsks,
    known_opps: &mut HashMap<String, Instant>,
    health: &BotHealth,
) -> StreamExit {
    let mut stream = Box::pin(stream);
    let mut local_best_asks: HashMap<U256, (Decimal, Instant)> = HashMap::new();
    let mut event_count: u64 = 0;
    let mut stale_skip_count: u64 = 0;
    let arb_min_price: Decimal = ARB_MIN_PRICE.parse().unwrap();
    let start = Instant::now();
    let mut last_event = Instant::now();

    // Rate monitoring
    let mut events_this_minute: u32 = 0;
    let mut minute_start = Instant::now();
    let mut peak_rate: u32 = 0;
    let mut peak_calibrated = false;
    let mut low_rate_minutes: u32 = 0;

    // Timers for max-age and stale detection
    let max_age_deadline = tokio::time::Instant::now() + MAX_CONNECTION_AGE;
    let max_age_timer = tokio::time::sleep_until(max_age_deadline);
    tokio::pin!(max_age_timer);
    let mut stale_check = tokio::time::interval(Duration::from_secs(30));
    stale_check.tick().await; // skip immediate tick
    let mut last_stale_log = Instant::now();

    loop {
        tokio::select! {
            event = stream.next() => {
                let bba = match event {
                    Some(Ok(b)) => b,
                    Some(Err(e)) => {
                        tracing::warn!("PolyWs: BBA stream error: {e}");
                        continue;
                    }
                    None => return StreamExit::Ended,
                };

                last_event = Instant::now();
                event_count += 1;
                events_this_minute += 1;

                if event_count == 1 {
                    tracing::info!("PolyWs: first BestBidAsk event received");
                }
                if event_count % 10_000 == 0 {
                    let elapsed = start.elapsed().as_secs();
                    tracing::info!(
                        "PolyWs: {event_count} events processed ({:.0} events/sec)",
                        event_count as f64 / elapsed.max(1) as f64,
                    );
                }

                // Rate monitoring: check every minute
                if minute_start.elapsed() >= Duration::from_secs(60) {
                    // Calibration phase: first 10 minutes, track peak rate
                    if start.elapsed() < RATE_CALIBRATION_PERIOD {
                        peak_rate = peak_rate.max(events_this_minute);
                    } else if !peak_calibrated {
                        peak_rate = peak_rate.max(events_this_minute);
                        peak_calibrated = true;
                        tracing::info!(
                            "PolyWs: peak rate calibrated at {peak_rate} events/min ({:.0}/sec)",
                            peak_rate as f64 / 60.0,
                        );
                    }

                    // Degradation check (only after calibration + sufficient activity)
                    if peak_calibrated && peak_rate >= MIN_PEAK_FOR_DEGRADATION {
                        let threshold = (peak_rate as f64 * DEGRADATION_RATIO) as u32;
                        if events_this_minute < threshold {
                            low_rate_minutes += 1;
                            tracing::warn!(
                                "PolyWs: low rate {events_this_minute}/min vs peak {peak_rate}/min — {low_rate_minutes}/{DEGRADATION_MINUTES} consecutive"
                            );
                            if low_rate_minutes >= DEGRADATION_MINUTES {
                                return StreamExit::Degraded;
                            }
                        } else {
                            if low_rate_minutes > 0 {
                                tracing::info!(
                                    "PolyWs: rate recovered ({events_this_minute}/min, peak {peak_rate}/min)"
                                );
                            }
                            low_rate_minutes = 0;
                        }
                    }

                    health.ws_events(events_this_minute as u64);
                    events_this_minute = 0;
                    minute_start = Instant::now();
                }

                // 1. Update best_ask for this token (local fast copy + shared for logger)
                local_best_asks.insert(bba.asset_id, (bba.best_ask, Instant::now()));
                if let Ok(mut shared) = shared_best_asks.write() {
                    shared.insert(bba.asset_id, (bba.best_ask, Instant::now()));
                }

                // 2. Look up complement and check for arb
                let info = match token_info.get(&bba.asset_id) {
                    Some(i) => i,
                    None => continue,
                };

                let complement_ask = match local_best_asks.get(&info.complement_token_id) {
                    Some(&(price, seen_at)) => {
                        if seen_at.elapsed() > ARB_PRICE_MAX_AGE {
                            stale_skip_count += 1;
                            if last_stale_log.elapsed() >= Duration::from_secs(60) {
                                tracing::info!(
                                    "PolyWs: {stale_skip_count} arb checks skipped due to stale complement prices"
                                );
                                last_stale_log = Instant::now();
                            }
                            continue; // Complement price too stale for arb
                        }
                        price
                    }
                    None => continue, // Haven't seen complement yet
                };

                // 3. Arb check: combined < (1.0 - threshold)
                // Skip if either ask is zero (empty order book) or below min price (phantom)
                if bba.best_ask.is_zero() || complement_ask.is_zero()
                    || bba.best_ask < arb_min_price || complement_ask < arb_min_price
                {
                    continue;
                }
                let combined = bba.best_ask + complement_ask;
                let threshold = Decimal::ONE - config.arb_threshold;

                if combined < threshold {
                    let net_edge = Decimal::ONE - combined;

                    // Cooldown check
                    let now = Instant::now();
                    known_opps.retain(|_, t| now.duration_since(*t) < ALERT_COOLDOWN);

                    if let std::collections::hash_map::Entry::Vacant(e) =
                        known_opps.entry(info.condition_id.clone())
                    {
                        e.insert(now);

                        let msg = format!(
                            "🎯 <b>Arb Opportunity</b> (real-time)\n\
                             Market: {}\n\
                             YES ask: ${}\n\
                             NO ask: ${}\n\
                             Combined: ${combined}\n\
                             Net edge: ${net_edge} ({:.2}%)",
                            info.question,
                            bba.best_ask,
                            complement_ask,
                            net_edge * Decimal::ONE_HUNDRED,
                        );

                        let _ = action_tx.send(StrategyAction::Alert(msg));

                        // Also send arb execution action for live/paper trading
                        let _ = action_tx.send(StrategyAction::ArbExecute {
                            token_a_id: bba.asset_id,
                            token_b_id: info.complement_token_id,
                            token_a_price: bba.best_ask,
                            token_b_price: complement_ask,
                            size_usdc: config.max_trade_usd,
                            condition_id: info.condition_id.clone(),
                            question: info.question.clone(),
                        });
                    }
                }
            }
            _ = &mut max_age_timer => {
                tracing::info!(
                    "PolyWs: max connection age reached after {event_count} events ({:.0} events/sec avg)",
                    event_count as f64 / start.elapsed().as_secs().max(1) as f64,
                );
                return StreamExit::MaxAge;
            }
            _ = stale_check.tick() => {
                if last_event.elapsed() > NO_EVENT_TIMEOUT {
                    tracing::warn!(
                        "PolyWs: no events for {:.0}s — connection appears dead",
                        last_event.elapsed().as_secs_f64(),
                    );
                    return StreamExit::Stale;
                }
            }
        }
    }
}

/// Build token_info map from a list of markets.
/// Creates 2 entries per market: yes→no and no→yes.
fn build_token_info(markets: &[BotMarket]) -> HashMap<U256, TokenInfo> {
    let mut map = HashMap::with_capacity(markets.len() * 2);

    for m in markets {
        map.insert(
            m.yes_token_id,
            TokenInfo {
                condition_id: m.condition_id.clone(),
                question: m.question.clone(),
                complement_token_id: m.no_token_id,
            },
        );
        map.insert(
            m.no_token_id,
            TokenInfo {
                condition_id: m.condition_id.clone(),
                question: m.question.clone(),
                complement_token_id: m.yes_token_id,
            },
        );
    }

    map
}

/// Try to build a BotMarket from a NewMarket WebSocket event.
/// Currently unused: the SDK's subscribe_new_markets() filters by provided asset_ids,
/// so new markets (with new asset_ids) never pass through. Kept for future SDK fixes.
#[allow(dead_code)]
fn bot_market_from_new_market(nm: &NewMarket) -> Option<BotMarket> {
    if nm.asset_ids.len() != 2 || nm.outcomes.len() != 2 {
        return None;
    }

    let first = nm.outcomes[0].to_lowercase();
    let (yes_idx, no_idx) = if first == "yes" || first == "up" {
        (0, 1)
    } else {
        (1, 0)
    };

    Some(BotMarket {
        condition_id: format!("{:?}", nm.market),
        question: nm.question.clone(),
        market_slug: nm.slug.clone(),
        end_date: None, // WS event doesn't include end_date; REST backup will fill it
        yes_token_id: nm.asset_ids[yes_idx],
        no_token_id: nm.asset_ids[no_idx],
        yes_outcome: nm.outcomes[yes_idx].clone(),
        no_outcome: nm.outcomes[no_idx].clone(),
        minimum_tick_size: Decimal::new(1, 2), // Default 0.01
        minimum_order_size: Decimal::ONE,      // Default 1
        neg_risk: false,
        active: true,
        enable_order_book: true,
    })
}
