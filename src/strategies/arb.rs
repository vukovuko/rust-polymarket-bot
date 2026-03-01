use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use polymarket_client_sdk::types::Decimal;

use crate::config::Config;
use crate::polymarket::client::PolyClient;
use crate::polymarket::market_finder::MarketFinder;

use super::{Mode, Strategy, StrategyAction};

const ALERT_COOLDOWN: Duration = Duration::from_secs(300); // 5 minutes between repeat alerts

pub struct ArbScanner {
    client: Arc<PolyClient>,
    market_finder: Arc<MarketFinder>,
    config: Arc<Config>,
    known_opps: HashMap<String, Instant>,
}

impl ArbScanner {
    pub fn new(
        client: Arc<PolyClient>,
        market_finder: Arc<MarketFinder>,
        config: Arc<Config>,
    ) -> Self {
        ArbScanner {
            client,
            market_finder,
            config,
            known_opps: HashMap::new(),
        }
    }

    fn should_alert(&mut self, condition_id: &str) -> bool {
        let now = Instant::now();

        // Clean up old entries
        self.known_opps.retain(|_, t| now.duration_since(*t) < ALERT_COOLDOWN);

        match self.known_opps.get(condition_id) {
            Some(last) if now.duration_since(*last) < ALERT_COOLDOWN => false,
            _ => {
                self.known_opps.insert(condition_id.to_string(), now);
                true
            }
        }
    }
}

impl Strategy for ArbScanner {
    fn name(&self) -> &str {
        "Arb Scanner"
    }

    fn mode(&self) -> Mode {
        Mode::Arb
    }

    async fn tick(&mut self) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();
        let markets = self.market_finder.all_markets().await;
        let mut scanned = 0;
        let mut arbs_found = 0;

        for market in &markets {
            if !market.enable_order_book || !market.active {
                continue;
            }

            // Fetch order books for both YES and NO tokens
            let yes_book = match self.client.get_order_book(market.yes_token_id).await {
                Ok(book) => book,
                Err(e) => {
                    tracing::debug!("Failed to get YES book for {}: {e}", market.question);
                    continue;
                }
            };

            let no_book = match self.client.get_order_book(market.no_token_id).await {
                Ok(book) => book,
                Err(e) => {
                    tracing::debug!("Failed to get NO book for {}: {e}", market.question);
                    continue;
                }
            };

            scanned += 1;

            // Need best asks on both sides
            let yes_ask = match yes_book.best_ask {
                Some(a) => a,
                None => continue,
            };
            let no_ask = match no_book.best_ask {
                Some(a) => a,
                None => continue,
            };

            let combined = yes_ask + no_ask;
            let threshold = Decimal::ONE - self.config.arb_threshold;

            if combined < threshold {
                // Maker fee is 0, so net_edge = 1.00 - combined
                let net_edge = Decimal::ONE - combined;
                arbs_found += 1;

                if self.should_alert(&market.condition_id) {
                    let msg = format!(
                        "🎯 <b>Arb Opportunity</b>\n\
                         Market: {}\n\
                         YES ask: ${yes_ask}\n\
                         NO ask: ${no_ask}\n\
                         Combined: ${combined}\n\
                         Net edge: ${net_edge} ({:.2}%)",
                        market.question,
                        net_edge * Decimal::ONE_HUNDRED,
                    );
                    actions.push(StrategyAction::Alert(msg));
                }
            }
        }

        if scanned > 0 {
            tracing::info!(
                "Arb scan: checked {scanned} markets, found {arbs_found} opportunities"
            );
        }

        Ok(actions)
    }
}
