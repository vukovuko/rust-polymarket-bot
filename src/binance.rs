use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::RwLock;
use tokio_tungstenite::connect_async;

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
const MAX_BUFFER_AGE_MS: u64 = 10_000;
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct PriceTick {
    pub price: f64,
    pub timestamp_ms: u64,
}

#[derive(Clone)]
pub struct BinanceFeed {
    prices: Arc<RwLock<VecDeque<PriceTick>>>,
    connected: Arc<AtomicBool>,
}

impl BinanceFeed {
    pub fn new() -> Self {
        BinanceFeed {
            prices: Arc::new(RwLock::new(VecDeque::with_capacity(512))),
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn run(&self) {
        let mut backoff = Duration::from_secs(1);

        loop {
            tracing::info!("Connecting to Binance WebSocket...");

            match connect_async(BINANCE_WS_URL).await {
                Ok((ws_stream, _)) => {
                    tracing::info!("Connected to Binance WebSocket");
                    self.connected.store(true, Ordering::Relaxed);
                    backoff = Duration::from_secs(1);

                    let (_write, mut read) = ws_stream.split();

                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                self.handle_message(&text).await;
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Ping(_)) => {}
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                                tracing::warn!("Binance WebSocket closed by server");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!("Binance WebSocket error: {e}");
                                break;
                            }
                            _ => {}
                        }
                    }

                    self.connected.store(false, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to Binance: {e}");
                }
            }

            tracing::info!("Reconnecting in {:?}...", backoff);
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_RECONNECT_DELAY);
        }
    }

    async fn handle_message(&self, text: &str) {
        // Binance trade message: {"e":"trade","E":...,"s":"BTCUSDT","t":...,"p":"97245.50","q":"0.001","T":1672515782136,"m":true,...}
        let parsed: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        let price = match parsed["p"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(p) => p,
            None => return,
        };

        let timestamp_ms = match parsed["T"].as_u64() {
            Some(t) => t,
            None => return,
        };

        let tick = PriceTick {
            price,
            timestamp_ms,
        };

        let mut prices = self.prices.write().await;
        prices.push_back(tick);

        // Prune old entries
        let cutoff = timestamp_ms.saturating_sub(MAX_BUFFER_AGE_MS);
        while prices.front().is_some_and(|t| t.timestamp_ms < cutoff) {
            prices.pop_front();
        }
    }

    pub async fn latest_price(&self) -> Option<f64> {
        self.prices.read().await.back().map(|t| t.price)
    }

    /// Returns the price change over the last 5 seconds as a fraction (e.g., 0.0015 = 0.15%).
    pub async fn price_change_5s(&self) -> Option<f64> {
        let prices = self.prices.read().await;
        let newest = prices.back()?;
        let cutoff = newest.timestamp_ms.saturating_sub(5_000);

        // Find the oldest tick within the 5s window
        let oldest = prices.iter().find(|t| t.timestamp_ms >= cutoff)?;

        if oldest.price == 0.0 {
            return None;
        }

        Some((newest.price - oldest.price) / oldest.price)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}
