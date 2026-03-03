use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{RwLock, watch};
use tokio_tungstenite::connect_async;

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
const MAX_BUFFER_AGE_MS: u64 = 10_000;
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Broadcast on every trade: (price_change_5s, latest_price)
pub type PriceSignal = (f64, f64);

#[derive(Debug, Clone)]
struct PriceTick {
    price: f64,
    timestamp_ms: u64,
}

#[derive(Clone)]
pub struct BinanceFeed {
    prices: Arc<RwLock<VecDeque<PriceTick>>>,
    connected: Arc<AtomicBool>,
    signal_tx: Arc<watch::Sender<Option<PriceSignal>>>,
}

impl BinanceFeed {
    pub fn new() -> (Self, watch::Receiver<Option<PriceSignal>>) {
        let (tx, rx) = watch::channel(None);
        let feed = BinanceFeed {
            prices: Arc::new(RwLock::new(VecDeque::with_capacity(512))),
            connected: Arc::new(AtomicBool::new(false)),
            signal_tx: Arc::new(tx),
        };
        (feed, rx)
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

        // Compute 5s change and broadcast immediately
        if let Some(change) = Self::compute_change_5s(&prices) {
            let _ = self.signal_tx.send(Some((change, price)));
        }
    }

    fn compute_change_5s(prices: &VecDeque<PriceTick>) -> Option<f64> {
        let newest = prices.back()?;
        let cutoff = newest.timestamp_ms.saturating_sub(5_000);
        let oldest = prices.iter().find(|t| t.timestamp_ms >= cutoff)?;
        if oldest.price == 0.0 {
            return None;
        }
        Some((newest.price - oldest.price) / oldest.price)
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}
