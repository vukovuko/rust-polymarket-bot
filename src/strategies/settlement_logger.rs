use std::io::Write;
use std::sync::Arc;

use polymarket_client_sdk::types::Utc;
use tokio::sync::{mpsc, watch};

use crate::binance::PriceSignal;
use crate::polymarket::market_finder::MarketFinder;
use crate::polymarket::ws::SharedBestAsks;

use super::StrategyAction;

/// How many seconds before window end to start logging.
const LOG_START_SECS: u64 = 60;
/// Log a snapshot every N seconds within the logging window.
const LOG_INTERVAL_SECS: u64 = 5;
/// Window duration in seconds.
const WINDOW_SECS: u64 = 300;

pub struct SettlementLogger {
    price_rx: watch::Receiver<Option<PriceSignal>>,
    best_asks: SharedBestAsks,
    market_finder: Arc<MarketFinder>,
    action_tx: mpsc::UnboundedSender<StrategyAction>,
}

#[derive(Debug)]
struct WindowState {
    window_start_unix: u64,
    btc_start_price: f64,
    logged_remaining: Vec<u64>, // which time_remaining values we've already logged
}

impl SettlementLogger {
    pub fn new(
        price_rx: watch::Receiver<Option<PriceSignal>>,
        best_asks: SharedBestAsks,
        market_finder: Arc<MarketFinder>,
        action_tx: mpsc::UnboundedSender<StrategyAction>,
    ) -> Self {
        SettlementLogger {
            price_rx,
            best_asks,
            market_finder,
            action_tx,
        }
    }

    pub async fn run(self) {
        tracing::info!("Settlement logger started");

        // Ensure logs directory exists
        if let Err(e) = std::fs::create_dir_all("logs") {
            tracing::error!("Failed to create logs directory: {e}");
            return;
        }

        // Open CSV file (append mode)
        let csv_path = "logs/settlement_data.csv";
        let file_exists = std::path::Path::new(csv_path).exists();
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(csv_path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to open {csv_path}: {e}");
                return;
            }
        };

        // Write CSV header if new file
        if !file_exists {
            let _ = writeln!(
                file,
                "timestamp,window_start_unix,time_remaining_s,btc_start_price,btc_current_price,\
                 distance_usd,distance_pct,up_best_ask,down_best_ask,predicted_winner,winning_ask"
            );
        }

        let mut current_window: Option<WindowState> = None;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            interval.tick().await;

            let now_unix = Utc::now().timestamp() as u64;
            let window_start = (now_unix / WINDOW_SECS) * WINDOW_SECS;
            let window_end = window_start + WINDOW_SECS;
            let time_remaining = window_end.saturating_sub(now_unix);

            // Get current BTC price
            let btc_price = match *self.price_rx.borrow() {
                Some((_change, price)) => price,
                None => continue, // No price yet
            };

            // New window detected — capture start price and log outcome of previous window
            if current_window
                .as_ref()
                .is_none_or(|w| w.window_start_unix != window_start)
            {
                // Log outcome of previous window
                if let Some(prev) = &current_window {
                    // BTC price NOW is approximately the end-of-previous-window price
                    // (we're within 1s of the boundary)
                    let actual_winner = if btc_price >= prev.btc_start_price {
                        "Up"
                    } else {
                        "Down"
                    };
                    let _ = writeln!(
                        file,
                        "{},{},OUTCOME,{:.2},{:.2},{:.2},{:.4},,,,{},,",
                        Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                        prev.window_start_unix,
                        prev.btc_start_price,
                        btc_price,
                        (btc_price - prev.btc_start_price).abs(),
                        ((btc_price - prev.btc_start_price) / prev.btc_start_price).abs(),
                        actual_winner,
                    );
                    let _ = file.flush();

                    tracing::info!(
                        "Settlement: window {} outcome={actual_winner} start=${:.2} end=${:.2} dist=${:.2}",
                        prev.window_start_unix,
                        prev.btc_start_price,
                        btc_price,
                        (btc_price - prev.btc_start_price).abs(),
                    );
                }

                // Start tracking new window
                current_window = Some(WindowState {
                    window_start_unix: window_start,
                    btc_start_price: btc_price,
                    logged_remaining: Vec::new(),
                });

                tracing::debug!(
                    "Settlement: new window {} start_price=${:.2}",
                    window_start,
                    btc_price,
                );
            }

            let state = current_window.as_mut().unwrap();

            // Only log in the final LOG_START_SECS seconds
            if time_remaining > LOG_START_SECS {
                continue;
            }

            // Log at LOG_INTERVAL_SECS intervals (60, 55, 50, ..., 5)
            // Round to nearest interval to handle timing jitter
            let bucket = (time_remaining / LOG_INTERVAL_SECS) * LOG_INTERVAL_SECS;
            if bucket == 0 || state.logged_remaining.contains(&bucket) {
                continue;
            }
            state.logged_remaining.push(bucket);

            // Find the current BTC 5-min market to get token IDs
            let market = match self.market_finder.find_current_btc_5min().await {
                Some(m) => m,
                None => {
                    tracing::debug!("Settlement: no current BTC market found at T-{bucket}s");
                    continue;
                }
            };

            // Read best_asks for Up and Down tokens
            let (up_ask, down_ask) = {
                let asks = self.best_asks.read().unwrap_or_else(|e| e.into_inner());
                let up = asks.get(&market.yes_token_id).copied();
                let down = asks.get(&market.no_token_id).copied();
                (up, down)
            };

            let distance_usd = (btc_price - state.btc_start_price).abs();
            let distance_pct = if state.btc_start_price > 0.0 {
                distance_usd / state.btc_start_price
            } else {
                0.0
            };

            let predicted_winner = if btc_price >= state.btc_start_price {
                "Up"
            } else {
                "Down"
            };

            let winning_ask = match predicted_winner {
                "Up" => up_ask,
                _ => down_ask,
            };

            let up_str = up_ask.map_or("-".to_string(), |a| format!("{a}"));
            let down_str = down_ask.map_or("-".to_string(), |a| format!("{a}"));
            let winning_str = winning_ask.map_or("-".to_string(), |a| format!("{a}"));

            // Write CSV row
            let _ = writeln!(
                file,
                "{},{},{},{:.2},{:.2},{:.2},{:.6},{},{},{},{},{}",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                state.window_start_unix,
                bucket,
                state.btc_start_price,
                btc_price,
                distance_usd,
                distance_pct,
                up_str,
                down_str,
                predicted_winner,
                winning_str,
                market.question,
            );
            let _ = file.flush();

            tracing::info!(
                "Settlement T-{bucket}s: BTC ${btc_price:.2} (start ${:.2}, dist ${distance_usd:.2}/{:.4}%) \
                 Up={up_str} Down={down_str} predicted={predicted_winner} winning_ask={winning_str}",
                state.btc_start_price,
                distance_pct * 100.0,
            );

            // Send Telegram summary at T-10s if distance is significant (>$50)
            if bucket == 10 && distance_usd > 50.0 {
                let msg = format!(
                    "📊 <b>Settlement Watch T-10s</b>\n\
                     Window: {}\n\
                     BTC start: ${:.2}\n\
                     BTC now: ${btc_price:.2}\n\
                     Distance: ${distance_usd:.2} ({:.3}%)\n\
                     Predicted: {predicted_winner}\n\
                     {predicted_winner} ask: {winning_str}\n\
                     Up ask: {up_str} | Down ask: {down_str}",
                    state.window_start_unix,
                    state.btc_start_price,
                    distance_pct * 100.0,
                );
                let _ = self.action_tx.send(StrategyAction::Alert(msg));
            }
        }
    }
}
