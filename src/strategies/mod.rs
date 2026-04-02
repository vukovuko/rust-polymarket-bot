pub mod paper_tracker;
pub mod settlement_logger;
pub mod weather;

use polymarket_client_sdk::types::{Decimal, U256};

#[derive(Debug)]
#[allow(dead_code)]
pub enum StrategyAction {
    Alert(String),
    PlaceOrder {
        token_id: U256,
        price: Decimal,
        size: Decimal,
        reason: String,
    },
    ArbExecute {
        token_a_id: U256,
        token_b_id: U256,
        token_a_price: Decimal,
        token_b_price: Decimal,
        size_usdc: Decimal,
        condition_id: String,
        question: String,
    },
    CancelAllOrders,
}
