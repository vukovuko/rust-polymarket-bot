mod binance;
mod config;
mod health;
mod polymarket;
mod risk;
mod strategies;
mod telegram;
mod weather;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use health::BotHealth;
use polymarket::client::PolyClient;
use polymarket::market_finder::MarketFinder;
use polymarket::ws::PolyWs;
use polymarket_client_sdk::types::{Decimal, U256};
use strategies::StrategyAction;
use strategies::paper_tracker::PaperTracker;
use strategies::settlement_logger::SettlementLogger;
use strategies::weather::WeatherStrategy;
use telegram::TelegramSender;
use tracing_subscriber::EnvFilter;
use weather::WeatherFetcher;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    // Suppress SDK serde warnings (feeType "crypto_15_min" spam)
    // while still respecting RUST_LOG for everything else
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive(
            "polymarket_client_sdk::serde_helpers=error"
                .parse()
                .unwrap(),
        );
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let config = Arc::new(Config::from_env()?);
    config.log_summary();

    let telegram = Arc::new(TelegramSender::new(
        config.tg_bot_token.as_deref(),
        config.tg_chat_id.as_deref(),
    ));
    telegram.alert_startup(config.alert_only).await;

    let risk_manager = Arc::new(risk::RiskManager::new(config.clone()));
    let health = Arc::new(BotHealth::new());
    let paper_tracker =
        Arc::new(PaperTracker::new().expect("Failed to initialize paper tracker CSV files"));

    tracing::info!("Bot initialized, alert_only={}", config.alert_only);

    // Init Polymarket client
    let poly_client = Arc::new(PolyClient::new(&config.private_key, &config.poly_api_url).await?);

    // Init Binance feed — returns a watch receiver for real-time price signals
    let (binance, price_rx) = binance::BinanceFeed::new();
    let binance = Arc::new(binance);

    // Init market finder
    let market_finder = Arc::new(MarketFinder::new(poly_client.clone()));

    // Initial market scan
    if let Err(e) = market_finder.refresh().await {
        tracing::error!("Initial market scan failed: {e}");
        telegram
            .alert_error("startup", &format!("Market scan failed: {e}"))
            .await;
    }

    // Initial weather market scan
    if let Err(e) = market_finder.refresh_weather().await {
        tracing::warn!("Initial weather market scan failed: {e}");
    }

    // Action channel — PolyWs, settlement logger, and weather push actions here
    let (action_tx, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<StrategyAction>();

    // PolyWs — real-time price stream via WebSocket best_bid_ask
    // Fetches current BTC + weather markets from market_finder on connect (and reconnect)
    let poly_ws = PolyWs::new(
        action_tx.clone(),
        config.clone(),
        market_finder.clone(),
        health.clone(),
    );

    // Get shared handles BEFORE poly_ws.run() consumes it
    let shared_best_asks = poly_ws.best_asks();
    let ws_rx = poly_ws.ws_receiver();
    let shared_best_asks_hb = shared_best_asks.clone(); // for heartbeat

    // Task handles for health monitoring — detect silent task death
    let mut task_handles: Vec<(&'static str, tokio::task::JoinHandle<()>)> = Vec::new();

    // Binance feed
    {
        let binance_clone = binance.clone();
        task_handles.push((
            "Binance feed",
            tokio::spawn(async move {
                binance_clone.run().await;
            }),
        ));
    }

    // Market refresh loop — subscribes new BTC tokens to WS
    {
        let mf_clone = market_finder.clone();
        let tg_clone = telegram.clone();
        let ws_rx_clone = ws_rx.clone();
        let refresh_secs = config.market_refresh_interval_secs;
        task_handles.push((
            "Market refresh",
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(refresh_secs));
                interval.tick().await; // skip immediate tick
                loop {
                    interval.tick().await;
                    if let Err(e) = mf_clone.refresh().await {
                        tracing::error!("Market refresh failed: {e}");
                        tg_clone
                            .alert_error("market_refresh", &format!("{e}"))
                            .await;
                        continue;
                    }

                    // Subscribe current BTC tokens to WS so settlement logger gets prices.
                    // Uses watch receiver to get latest WsClient (survives reconnections).
                    // SDK deduplicates — re-subscribing existing tokens is a no-op.
                    let btc_markets = mf_clone.btc_5min_markets().await;
                    let tokens: Vec<U256> = btc_markets
                        .iter()
                        .flat_map(|m| [m.yes_token_id, m.no_token_id])
                        .collect();
                    if !tokens.is_empty() {
                        let ws = ws_rx_clone.borrow().clone();
                        for chunk in tokens.chunks(100) {
                            if let Err(e) = ws.subscribe_best_bid_ask(chunk.to_vec()) {
                                tracing::warn!("Failed to subscribe BTC tokens to WS: {e}");
                            }
                        }
                        tracing::debug!("Re-subscribed {} BTC tokens to WS", tokens.len());
                    }
                }
            }),
        ));
    }

    // PolyWs (fatal — separate handle, monitored in main select loop)
    let mut ws_handle = tokio::spawn(async move {
        // run() has an internal reconnection loop — it never returns normally
        poly_ws.run().await;
    });

    // Settlement logger — logs final-seconds prices for each 5-min window
    // Reads BTC price from Binance watch channel + token best_asks from PolyWs
    {
        let settlement_logger = SettlementLogger::new(
            price_rx,
            shared_best_asks.clone(),
            market_finder.clone(),
            action_tx.clone(),
        );
        task_handles.push((
            "Settlement logger",
            tokio::spawn(async move {
                settlement_logger.run().await;
            }),
        ));
    }

    // Weather strategy — scans for forecast vs market price edges
    {
        let weather_fetcher = Arc::new(WeatherFetcher::new());
        let weather_strategy = WeatherStrategy::new(
            weather_fetcher,
            market_finder.clone(),
            shared_best_asks,
            action_tx,
            config.clone(),
            ws_rx,
            health.clone(),
            paper_tracker.clone(),
        );
        task_handles.push((
            "Weather strategy",
            tokio::spawn(async move {
                weather_strategy.run().await;
            }),
        ));
    }

    tracing::info!("Entering main loop...");

    // Heartbeat timer — sends health summary to Telegram every 2 hours
    let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(2 * 3600));
    heartbeat_interval.tick().await; // skip immediate tick

    // Fast task death check — every 60s
    let mut task_check_interval = tokio::time::interval(Duration::from_secs(60));
    task_check_interval.tick().await; // skip immediate tick

    let mut dead_tasks: HashSet<&str> = HashSet::new();

    loop {
        tokio::select! {
            action = action_rx.recv() => {
                match action {
                    Some(a) => process_actions(&[a], &telegram, &config, &poly_client, &risk_manager, &paper_tracker).await,
                    None => {
                        tracing::error!("All action senders dropped — all strategy tasks must have died");
                        telegram.send_silent("🚨 <b>FATAL</b>: All strategy tasks died — shutting down").await;
                        break;
                    }
                }
            }
            result = &mut ws_handle => {
                tracing::error!("PolyWs task died: {result:?}");
                telegram.send_silent("🚨 <b>FATAL</b>: WebSocket task died — shutting down").await;
                break;
            }
            _ = task_check_interval.tick() => {
                // Check for newly dead tasks every 60s
                for (name, handle) in &task_handles {
                    if handle.is_finished() && dead_tasks.insert(*name) {
                        tracing::error!("{name} task died");
                        telegram
                            .send_silent(&format!("⚠️ <b>{name}</b> task died — bot partially degraded"))
                            .await;
                    }
                }
            }
            _ = heartbeat_interval.tick() => {
                // Check for resolved weather outcomes before building heartbeat
                let outcome_msgs = paper_tracker.check_weather_outcomes().await;
                for msg in &outcome_msgs {
                    telegram.send_silent(msg).await;
                }
                if !outcome_msgs.is_empty() {
                    tracing::info!("Weather: {} outcomes resolved", outcome_msgs.len());
                }

                // Send health summary
                let s = health.summary();
                let btc = market_finder.btc_5min_markets().await.len();
                let wx = market_finder.weather_markets().await.len();
                let prices = shared_best_asks_hb.read().map(|a| a.len()).unwrap_or(0);

                let ws_age = s
                    .ws_age
                    .map(health::format_duration)
                    .unwrap_or_else(|| "-".into());
                let ws_last = s
                    .ws_last_event_ago
                    .map(|d| format!("{}s ago", d.as_secs()))
                    .unwrap_or_else(|| "-".into());
                let wx_last = s
                    .weather_last_scan_ago
                    .map(health::format_duration)
                    .unwrap_or_else(|| "-".into());

                let tasks_status = if dead_tasks.is_empty() {
                    "all alive".to_string()
                } else {
                    format!(
                        "DEAD: {}",
                        dead_tasks.iter().copied().collect::<Vec<_>>().join(", ")
                    )
                };

                let paper_pnl = paper_tracker.daily_summary();

                let msg = format!(
                    "💓 <b>Health</b> — {}\n\
                     Uptime: {}\n\
                     WS: age {ws_age}, last event {ws_last}, {} reconnects, {}K events\n\
                     Markets: {btc} BTC, {wx} weather, {prices} priced\n\
                     Weather: {} scans, {} edges today, last {wx_last}\n\
                     Tasks: {tasks_status}\n\
                     \n📊 <b>Paper P&L</b>\n\
                     {paper_pnl}",
                    chrono::Utc::now().format("%H:%M UTC"),
                    health::format_duration(s.uptime),
                    s.ws_reconnects,
                    s.ws_events_total / 1000,
                    s.weather_scans_today,
                    s.weather_edges_today,
                );

                telegram.send_silent(&msg).await;
                tracing::info!("Heartbeat sent to Telegram");
            }
            _ = shutdown_signal() => {
                tracing::info!("Shutdown signal received...");
                break;
            }
        }
    }

    telegram.send_silent("🛑 Bot shutting down.").await;
    tracing::info!("Goodbye.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = ctrl_c => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.ok();
    }
}

