//! Standalone test: prove Polymarket WebSocket works with real data.
//!
//! Fetches a handful of active markets via REST, subscribes to best_bid_ask
//! via WebSocket, and prints every event for 30 seconds.
//!
//! Run: cargo run --example ws_test

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::auth::{LocalSigner, Signer};
use polymarket_client_sdk::clob::ws::Client as WsClient;
use polymarket_client_sdk::clob::{Client as RestClient, Config as ClobConfig};
use polymarket_client_sdk::types::U256;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt().with_env_filter("info").init();

    // --- Step 1: Authenticate REST client and fetch markets ---
    let private_key = std::env::var("PRIVATE_KEY").expect("PRIVATE_KEY env var required");
    let api_url =
        std::env::var("POLY_API_URL").unwrap_or_else(|_| "https://clob.polymarket.com".to_string());

    let signer = LocalSigner::from_str(&private_key)?.with_chain_id(Some(POLYGON));

    tracing::info!("Authenticating (address: {:?})...", signer.address());

    let rest = RestClient::new(&api_url, ClobConfig::default())?
        .authentication_builder(&signer)
        .authenticate()
        .await?;

    tracing::info!("Fetching active markets...");

    let mut token_ids: Vec<U256> = Vec::new();
    let mut market_count = 0u32;
    let mut stream = Box::pin(rest.stream_data(RestClient::markets));

    while let Some(result) = stream.next().await {
        match result {
            Ok(market) => {
                if market.active && !market.closed && market.enable_order_book {
                    for token in &market.tokens {
                        token_ids.push(token.token_id);
                    }
                    market_count += 1;
                    if market_count >= 10 {
                        break;
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Error streaming market: {e}");
                break;
            }
        }
    }
    // Drop the stream (and its borrow on rest) so rest can be dropped
    drop(stream);

    tracing::info!(
        "Collected {} token IDs from {} markets",
        token_ids.len(),
        market_count,
    );

    if token_ids.is_empty() {
        tracing::error!("No tokens found — nothing to subscribe to");
        return Ok(());
    }

    // --- Step 2: Subscribe to WebSocket best_bid_ask ---
    tracing::info!("Creating WebSocket client...");
    let ws_client = WsClient::default();

    tracing::info!(
        "Subscribing to best_bid_ask for {} tokens...",
        token_ids.len()
    );
    let mut bba_stream = Box::pin(ws_client.subscribe_best_bid_ask(token_ids.clone())?);

    let event_count = Arc::new(AtomicU64::new(0));
    let counter = event_count.clone();

    // --- Step 3: Print events for 30 seconds ---
    let timeout = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(timeout);

    tracing::info!("Listening for 30 seconds...\n");

    loop {
        tokio::select! {
            Some(result) = bba_stream.next() => {
                match result {
                    Ok(bba) => {
                        let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
                        let spread = bba.best_ask - bba.best_bid;
                        println!(
                            "[#{n}] asset={} bid={} ask={} spread={} ts={}",
                            bba.asset_id, bba.best_bid, bba.best_ask, spread, bba.timestamp,
                        );
                    }
                    Err(e) => {
                        tracing::warn!("BestBidAsk stream error: {e}");
                    }
                }
            }
            () = &mut timeout => {
                break;
            }
        }
    }

    let total = event_count.load(Ordering::Relaxed);
    tracing::info!("Done. Received {total} BestBidAsk events in 30 seconds.");

    // Also quickly test new_market subscription
    tracing::info!("Testing new_market subscription (5 seconds)...");
    let mut nm_stream = Box::pin(ws_client.subscribe_new_markets(token_ids)?);
    let nm_timeout = tokio::time::sleep(Duration::from_secs(5));
    tokio::pin!(nm_timeout);

    loop {
        tokio::select! {
            Some(result) = nm_stream.next() => {
                match result {
                    Ok(nm) => {
                        println!("NEW MARKET: id={} question={}", nm.id, nm.question);
                    }
                    Err(e) => {
                        tracing::warn!("NewMarket stream error: {e}");
                    }
                }
            }
            () = &mut nm_timeout => {
                break;
            }
        }
    }

    tracing::info!("All done.");
    Ok(())
}
