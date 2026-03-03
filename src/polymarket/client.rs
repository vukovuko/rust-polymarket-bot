use std::str::FromStr;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use polymarket_client_sdk::POLYGON;
use polymarket_client_sdk::auth::state::Authenticated;
use polymarket_client_sdk::auth::{LocalSigner, Normal, Signer};
use polymarket_client_sdk::clob::types::{OrderType, Side};
use polymarket_client_sdk::clob::{Client, Config as ClobConfig};
use polymarket_client_sdk::types::{Decimal, U256};

use super::types::BotMarket;

pub struct OrderResult {
    pub order_id: String,
    pub success: bool,
    pub error_msg: Option<String>,
}

pub struct PolyClient {
    clob: Client<Authenticated<Normal>>,
    signer: LocalSigner<k256::ecdsa::SigningKey>,
}

impl PolyClient {
    pub async fn new(private_key: &str, api_url: &str) -> Result<Self> {
        let signer = LocalSigner::from_str(private_key)
            .context("Failed to parse private key")?
            .with_chain_id(Some(POLYGON));

        tracing::info!(
            "Authenticating with Polymarket (address: {:?})...",
            signer.address()
        );

        let clob = Client::new(api_url, ClobConfig::default())
            .context("Failed to create CLOB client")?
            .authentication_builder(&signer)
            .authenticate()
            .await
            .context("Failed to authenticate with Polymarket")?;

        tracing::info!("Authenticated with Polymarket successfully");

        Ok(PolyClient { clob, signer })
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

    #[allow(dead_code)]
    pub async fn get_fee_rate(&self, token_id: U256) -> Result<u32> {
        let resp = self
            .clob
            .fee_rate_bps(token_id)
            .await
            .context("Failed to fetch fee rate")?;
        Ok(resp.base_fee)
    }

    #[allow(dead_code)]
    pub async fn get_midpoint(&self, token_id: U256) -> Result<Decimal> {
        use polymarket_client_sdk::clob::types::request::MidpointRequest;
        let request = MidpointRequest::builder().token_id(token_id).build();
        let resp = self
            .clob
            .midpoint(&request)
            .await
            .context("Failed to fetch midpoint")?;
        Ok(resp.mid)
    }

    /// Place a maker-only limit buy order.
    pub async fn place_limit_buy(
        &self,
        token_id: U256,
        price: Decimal,
        size: Decimal,
    ) -> Result<OrderResult> {
        let signable = self
            .clob
            .limit_order()
            .token_id(token_id)
            .side(Side::Buy)
            .price(price)
            .size(size)
            .order_type(OrderType::GTC)
            .post_only(true)
            .build()
            .await
            .context("Failed to build limit order")?;

        let signed = self
            .clob
            .sign(&self.signer, signable)
            .await
            .context("Failed to sign order")?;

        let resp = self
            .clob
            .post_order(signed)
            .await
            .context("Failed to post order")?;

        Ok(OrderResult {
            order_id: resp.order_id,
            success: resp.success,
            error_msg: resp.error_msg,
        })
    }

    /// Cancel a single order by ID.
    pub async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let resp = self
            .clob
            .cancel_order(order_id)
            .await
            .context("Failed to cancel order")?;

        if !resp.not_canceled.is_empty() {
            tracing::warn!("Failed to cancel some orders: {:?}", resp.not_canceled);
        }
        Ok(())
    }

    /// Cancel all orders (kill switch).
    pub async fn cancel_all(&self) -> Result<()> {
        let resp = self
            .clob
            .cancel_all_orders()
            .await
            .context("Failed to cancel all orders")?;

        tracing::info!(
            "Cancelled {} orders, {} failed",
            resp.canceled.len(),
            resp.not_canceled.len(),
        );
        Ok(())
    }
}
