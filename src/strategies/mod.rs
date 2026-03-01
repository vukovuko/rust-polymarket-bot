pub mod arb;
pub mod momentum;

use anyhow::Result;
use polymarket_client_sdk::types::{Decimal, U256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Arb,
    Momentum,
    Spread,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Arb => write!(f, "Arb"),
            Mode::Momentum => write!(f, "Momentum"),
            Mode::Spread => write!(f, "Spread"),
        }
    }
}

#[derive(Debug)]
pub enum StrategyAction {
    Alert(String),
    PlaceOrder {
        token_id: U256,
        price: Decimal,
        size: Decimal,
        reason: String,
    },
    CancelAllOrders,
}

pub trait Strategy {
    fn name(&self) -> &str;
    fn mode(&self) -> Mode;
    fn tick(&mut self) -> impl std::future::Future<Output = Result<Vec<StrategyAction>>> + Send;
}
