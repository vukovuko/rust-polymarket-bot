use polymarket_client_sdk::clob::types::response::{MarketResponse, OrderBookSummaryResponse};
use polymarket_client_sdk::types::{DateTime, Decimal, U256, Utc};

#[derive(Debug, Clone)]
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

        // Determine which token is YES and which is NO
        let (yes_idx, no_idx) = if m.tokens[0].outcome.to_lowercase() == "yes" {
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

#[derive(Debug, Clone)]
pub struct SimpleBook {
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub midpoint: Option<Decimal>,
}

impl SimpleBook {
    pub fn from_order_book(book: &OrderBookSummaryResponse) -> Self {
        let best_bid = book.bids.iter().map(|o| o.price).max();
        let best_ask = book.asks.iter().map(|o| o.price).min();
        let midpoint = match (best_bid, best_ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::TWO),
            _ => None,
        };

        SimpleBook {
            best_bid,
            best_ask,
            midpoint,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Up => write!(f, "UP"),
            Direction::Down => write!(f, "DOWN"),
        }
    }
}
