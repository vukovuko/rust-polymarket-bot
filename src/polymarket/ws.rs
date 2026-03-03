use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use polymarket_client_sdk::clob::ws::{BestBidAsk, Client as WsClient, NewMarket};
use polymarket_client_sdk::types::{Decimal, U256};
use tokio::sync::mpsc;

/// Shared best-ask prices, readable by other components (e.g. settlement logger).
pub type SharedBestAsks = Arc<RwLock<HashMap<U256, Decimal>>>;

use crate::config::Config;
use crate::polymarket::market_finder::MarketFinder;
use crate::polymarket::types::BotMarket;
use crate::strategies::StrategyAction;

const ALERT_COOLDOWN: Duration = Duration::from_secs(300);
/// Max tokens per subscribe call. Community reports suggest ~200-500 is safe;
/// we stay conservative. The SDK ref-counts, so multiple calls just add more assets.
const SUBSCRIBE_BATCH_SIZE: usize = 100;

/// Maps a token_id to its market context.
#[derive(Debug, Clone)]
struct TokenInfo {
    condition_id: String,
    question: String,
    complement_token_id: U256,
}

/// Real-time WebSocket manager for Polymarket.
///
/// Subscribes to `best_bid_ask` for all active markets and detects
/// arb opportunities on every price update (sub-millisecond).
pub struct PolyWs {
    ws_client: WsClient,
    token_info: HashMap<U256, TokenInfo>,
    best_asks: SharedBestAsks,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
    config: Arc<Config>,
    market_finder: Arc<MarketFinder>,
}

impl PolyWs {
    /// Build from an initial set of markets (from REST scan).
    pub fn new(
        markets: &[BotMarket],
        action_tx: mpsc::UnboundedSender<StrategyAction>,
        config: Arc<Config>,
        market_finder: Arc<MarketFinder>,
    ) -> Self {
        let ws_client = WsClient::default();
        let token_info = build_token_info(markets);

        tracing::info!(
            "PolyWs: built token map for {} markets ({} tokens)",
            markets.len(),
            token_info.len(),
        );

        PolyWs {
            ws_client,
            token_info,
            best_asks: Arc::new(RwLock::new(HashMap::new())),
            action_tx,
            config,
            market_finder,
        }
    }

    /// Get a handle to the shared best_asks map.
    /// Call this BEFORE `run()` (which consumes self).
    pub fn best_asks(&self) -> SharedBestAsks {
        self.best_asks.clone()
    }

    /// Get a clone of the WsClient for dynamic subscriptions.
    /// Other components (e.g. weather strategy) can call `subscribe_best_bid_ask`
    /// on this clone to add new tokens — events flow to the same shared connection
    /// and appear in the existing BBA stream's `shared_best_asks`.
    pub fn ws_client(&self) -> WsClient {
        self.ws_client.clone()
    }

    /// Run the WebSocket streams. Spawns tasks for best_bid_ask and new_market.
    /// This method never returns under normal operation.
    pub async fn run(self) {
        let token_ids: Vec<U256> = self.token_info.keys().copied().collect();

        if token_ids.is_empty() {
            tracing::warn!("PolyWs: no tokens to subscribe to — exiting");
            return;
        }

        // Subscribe to best_bid_ask in batches to respect server limits
        let total = token_ids.len();
        for chunk in token_ids.chunks(SUBSCRIBE_BATCH_SIZE) {
            if let Err(e) = self.ws_client.subscribe_best_bid_ask(chunk.to_vec()) {
                tracing::error!("PolyWs: failed to subscribe to best_bid_ask batch: {e}");
                return;
            }
        }

        // Final subscription call to get the stream
        // (SDK ref-counts, so re-subscribing the full list just returns a stream
        // over all previously registered assets)
        let bba_stream = match self.ws_client.subscribe_best_bid_ask(token_ids.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("PolyWs: failed to get best_bid_ask stream: {e}");
                return;
            }
        };

        tracing::info!(
            "PolyWs: subscribed to best_bid_ask for {} tokens (in {} batches)",
            total,
            (total + SUBSCRIBE_BATCH_SIZE - 1) / SUBSCRIBE_BATCH_SIZE,
        );

        let token_info = Arc::new(self.token_info);
        let config = self.config.clone();
        let action_tx = self.action_tx.clone();
        let best_asks = self.best_asks.clone();

