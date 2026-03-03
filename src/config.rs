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
    pub spread_offset: Decimal,
    pub tg_bot_token: Option<String>,
    pub tg_chat_id: Option<String>,
    pub alert_only: bool,
    // Weather strategy
    pub edge_threshold: f64,
    pub min_probability: f64,
    pub max_weather_position: Decimal,
    pub weather_scan_interval_secs: u64,
    pub weather_fast_scan_interval_secs: u64,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
    pub std_inflation: f64,
    pub slippage_estimate: f64,
    pub apply_bias_correction: bool,
    pub require_ws_price: bool,
    // Kelly sizing
    pub bankroll: f64,
    pub kelly_fraction: f64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let private_key =
            std::env::var("PRIVATE_KEY").context("PRIVATE_KEY env var is required")?;

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
        let spread_offset = env_decimal("SPREAD_OFFSET", "0.03")?;
        let alert_only = env_bool("ALERT_ONLY", true);

        let tg_bot_token = non_empty_env("TG_BOT_TOKEN");
        let tg_chat_id = non_empty_env("TG_CHAT_ID");

        let edge_threshold = env_f64("EDGE_THRESHOLD", "0.10")?;
        let min_probability = env_f64("MIN_PROBABILITY", "0.30")?;
        let max_weather_position = env_decimal("MAX_WEATHER_POSITION", "10")?;
        let weather_scan_interval_secs = env_u64("WEATHER_SCAN_INTERVAL", "1800")?;
        let weather_fast_scan_interval_secs = env_u64("WEATHER_FAST_SCAN_INTERVAL", "300")?;
        let min_entry_price = env_f64("MIN_ENTRY_PRICE", "0.03")?;
        let max_entry_price = env_f64("MAX_ENTRY_PRICE", "0.65")?;
        let std_inflation = env_f64("STD_INFLATION", "1.8")?;
        let slippage_estimate = env_f64("SLIPPAGE_ESTIMATE", "0.02")?;
        let apply_bias_correction = env_bool("APPLY_BIAS_CORRECTION", true);
        let require_ws_price = env_bool("REQUIRE_WS_PRICE", false);
        let bankroll = env_f64("BANKROLL", "77")?;
        let kelly_fraction = env_f64("KELLY_FRACTION", "0.25")?;

        Ok(Config {
            private_key,
            poly_api_url,
            max_trade_usd,
            max_daily_exposure,
            kill_switch_loss,
            max_active_positions,
            arb_threshold,
            spread_offset,
            tg_bot_token,
            tg_chat_id,
            alert_only,
            edge_threshold,
            min_probability,
            max_weather_position,
            weather_scan_interval_secs,
            weather_fast_scan_interval_secs,
            min_entry_price,
            max_entry_price,
            std_inflation,
            slippage_estimate,
            apply_bias_correction,
            require_ws_price,
            bankroll,
            kelly_fraction,
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
        tracing::info!("  Weather edge threshold: {}%", self.edge_threshold * 100.0);
        tracing::info!(
            "  Weather min probability: {}%",
            self.min_probability * 100.0
        );
        tracing::info!("  Weather max position: ${}", self.max_weather_position);
        tracing::info!(
            "  Weather scan interval: {}s (fast: {}s)",
            self.weather_scan_interval_secs,
            self.weather_fast_scan_interval_secs,
        );
        tracing::info!(
            "  Weather entry price: ${:.2}–${:.2}",
            self.min_entry_price,
            self.max_entry_price,
        );
        tracing::info!("  Weather std inflation: {:.2}x", self.std_inflation);
        tracing::info!(
            "  Weather slippage estimate: {:.1}%",
            self.slippage_estimate * 100.0
        );
        tracing::info!("  Weather bias correction: {}", self.apply_bias_correction);
        tracing::info!("  Weather require WS price: {}", self.require_ws_price);
        tracing::info!(
            "  Kelly: {:.0}% of ${:.0} bankroll",
            self.kelly_fraction * 100.0,
            self.bankroll,
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

fn env_u64(key: &str, default: &str) -> Result<u64> {
    let val = std::env::var(key).unwrap_or_else(|_| default.to_string());
    val.parse::<u64>()
        .with_context(|| format!("{key} must be a valid integer"))
}

fn env_u32(key: &str, default: &str) -> Result<u32> {
    let val = std::env::var(key).unwrap_or_else(|_| default.to_string());
    val.parse::<u32>()
        .with_context(|| format!("{key} must be a valid integer"))
}

fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let lower = v.trim().to_lowercase();
            match lower.as_str() {
                "true" | "1" | "yes" => true,
                "false" | "0" | "no" => false,
                _ => {
                    tracing::warn!(
                        "{key}={v} is not a recognized boolean — defaulting to {default}. \
                         Use true/false/1/0/yes/no."
                    );
                    default
                }
            }
        }
        Err(_) => default,
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}
