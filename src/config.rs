use anyhow::{Context, Result, bail};
use polymarket_client_sdk::types::Decimal;
use std::str::FromStr;

pub struct Config {
    pub private_key: String,
    pub poly_api_url: String,
    pub max_trade_usd: Decimal,
    pub max_daily_exposure: Decimal,
    pub kill_switch_loss: Decimal,
    pub max_active_positions: u32,
    pub arb_threshold: Decimal,
    pub momentum_threshold: f64,
    pub spread_offset: Decimal,
    pub tg_bot_token: Option<String>,
    pub tg_chat_id: Option<String>,
    pub alert_only: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let private_key = std::env::var("PRIVATE_KEY")
            .context("PRIVATE_KEY env var is required")?;

        if !private_key.starts_with("0x") || private_key.len() != 66 {
            bail!("PRIVATE_KEY must start with 0x and be 66 characters (32 bytes hex)");
        }

        let poly_api_url = std::env::var("POLY_API_URL")
            .unwrap_or_else(|_| "https://clob.polymarket.com".to_string());

        let max_trade_usd = env_decimal("MAX_TRADE_USD", "5")?;
        let max_daily_exposure = env_decimal("MAX_DAILY_EXPOSURE", "20")?;
        let kill_switch_loss = env_decimal("KILL_SWITCH_LOSS", "10")?;
        let max_active_positions = env_u32("MAX_ACTIVE_POSITIONS", "3")?;
        let arb_threshold = env_decimal("ARB_THRESHOLD", "0.02")?;
        let momentum_threshold = env_f64("MOMENTUM_THRESHOLD", "0.0015")?;
        let spread_offset = env_decimal("SPREAD_OFFSET", "0.03")?;
        let alert_only = env_bool("ALERT_ONLY", true);

        let tg_bot_token = non_empty_env("TG_BOT_TOKEN");
        let tg_chat_id = non_empty_env("TG_CHAT_ID");

        Ok(Config {
            private_key,
            poly_api_url,
            max_trade_usd,
            max_daily_exposure,
            kill_switch_loss,
            max_active_positions,
            arb_threshold,
            momentum_threshold,
            spread_offset,
            tg_bot_token,
            tg_chat_id,
            alert_only,
        })
    }

    pub fn log_summary(&self) {
        tracing::info!("Configuration loaded:");
        tracing::info!("  API URL: {}", self.poly_api_url);
        tracing::info!("  Max trade: ${}", self.max_trade_usd);
        tracing::info!("  Max daily exposure: ${}", self.max_daily_exposure);
        tracing::info!("  Kill switch at: -${}", self.kill_switch_loss);
        tracing::info!("  Max active positions: {}", self.max_active_positions);
        tracing::info!("  Arb threshold: {}", self.arb_threshold);
        tracing::info!("  Momentum threshold: {}%", self.momentum_threshold * 100.0);
        tracing::info!("  Spread offset: {}", self.spread_offset);
        tracing::info!("  Alert only: {}", self.alert_only);
        tracing::info!(
            "  Telegram: {}",
            if self.tg_bot_token.is_some() && self.tg_chat_id.is_some() {
                "configured"
            } else {
                "disabled"
            }
        );
    }
}

fn env_decimal(key: &str, default: &str) -> Result<Decimal> {
    let val = std::env::var(key).unwrap_or_else(|_| default.to_string());
    Decimal::from_str(&val).with_context(|| format!("{key} must be a valid decimal"))
}

fn env_f64(key: &str, default: &str) -> Result<f64> {
    let val = std::env::var(key).unwrap_or_else(|_| default.to_string());
    val.parse::<f64>()
        .with_context(|| format!("{key} must be a valid number"))
}

fn env_u32(key: &str, default: &str) -> Result<u32> {
    let val = std::env::var(key).unwrap_or_else(|_| default.to_string());
    val.parse::<u32>()
        .with_context(|| format!("{key} must be a valid integer"))
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .map(|v| matches!(v.to_lowercase().as_str(), "true" | "1" | "yes"))
        .unwrap_or(default)
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
