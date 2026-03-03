mod binance;
mod config;
mod polymarket;
mod risk;
mod strategies;
mod telegram;
mod weather;

use std::sync::Arc;
use std::time::Duration;

use config::Config;
use polymarket::client::PolyClient;
use polymarket::market_finder::MarketFinder;
use polymarket::ws::PolyWs;
use polymarket_client_sdk::types::{Decimal, U256};
use strategies::StrategyAction;
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
    telegram.alert_startup().await;

    let risk_manager = Arc::new(risk::RiskManager::new(config.clone()));

    tracing::info!("Bot initialized, alert_only={}", config.alert_only);

    // Init Polymarket client
    let poly_client = Arc::new(PolyClient::new(&config.private_key, &config.poly_api_url).await?);

    // Init Binance feed — returns a watch receiver for real-time price signals
    let (binance, price_rx) = binance::BinanceFeed::new();
    let binance = Arc::new(binance);
    let binance_clone = binance.clone();
    tokio::spawn(async move {
        binance_clone.run().await;
    });

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

    // Market refresh loop is spawned after PolyWs creation (needs ws_client for BTC token subscriptions)

    // Action channel — PolyWs, settlement logger, and weather push actions here
    let (action_tx, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<StrategyAction>();

    // PolyWs — real-time price stream via WebSocket best_bid_ask
    // Subscribe to BTC 5-min markets + weather markets for real-time prices
    let mut initial_markets = market_finder.btc_5min_markets().await;
    let weather_bot_markets: Vec<_> = market_finder
        .weather_markets()
        .await
        .into_iter()
        .map(|wm| wm.market)
        .collect();
    tracing::info!(
        "WS subscription: {} BTC + {} weather tokens",
        initial_markets.len(),
        weather_bot_markets.len(),
    );
    initial_markets.extend(weather_bot_markets);
    let poly_ws = PolyWs::new(
        &initial_markets,
        action_tx.clone(),
        config.clone(),
        market_finder.clone(),
    );

    // Get shared handles BEFORE poly_ws.run() consumes it
    let shared_best_asks = poly_ws.best_asks();
    let ws_client = poly_ws.ws_client();

    // Seed shared_best_asks with REST midpoints for initial BTC tokens.
    // WS only sends deltas (no initial snapshot), so without this,
    // the settlement logger shows Up=- Down=- until someone trades.
    {
        let btc_markets = market_finder.btc_5min_markets().await;
        let mut seeded = 0u32;
        for market in &btc_markets {
            for token_id in [market.yes_token_id, market.no_token_id] {
                match poly_client.get_midpoint(token_id).await {
                    Ok(mid) => {
                        if let Ok(mut asks) = shared_best_asks.write() {
                            asks.insert(token_id, mid);
                            seeded += 1;
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Failed to seed midpoint for {token_id}: {e}");
                    }
                }
            }
        }
        tracing::info!("Seeded {seeded} BTC token midpoints from REST");
    }

    // Market refresh loop — every 300s, subscribes new BTC tokens to WS + seeds midpoints
    {
        let mf_clone = market_finder.clone();
        let tg_clone = telegram.clone();
        let ws_clone = ws_client.clone();
        let shared_asks = shared_best_asks.clone();
        let pc = poly_client.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300));
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
                // SDK deduplicates — re-subscribing existing tokens is a no-op.
                let btc_markets = mf_clone.btc_5min_markets().await;
                let tokens: Vec<U256> = btc_markets
                    .iter()
                    .flat_map(|m| [m.yes_token_id, m.no_token_id])
                    .collect();
                if !tokens.is_empty() {
                    for chunk in tokens.chunks(100) {
                        if let Err(e) = ws_clone.subscribe_best_bid_ask(chunk.to_vec()) {
                            tracing::warn!("Failed to subscribe BTC tokens to WS: {e}");
                        }
                    }
                    tracing::debug!("Re-subscribed {} BTC tokens to WS", tokens.len());
                }

                // Seed shared_best_asks for new tokens that WS hasn't sent events for.
                // WS sends deltas only — new markets with no activity have no prices.
                for market in &btc_markets {
                    for token_id in [market.yes_token_id, market.no_token_id] {
                        let needs_seed = shared_asks
                            .read()
                            .map(|asks| !asks.contains_key(&token_id))
                            .unwrap_or(true);
                        if needs_seed {
                            if let Ok(mid) = pc.get_midpoint(token_id).await {
                                if let Ok(mut asks) = shared_asks.write() {
                                    asks.insert(token_id, mid);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    let tg_ws = telegram.clone();
    tokio::spawn(async move {
        poly_ws.run().await;
        // If run() returns, the WS connection died permanently
        tracing::error!("PolyWs exited unexpectedly");
        tg_ws
            .alert_error("poly_ws", "WebSocket connection died permanently")
            .await;
    });

    // Settlement logger — logs final-seconds prices for each 5-min window
    // Reads BTC price from Binance watch channel + token best_asks from PolyWs
    let settlement_logger = SettlementLogger::new(
        price_rx,
        shared_best_asks.clone(),
        market_finder.clone(),
        action_tx.clone(),
    );
    tokio::spawn(async move {
        settlement_logger.run().await;
    });

    // Weather strategy — scans for forecast vs market price edges
    let weather_fetcher = Arc::new(WeatherFetcher::new());
    let weather_strategy = WeatherStrategy::new(
        weather_fetcher,
        market_finder.clone(),
        shared_best_asks,
        action_tx,
        config.clone(),
        ws_client,
    );
    tokio::spawn(async move {
        weather_strategy.run().await;
    });

    tracing::info!("Entering main loop...");

    loop {
        tokio::select! {
            action = action_rx.recv() => {
                match action {
                    Some(a) => process_actions(&[a], &telegram, &config, &poly_client, &risk_manager).await,
                    None => {
                        tracing::error!("All action senders dropped — all strategy tasks must have died");
                        telegram.send_silent("🚨 <b>FATAL</b>: All strategy tasks died — shutting down").await;
                        break;
                    }
                }
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
                        risk::RiskDecision::Rejected(reason) => {
                            tracing::warn!("Order rejected by risk manager: {reason}");
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
                if config.alert_only {
                    // Paper trade: complement arb needs equal share counts on both sides.
                    // One side pays $1/share at settlement → profit = shares - total_cost.
                    let max_a = (*size_usdc / *token_a_price).trunc_with_scale(2);
                    let max_b = (*size_usdc / *token_b_price).trunc_with_scale(2);
                    let shares = max_a.min(max_b);
                    let cost_a = *token_a_price * shares;
                    let cost_b = *token_b_price * shares;
                    let total_cost = cost_a + cost_b;
                    let profit = shares - total_cost;
                    tracing::info!(
                        "PAPER ARB: {question} — {shares} shares @ ${token_a_price}+${token_b_price} = ${total_cost} (profit=${profit})"
                    );
                } else {
                    process_arb_execute(
                        poly_client,
                        risk_manager,
                        telegram,
                        *token_a_id,
                        *token_b_id,
                        *token_a_price,
                        *token_b_price,
                        *size_usdc,
                        condition_id,
                        question,
                    )
                    .await;
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
