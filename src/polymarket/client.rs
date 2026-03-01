use std::str::FromStr;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::auth::{LocalSigner, Normal, Signer};
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::clob::types::request::OrderBookSummaryRequest;
use polymarket_client_sdk::clob::types::response::OrderBookSummaryResponse;
use polymarket_client_sdk::clob::{Client, Config as ClobConfig};
use polymarket_client_sdk::types::{Decimal, U256};

use super::types::{BotMarket, SimpleBook};

pub struct PolyClient {
    clob: Client<Authenticated<Normal>>,
    // Store private key string for Phase 2 (re-create signer when needed for order signing)
    #[allow(dead_code)]
    private_key: String,
}

impl PolyClient {
    pub async fn new(private_key: &str, api_url: &str) -> Result<Self> {
        let signer = LocalSigner::from_str(private_key)
            .context("Failed to parse private key")?
            .with_chain_id(Some(POLYGON));

        tracing::info!("Authenticating with Polymarket (address: {:?})...", signer.address());

        let clob = Client::new(api_url, ClobConfig::default())
            .context("Failed to create CLOB client")?
            .authentication_builder(&signer)
            .authenticate()
            .await
            .context("Failed to authenticate with Polymarket")?;

        tracing::info!("Authenticated with Polymarket successfully");

        Ok(PolyClient {
            clob,
            private_key: private_key.to_string(),
        })
    }

    pub async fn fetch_all_active_markets(&self) -> Result<Vec<BotMarket>> {
        let mut markets = Vec::new();
        let mut stream = Box::pin(self.clob.stream_data(Client::markets));

        while let Some(result) = stream.next().await {
            match result {
                Ok(market) => {
                    if market.active && !market.closed && market.enable_order_book {
                        if let Some(bot_market) = BotMarket::from_market_response(&market) {
                            markets.push(bot_market);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Error streaming market: {e}");
                    break;
                }
            }
        }

        tracing::info!("Fetched {} active markets with order books", markets.len());
        Ok(markets)
    }

    pub async fn get_order_book(&self, token_id: U256) -> Result<SimpleBook> {
        let request = OrderBookSummaryRequest::builder()
            .token_id(token_id)
            .build();

        let book: OrderBookSummaryResponse = self.clob.order_book(&request).await
            .context("Failed to fetch order book")?;

        Ok(SimpleBook::from_order_book(&book))
    }

    pub async fn get_fee_rate(&self, token_id: U256) -> Result<u32> {
        let resp = self.clob.fee_rate_bps(token_id).await
            .context("Failed to fetch fee rate")?;
        Ok(resp.base_fee)
    }

    pub async fn get_midpoint(&self, token_id: U256) -> Result<Decimal> {
        use polymarket_client_sdk::clob::types::request::MidpointRequest;
        let request = MidpointRequest::builder().token_id(token_id).build();
        let resp = self.clob.midpoint(&request).await
            .context("Failed to fetch midpoint")?;
        Ok(resp.mid)
    }
}