async fn process_actions(
    actions: &[StrategyAction],
    telegram: &TelegramSender,
    config: &Config,
    poly_client: &PolyClient,
    risk_manager: &risk::RiskManager,
    paper_tracker: &PaperTracker,
) {
    for action in actions {
        match action {
            StrategyAction::Alert(msg) => {
                telegram.send_silent(msg).await;
            }
            StrategyAction::PlaceOrder {
                token_id,
                price,
                size,
                reason,
            } => {
                if config.alert_only {
                    tracing::info!(
                        "PAPER TRADE: {reason} — token={token_id} price={price} size={size}"
                    );
                } else {
                    let cost_usdc = *price * *size;
                    match risk_manager.check_trade(cost_usdc).await {
                        risk::RiskDecision::Approved => {}
                        risk::RiskDecision::Rejected(ref reject_reason) => {
                            tracing::warn!("Order rejected by risk manager: {reject_reason}");
                            telegram
                                .send_silent(&format!(
                                    "⚠️ <b>Order Rejected</b>\n{reason}\nRisk: {reject_reason}"
                                ))
                                .await;
                            continue;
                        }
                        risk::RiskDecision::KillSwitch(reason) => {
                            tracing::error!("Kill switch triggered: {reason}");
                            telegram
                                .send_silent(&format!("🚨 <b>KILL SWITCH</b>: {reason}"))
                                .await;
                            if let Err(e) = poly_client.cancel_all().await {
                                tracing::error!("Failed to cancel all orders: {e}");
                            }
                            continue;
                        }
                    }

                    match poly_client.place_limit_buy(*token_id, *price, *size).await {
                        Ok(result) if result.success => {
                            risk_manager
                                .record_order(risk::TrackedPosition {
                                    order_id: result.order_id.clone(),
                                    token_id: token_id.to_string(),
                                    price: *price,
                                    size: *size,
                                    cost_usdc,
                                    reason: reason.clone(),
                                    placed_at: chrono::Utc::now(),
                                })
                                .await;
                            let (exp, pnl, pos) = risk_manager.stats().await;
                            telegram
                                .send_silent(&format!(
                                    "✅ <b>Order Placed</b>\n\
                                     {reason}\n\
                                     Token: {token_id}\n\
                                     Price: ${price} × {size} shares = ${cost_usdc}\n\
                                     Order ID: {}\n\
                                     Risk: exp=${exp} pnl=${pnl} positions={pos}",
                                    result.order_id,
                                ))
                                .await;
                        }
                        Ok(result) => {
                            let err = result.error_msg.as_deref().unwrap_or("unknown");
                            tracing::error!("Order failed: {err}");
                            telegram
                                .send_silent(&format!(
                                    "❌ <b>Order Failed</b>\n{reason}\nError: {err}"
                                ))
                                .await;
                        }
                        Err(e) => {
                            tracing::error!("Order placement error: {e}");
                            telegram
                                .send_silent(&format!(
                                    "❌ <b>Order Error</b>\n{reason}\nError: {e}"
                                ))
                                .await;
                        }
                    }
                }
            }
            StrategyAction::ArbExecute {
                token_a_id,
                token_b_id,
                token_a_price,
                token_b_price,
                size_usdc,
                condition_id,
                question,
            } => {
                // Always paper-trade arbs regardless of alert_only mode.
                // Arb strategy is confirmed non-viable: 99.7% phantom (WS bid-ask bounce).
                // Real orders would waste money and consume position slots.
                {
                    // Paper trade: complement arb needs equal share counts on both sides.
                    // One side pays $1/share at settlement → profit = shares - total_cost.
                    let max_a = (*size_usdc / *token_a_price).trunc_with_scale(2);
                    let max_b = (*size_usdc / *token_b_price).trunc_with_scale(2);
                    let shares = max_a.min(max_b);
                    let cost_a = *token_a_price * shares;
                    let cost_b = *token_b_price * shares;
                    let total_cost = cost_a + cost_b;
                    let profit = shares - total_cost;

                    // Shadow execution: verify arb via REST before logging
                    let (book_a, book_b) = tokio::join!(
                        poly_client.get_best_ask(*token_a_id),
                        poly_client.get_best_ask(*token_b_id),
                    );

                    let (verified, rest_a, rest_b, depth_a, depth_b) = match (book_a, book_b) {
                        (Ok((ask_a, dep_a)), Ok((ask_b, dep_b))) => {
                            let rest_combined = ask_a + ask_b;
                            let threshold = Decimal::ONE - config.arb_threshold;
                            (rest_combined < threshold, Some(ask_a), Some(ask_b), Some(dep_a), Some(dep_b))
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            tracing::warn!("Shadow exec: REST fetch failed: {e}");
                            (false, None, None, None, None)
                        }
                    };

                    let label = if verified { "VERIFIED" } else { "PHANTOM" };
                    tracing::info!(
                        "PAPER ARB [{label}]: {question} — {shares} shares @ ${token_a_price}+${token_b_price} = ${total_cost} (profit=${profit})"
                    );
                    let msg = paper_tracker.record_arb(
                        condition_id,
                        question,
                        *token_a_price,
                        *token_b_price,
                        shares,
                        total_cost,
                        profit,
                        rest_a,
                        rest_b,
                        depth_a,
                        depth_b,
                        verified,
                    );
                    telegram.send_silent(&msg).await;
                }
            }
            StrategyAction::CancelAllOrders => {
                tracing::warn!("Kill switch triggered — cancelling all orders");
                telegram
                    .send_silent("🚨 <b>KILL SWITCH</b> — cancelling all orders")
                    .await;
                if let Err(e) = poly_client.cancel_all().await {
                    tracing::error!("Failed to cancel all orders: {e}");
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_arb_execute(
    poly_client: &PolyClient,
    risk_manager: &risk::RiskManager,
    telegram: &TelegramSender,
    token_a_id: U256,
    token_b_id: U256,
    token_a_price: Decimal,
    token_b_price: Decimal,
    size_usdc: Decimal,
    condition_id: &str,
    question: &str,
) {
    // Complement arb: equal share counts on both sides.
    let max_a = (size_usdc / token_a_price).trunc_with_scale(2);
    let max_b = (size_usdc / token_b_price).trunc_with_scale(2);
    let shares = max_a.min(max_b);
    let cost_a = token_a_price * shares;
    let cost_b = token_b_price * shares;
    let total_cost = cost_a + cost_b;

    match risk_manager.check_arb_trade(cost_a, cost_b).await {
        risk::RiskDecision::Approved => {}
        risk::RiskDecision::Rejected(reason) => {
            tracing::warn!("Arb rejected by risk manager: {reason}");
            return;
        }
        risk::RiskDecision::KillSwitch(reason) => {
            tracing::error!("Kill switch triggered during arb: {reason}");
            telegram
                .send_silent(&format!("🚨 <b>KILL SWITCH</b>: {reason}"))
                .await;
            if let Err(e) = poly_client.cancel_all().await {
                tracing::error!("Failed to cancel all orders: {e}");
            }
            return;
        }
    }

    // Place both sides
    let result_a = poly_client
        .place_limit_buy(token_a_id, token_a_price, shares)
        .await;
    let result_b = poly_client
        .place_limit_buy(token_b_id, token_b_price, shares)
        .await;

    match (&result_a, &result_b) {
        (Ok(a), Ok(b)) if a.success && b.success => {
            // Both succeeded
            let now = chrono::Utc::now();
            risk_manager
                .record_order(risk::TrackedPosition {
                    order_id: a.order_id.clone(),
                    token_id: token_a_id.to_string(),
                    price: token_a_price,
                    size: shares,
                    cost_usdc: cost_a,
                    reason: format!("arb-A {condition_id}"),
                    placed_at: now,
                })
                .await;
            risk_manager
                .record_order(risk::TrackedPosition {
                    order_id: b.order_id.clone(),
                    token_id: token_b_id.to_string(),
                    price: token_b_price,
                    size: shares,
                    cost_usdc: cost_b,
                    reason: format!("arb-B {condition_id}"),
                    placed_at: now,
                })
                .await;
            let profit = shares - total_cost;
            let (exp, pnl, pos) = risk_manager.stats().await;
            telegram
                .send_silent(&format!(
                    "✅ <b>Arb Executed</b>\n\
                     {question}\n\
                     Side A: {shares}@${token_a_price} (order {})\n\
                     Side B: {shares}@${token_b_price} (order {})\n\
                     Total: ${total_cost} → est profit ${profit}\n\
                     Risk: exp=${exp} pnl=${pnl} positions={pos}",
                    a.order_id, b.order_id,
                ))
                .await;
        }
        _ => {
            // At least one failed — cancel the successful one
            let a_ok = result_a.as_ref().is_ok_and(|r| r.success);
            let b_ok = result_b.as_ref().is_ok_and(|r| r.success);

            if a_ok {
                let order_id = &result_a.as_ref().unwrap().order_id;
                tracing::warn!("Arb: side B failed, cancelling side A order {order_id}");
                if let Err(e) = poly_client.cancel_order(order_id).await {
                    tracing::error!("Failed to cancel side A: {e}");
                }
            }
            if b_ok {
                let order_id = &result_b.as_ref().unwrap().order_id;
                tracing::warn!("Arb: side A failed, cancelling side B order {order_id}");
                if let Err(e) = poly_client.cancel_order(order_id).await {
                    tracing::error!("Failed to cancel side B: {e}");
                }
            }

            let err_a = match &result_a {
                Ok(r) if !r.success => r.error_msg.clone().unwrap_or("rejected".into()),
                Err(e) => e.to_string(),
                _ => "ok".into(),
            };
            let err_b = match &result_b {
                Ok(r) if !r.success => r.error_msg.clone().unwrap_or("rejected".into()),
                Err(e) => e.to_string(),
                _ => "ok".into(),
            };

            telegram
                .send_silent(&format!(
                    "❌ <b>Arb Failed</b>\n\
                     {question}\n\
                     Side A: {err_a}\n\
                     Side B: {err_b}\n\
                     (successful side cancelled)",
                ))
                .await;
        }
    }
}
