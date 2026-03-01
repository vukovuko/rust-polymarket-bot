use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use polymarket_client_sdk::types::Decimal;

use crate::binance::BinanceFeed;
use crate::config::Config;
use crate::polymarket::client::PolyClient;
use crate::polymarket::market_finder::MarketFinder;
use crate::polymarket::types::Direction;

use super::{Mode, Strategy, StrategyAction};

const SIGNAL_COOLDOWN: Duration = Duration::from_secs(60);

pub struct MomentumSniper {
    binance: Arc<BinanceFeed>,
    market_finder: Arc<MarketFinder>,
    client: Arc<PolyClient>,
    config: Arc<Config>,
    last_signal: Option<Instant>,
    current_window_market_id: Option<String>,
    current_window_traded: bool,
}

impl MomentumSniper {
    pub fn new(
        binance: Arc<BinanceFeed>,
        market_finder: Arc<MarketFinder>,
        client: Arc<PolyClient>,
        config: Arc<Config>,
    ) -> Self {
        MomentumSniper {
            binance,
            market_finder,
            client,
            config,
            last_signal: None,
            current_window_market_id: None,
            current_window_traded: false,
        }
    }
}

impl Strategy for MomentumSniper {
    fn name(&self) -> &str {
        "Momentum Sniper"
    }

    fn mode(&self) -> Mode {
        Mode::Momentum
    }

    async fn tick(&mut self) -> Result<Vec<StrategyAction>> {
        let mut actions = Vec::new();

        // Check if Binance feed is connected
        if !self.binance.is_connected() {
            return Ok(actions);
        }

        // Get price change over last 5 seconds
        let change = match self.binance.price_change_5s().await {
            Some(c) => c,
            None => return Ok(actions),
        };

        // Check if above threshold
        if change.abs() < self.config.momentum_threshold {
            return Ok(actions);
        }

        // Within cooldown?
        if let Some(last) = self.last_signal {
            if last.elapsed() < SIGNAL_COOLDOWN {
                return Ok(actions);
            }
        }

        // Find current 5-min BTC market
        let market = match self.market_finder.find_current_btc_5min().await {
            Some(m) => m,
            None => {
                tracing::debug!("Momentum signal but no BTC 5-min market found");
                return Ok(actions);
            }
        };

        // Reset traded flag if market window changed
        if self.current_window_market_id.as_ref() != Some(&market.condition_id) {
            self.current_window_market_id = Some(market.condition_id.clone());
            self.current_window_traded = false;
        }

        // Already traded this window?
        if self.current_window_traded {
            return Ok(actions);
        }

        let direction = if change > 0.0 {
            Direction::Up
        } else {
            Direction::Down
        };

        // Determine target token: UP = YES, DOWN = NO
        let target_token_id = match direction {
            Direction::Up => market.yes_token_id,
            Direction::Down => market.no_token_id,
        };

        // Get order book for target token
        let book = match self.client.get_order_book(target_token_id).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("Failed to get book for momentum target: {e}");
                return Ok(actions);
            }
        };

        let max_ask = Decimal::new(90, 2); // 0.90

        if let Some(best_ask) = book.best_ask {
            if best_ask < max_ask {
                let btc_price = self.binance.latest_price().await.unwrap_or(0.0);

                let msg = format!(
                    "⚡ <b>Momentum Signal</b>\n\
                     Direction: {direction}\n\
                     BTC change (5s): {:.3}%\n\
                     BTC price: ${btc_price:.2}\n\
                     Market: {}\n\
                     Target ask: ${best_ask}\n\
                     Potential profit: ${:.4}",
                    change * 100.0,
                    market.question,
                    Decimal::ONE - best_ask,
                );
                actions.push(StrategyAction::Alert(msg));

                self.last_signal = Some(Instant::now());
                self.current_window_traded = true;
            } else {
                tracing::debug!(
                    "Momentum signal but ask too high: ${best_ask} >= $0.90"
                );
            }
        }

        Ok(actions)
    }
}
