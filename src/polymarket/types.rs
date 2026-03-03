use chrono::NaiveDate;
use polymarket_client_sdk::clob::types::response::MarketResponse;
use polymarket_client_sdk::types::{DateTime, Decimal, U256, Utc};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BotMarket {
    pub condition_id: String,
    pub question: String,
    pub market_slug: String,
    pub end_date: Option<DateTime<Utc>>,
    pub yes_token_id: U256,
    pub no_token_id: U256,
    pub yes_outcome: String,
    pub no_outcome: String,
    pub minimum_tick_size: Decimal,
    pub minimum_order_size: Decimal,
    pub neg_risk: bool,
    pub active: bool,
    pub enable_order_book: bool,
}

impl BotMarket {
    pub fn from_market_response(m: &MarketResponse) -> Option<Self> {
        if m.tokens.len() != 2 {
            return None;
        }

        let condition_id = m.condition_id.map(|c| format!("{c:?}"))?;

        // Determine which token is the "positive" side (Yes/Up) and "negative" (No/Down).
        // Polymarket uses "Yes"/"No" for most markets, but "Up"/"Down" for BTC 5-min markets.
        let first = m.tokens[0].outcome.to_lowercase();
        let (yes_idx, no_idx) = if first == "yes" || first == "up" {
            (0, 1)
        } else {
            (1, 0)
        };

        Some(BotMarket {
            condition_id,
            question: m.question.clone(),
            market_slug: m.market_slug.clone(),
            end_date: m.end_date_iso,
            yes_token_id: m.tokens[yes_idx].token_id,
            no_token_id: m.tokens[no_idx].token_id,
            yes_outcome: m.tokens[yes_idx].outcome.clone(),
            no_outcome: m.tokens[no_idx].outcome.clone(),
            minimum_tick_size: m.minimum_tick_size,
            minimum_order_size: m.minimum_order_size,
            neg_risk: m.neg_risk,
            active: m.active,
            enable_order_book: m.enable_order_book,
        })
    }
}

/// A weather temperature bucket market with structured metadata.
/// Each bucket is one binary market within a multi-outcome weather event.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct WeatherMarket {
    pub market: BotMarket,
    pub city_slug: String,
    pub city_name: String,
    pub date: NaiveDate,
    /// Lower temperature bound. f64::NEG_INFINITY for "X or below" buckets.
    pub bucket_lower: f64,
    /// Upper temperature bound. f64::INFINITY for "X or above" buckets.
    pub bucket_upper: f64,
    /// True if temperature is in Fahrenheit, false for Celsius.
    pub fahrenheit: bool,
    /// YES price from Gamma API at discovery time (may be stale).
    pub gamma_yes_price: f64,
}

impl WeatherMarket {
    /// Human-readable bucket label like "38-39°F" or "≤31°F".
    pub fn bucket_label(&self) -> String {
        let unit = if self.fahrenheit { "F" } else { "C" };
        if self.bucket_lower == f64::NEG_INFINITY {
            format!("≤{:.0}°{unit}", self.bucket_upper)
        } else if self.bucket_upper == f64::INFINITY {
            format!("≥{:.0}°{unit}", self.bucket_lower)
        } else {
            format!("{:.0}-{:.0}°{unit}", self.bucket_lower, self.bucket_upper)
        }
    }
}