        // Task 1: best_bid_ask stream — hot path, arb detection
        // best_asks is shared via std::sync::RwLock (sub-microsecond lock, no async overhead)
        let ti = token_info.clone();
        let cfg = config.clone();
        let tx = action_tx.clone();
        let ba = best_asks.clone();
        let bba_handle = tokio::spawn(async move {
            run_bba_stream(bba_stream, ti, cfg, tx, ba).await;
        });

        // Task 2: new_market stream — rare events (non-fatal if subscription fails)
        let mf = self.market_finder.clone();
        let nm_handle = match self.ws_client.subscribe_new_markets(token_ids) {
            Ok(nm_stream) => {
                tracing::info!("PolyWs: subscribed to new_markets");
                Some(tokio::spawn(async move {
                    run_new_market_stream(nm_stream, mf).await;
                }))
            }
            Err(e) => {
                tracing::warn!("PolyWs: failed to subscribe to new_markets: {e}");
                None
            }
        };

        // Wait for either task to finish (shouldn't happen normally)
        tokio::select! {
            r = bba_handle => {
                tracing::error!("PolyWs: best_bid_ask task exited: {r:?}");
            }
            r = async {
                match nm_handle {
                    Some(h) => h.await,
                    None => std::future::pending().await,
                }
            } => {
                tracing::error!("PolyWs: new_market task exited: {r:?}");
            }
        }
    }
}

/// Process the best_bid_ask stream, updating prices and detecting arbs.
/// best_asks is shared via std::sync::RwLock so the settlement logger can read prices.
/// Write lock is held for a single HashMap insert (~nanoseconds), no contention issues.
async fn run_bba_stream(
    stream: impl futures_util::Stream<Item = Result<BestBidAsk, polymarket_client_sdk::error::Error>>,
    token_info: Arc<HashMap<U256, TokenInfo>>,
    config: Arc<Config>,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
    shared_best_asks: SharedBestAsks,
) {
    let mut stream = Box::pin(stream);
    let mut local_best_asks: HashMap<U256, Decimal> = HashMap::new();
    let mut known_opps: HashMap<String, Instant> = HashMap::new();
    let mut event_count: u64 = 0;
    let start = Instant::now();

    while let Some(result) = stream.next().await {
        let bba = match result {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("PolyWs: BestBidAsk stream error: {e}");
                continue;
            }
        };

        event_count += 1;
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

        // 1. Update best_ask for this token (local fast copy + shared for logger)
        local_best_asks.insert(bba.asset_id, bba.best_ask);
        if let Ok(mut shared) = shared_best_asks.write() {
            shared.insert(bba.asset_id, bba.best_ask);
        }

        // 2. Look up complement and check for arb
        let info = match token_info.get(&bba.asset_id) {
            Some(i) => i,
            None => continue,
        };

        let complement_ask = match local_best_asks.get(&info.complement_token_id) {
            Some(&a) => a,
            None => continue, // Haven't seen complement yet
        };

        // 3. Arb check: combined < (1.0 - threshold)
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

    tracing::warn!("PolyWs: BestBidAsk stream ended after {event_count} events");
}

/// Process new_market events — add to market finder for instant discovery.
async fn run_new_market_stream(
    stream: impl futures_util::Stream<Item = Result<NewMarket, polymarket_client_sdk::error::Error>>,
    market_finder: Arc<MarketFinder>,
) {
    let mut stream = Box::pin(stream);

    while let Some(result) = stream.next().await {
        let nm = match result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("PolyWs: NewMarket stream error: {e}");
                continue;
            }
        };

        tracing::info!(
            "PolyWs: new market via WebSocket: \"{}\" (assets: {})",
            nm.question,
            nm.asset_ids.len(),
        );

        // Build a BotMarket from the NewMarket event if it's binary (2 tokens)
        if let Some(bot_market) = bot_market_from_new_market(&nm) {
            market_finder.add_market(bot_market).await;
        }
    }

    tracing::warn!("PolyWs: NewMarket stream ended");
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
fn bot_market_from_new_market(nm: &NewMarket) -> Option<BotMarket> {
    if nm.asset_ids.len() != 2 || nm.outcomes.len() != 2 {
        return None;
    }

    let (yes_idx, no_idx) = if nm.outcomes[0].to_lowercase() == "yes" {
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
