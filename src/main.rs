mod binance;
mod config;
mod polymarket;
mod strategies;
mod telegram;

use std::sync::Arc;
use std::time::Duration;

use config::Config;
use polymarket::client::PolyClient;
use polymarket::market_finder::MarketFinder;
use strategies::arb::ArbScanner;
use strategies::momentum::MomentumSniper;
use strategies::{Strategy, StrategyAction};
use telegram::TelegramSender;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env file
    dotenvy::dotenv().ok();

    // Init tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load config
    let config = Arc::new(Config::from_env()?);
    config.log_summary();

    // Init Telegram
    let telegram = Arc::new(TelegramSender::new(
        config.tg_bot_token.as_deref(),
        config.tg_chat_id.as_deref(),
    ));
    telegram.alert_startup().await;

    tracing::info!("Bot initialized, alert_only={}", config.alert_only);

    // Init Polymarket client
    let poly_client = Arc::new(
        PolyClient::new(&config.private_key, &config.poly_api_url).await?,
    );

    // Init Binance feed
    let binance = Arc::new(binance::BinanceFeed::new());
    let binance_clone = binance.clone();
    tokio::spawn(async move {
        binance_clone.as_ref().run().await;
    });

    // Init market finder
    let market_finder = Arc::new(MarketFinder::new(poly_client.clone()));

    // Do initial market scan
    if let Err(e) = market_finder.refresh().await {
        tracing::error!("Initial market scan failed: {e}");
        telegram
            .alert_error("startup", &format!("Market scan failed: {e}"))
            .await;
    }

    // Spawn market refresh loop
    let mf_clone = market_finder.clone();
    let tg_clone = telegram.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5 minutes
        interval.tick().await; // skip immediate tick (already did initial scan)
        loop {
            interval.tick().await;
            if let Err(e) = mf_clone.refresh().await {
                tracing::error!("Market refresh failed: {e}");
                tg_clone
                    .alert_error("market_refresh", &format!("{e}"))
                    .await;
            }
        }
    });

    // Init strategies
    let mut arb = ArbScanner::new(
        poly_client.clone(),
        market_finder.clone(),
        config.clone(),
    );
    let mut momentum = MomentumSniper::new(
        binance.clone(),
        market_finder.clone(),
        poly_client.clone(),
        config.clone(),
    );

    // Main strategy loop
    let mut arb_interval = tokio::time::interval(Duration::from_secs(30));
    let mut momentum_interval = tokio::time::interval(Duration::from_millis(500));

    tracing::info!("Entering main loop...");

    loop {
        tokio::select! {
            _ = arb_interval.tick() => {
                match arb.tick().await {
                    Ok(actions) => process_actions(&actions, &telegram, &config).await,
                    Err(e) => {
                        tracing::error!("Arb scanner error: {e}");
                        telegram.alert_error("arb_scanner", &e.to_string()).await;
                    }
                }
            }
            _ = momentum_interval.tick() => {
                match momentum.tick().await {
                    Ok(actions) => process_actions(&actions, &telegram, &config).await,
                    Err(e) => {
                        tracing::error!("Momentum sniper error: {e}");
                        // Don't alert every 500ms on repeated errors
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received ctrl+c, shutting down...");
                break;
            }
        }
    }

    telegram.send_silent("Bot shutting down.").await;
    tracing::info!("Goodbye.");
    Ok(())
}

async fn process_actions(actions: &[StrategyAction], telegram: &TelegramSender, config: &Config) {
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
                    // Phase 2: actual order placement
                    tracing::warn!("Order placement not yet implemented");
                }
            }
            StrategyAction::CancelAllOrders => {
                tracing::warn!("Kill switch triggered — cancel all not yet implemented");
                telegram
                    .send_silent("🚨 <b>KILL SWITCH</b> — cancelling all orders")
                    .await;
            }
        }
    }
}
