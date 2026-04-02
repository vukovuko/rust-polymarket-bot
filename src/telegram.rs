use anyhow::Result;
use serde_json::json;

pub struct TelegramSender {
    client: reqwest::Client,
    bot_token: String,
    chat_id: String,
    enabled: bool,
}

impl TelegramSender {
    pub fn new(tg_bot_token: Option<&str>, tg_chat_id: Option<&str>) -> Self {
        match (tg_bot_token, tg_chat_id) {
            (Some(token), Some(chat_id)) if !token.is_empty() && !chat_id.is_empty() => {
                tracing::info!("Telegram alerts enabled");
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(10))
                    .build()
                    .unwrap_or_default();
                TelegramSender {
                    client,
                    bot_token: token.to_string(),
                    chat_id: chat_id.to_string(),
                    enabled: true,
                }
            }
            _ => {
                tracing::warn!("Telegram not configured — alerts will only be logged");
                TelegramSender {
                    client: reqwest::Client::new(),
                    bot_token: String::new(),
                    chat_id: String::new(),
                    enabled: false,
                }
            }
        }
    }

    pub async fn send(&self, message: &str) -> Result<()> {
        if !self.enabled {
            tracing::info!("[TG disabled] {message}");
            return Ok(());
        }

        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        let body = json!({
            "chat_id": self.chat_id,
            "text": message,
            "parse_mode": "HTML",
        });

        let resp = self.client.post(&url).json(&body).send().await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            tracing::error!("Telegram API error {status}: {text}");
            anyhow::bail!("Telegram send failed: {status}");
        }

        Ok(())
    }

    pub async fn send_silent(&self, message: &str) {
        if let Err(e) = self.send(message).await {
            tracing::error!("Failed to send Telegram alert: {e}");
        }
    }

    pub async fn alert_startup(&self, alert_only: bool) {
        let mode = if alert_only {
            "Alert-only mode"
        } else {
            "LIVE TRADING mode"
        };
        self.send_silent(&format!(
            "🤖 <b>Polymarket Bot Starting</b>\n{mode} active."
        ))
        .await;
    }

    pub async fn alert_error(&self, context: &str, error: &str) {
        let msg = format!("⚠️ <b>Error in {context}</b>\n<code>{error}</code>");
        self.send_silent(&msg).await;
    }
}
