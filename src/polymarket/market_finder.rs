use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use polymarket_client_sdk::types::Utc;
use tokio::sync::RwLock;

use super::client::PolyClient;
use super::types::BotMarket;

pub struct MarketFinder {
    client: Arc<PolyClient>,
    all_markets: RwLock<Vec<BotMarket>>,
    last_full_scan: RwLock<Instant>,
}

impl MarketFinder {
    pub fn new(client: Arc<PolyClient>) -> Self {
        MarketFinder {
            client,
            all_markets: RwLock::new(Vec::new()),
            last_full_scan: RwLock::new(Instant::now()),
        }
    }

    pub async fn refresh(&self) -> Result<()> {
        let markets = self.client.fetch_all_active_markets().await?;
        let btc_5min_count = markets.iter().filter(|m| is_btc_5min_market(m)).count();

        tracing::info!(
            "Market refresh: {} total active, {} BTC 5-min markets",
            markets.len(),
            btc_5min_count
        );

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

    pub async fn all_markets(&self) -> Vec<BotMarket> {
        self.all_markets.read().await.clone()
    }

    pub async fn market_count(&self) -> usize {
        self.all_markets.read().await.len()
    }
}

fn is_btc_5min_market(market: &BotMarket) -> bool {
    let q = market.question.to_lowercase();
    let slug = market.market_slug.to_lowercase();

    // Check for BTC-related keywords
    let is_btc = q.contains("btc") || q.contains("bitcoin");

    // Check for 5-minute indicators
    let is_5min = q.contains("5 min")
        || q.contains("5-min")
        || q.contains("5min")
        || slug.contains("5m")
        || slug.contains("5-min");

    // Check for up/down binary pattern
    let is_binary = q.contains("up") || q.contains("down") || q.contains("above") || q.contains("below");

    is_btc && (is_5min || is_binary && slug.contains("5m"))
}
