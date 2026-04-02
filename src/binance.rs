use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::watch;
use tokio_tungstenite::connect_async;

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Broadcast on every trade: (latest_price, received_at)
pub type PriceSignal = (f64, Instant);

#[derive(Clone)]
pub struct BinanceFeed {
    connected: Arc<AtomicBool>,
    signal_tx: Arc<watch::Sender<Option<PriceSignal>>>,
}

impl BinanceFeed {
    pub fn new() -> (Self, watch::Receiver<Option<PriceSignal>>) {
        let (tx, rx) = watch::channel(None);
        let feed = BinanceFeed {
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
                                self.handle_message(&text);
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

    fn handle_message(&self, text: &str) {
        let parsed: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => return,
        };

        let price = match parsed["p"].as_str().and_then(|s| s.parse::<f64>().ok()) {
            Some(p) => p,
            None => return,
        };

        let _ = self.signal_tx.send(Some((price, Instant::now())));
    }

    #[allow(dead_code)]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }
}
