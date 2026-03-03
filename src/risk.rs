use std::collections::HashMap;
use std::sync::Arc;

use chrono::{NaiveDate, Utc};
use polymarket_client_sdk::types::Decimal;
use tokio::sync::RwLock;

use crate::config::Config;

#[derive(Debug)]
pub enum RiskDecision {
    Approved,
    Rejected(String),
    KillSwitch(String),
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TrackedPosition {
    pub order_id: String,
    pub token_id: String,
    pub price: Decimal,
    pub size: Decimal,
    pub cost_usdc: Decimal,
    pub reason: String,
    pub placed_at: chrono::DateTime<Utc>,
}

struct RiskState {
    daily_exposure: Decimal,
    daily_pnl: Decimal,
    active_positions: HashMap<String, TrackedPosition>,
    tracking_date: NaiveDate,
    killed: bool,
}

pub struct RiskManager {
    state: RwLock<RiskState>,
    config: Arc<Config>,
}

impl RiskManager {
    pub fn new(config: Arc<Config>) -> Self {
        RiskManager {
            state: RwLock::new(RiskState {
                daily_exposure: Decimal::ZERO,
                daily_pnl: Decimal::ZERO,
                active_positions: HashMap::new(),
                tracking_date: Utc::now().date_naive(),
                killed: false,
            }),
            config,
        }
    }

    /// Check whether a trade of the given cost is allowed.
    pub async fn check_trade(&self, cost_usdc: Decimal) -> RiskDecision {
        let mut state = self.state.write().await;
        self.maybe_reset_day_inner(&mut state);

        if state.killed {
            return RiskDecision::KillSwitch(
                "Kill switch active — trading halted for today".into(),
            );
        }

        if cost_usdc > self.config.max_trade_usd {
            return RiskDecision::Rejected(format!(
                "Trade cost ${cost_usdc} exceeds max trade size ${}",
                self.config.max_trade_usd,
            ));
        }

        if state.daily_exposure + cost_usdc > self.config.max_daily_exposure {
            return RiskDecision::Rejected(format!(
                "Would exceed daily exposure limit: current ${} + ${cost_usdc} > ${}",
                state.daily_exposure, self.config.max_daily_exposure,
            ));
        }

        let position_count = state.active_positions.len() as u32;
        if position_count >= self.config.max_active_positions {
            return RiskDecision::Rejected(format!(
                "Max active positions reached: {position_count}/{}",
                self.config.max_active_positions,
            ));
        }

        // Check kill switch loss threshold
        if state.daily_pnl < -self.config.kill_switch_loss {
            state.killed = true;
            return RiskDecision::KillSwitch(format!(
                "Daily loss ${} exceeds kill switch threshold ${}",
                state.daily_pnl, self.config.kill_switch_loss,
            ));
        }

        RiskDecision::Approved
    }

    /// Record a successfully placed order.
    pub async fn record_order(&self, position: TrackedPosition) {
        let mut state = self.state.write().await;
        state.daily_exposure += position.cost_usdc;
        tracing::info!(
            "Risk: recorded order {} — cost=${}, total exposure=${}",
            position.order_id,
            position.cost_usdc,
            state.daily_exposure,
        );
        state
            .active_positions
            .insert(position.order_id.clone(), position);
    }

    /// Record a cancelled order (remove from active, decrement exposure).
    #[allow(dead_code)]
    pub async fn record_cancel(&self, order_id: &str) {
        let mut state = self.state.write().await;
        if let Some(pos) = state.active_positions.remove(order_id) {
            state.daily_exposure -= pos.cost_usdc;
            tracing::info!(
                "Risk: cancelled order {order_id} — returned ${}, total exposure=${}",
                pos.cost_usdc,
                state.daily_exposure,
            );
        }
    }

    /// Record a settlement (remove from active, update P&L).
    #[allow(dead_code)]
    pub async fn record_settlement(&self, order_id: &str, pnl: Decimal) {
        let mut state = self.state.write().await;
        if let Some(pos) = state.active_positions.remove(order_id) {
            state.daily_pnl += pnl;
            state.daily_exposure -= pos.cost_usdc;
            tracing::info!(
                "Risk: settled order {order_id} — pnl=${pnl}, daily pnl=${}, exposure=${}",
                state.daily_pnl,
                state.daily_exposure,
            );
        }
    }

    #[allow(dead_code)]
    pub async fn is_killed(&self) -> bool {
        self.state.read().await.killed
    }

    /// Check whether a paired arb trade is allowed.
    /// Each side is checked individually against max_trade_usd,
    /// but the combined cost is checked against daily exposure.
    pub async fn check_arb_trade(&self, cost_a: Decimal, cost_b: Decimal) -> RiskDecision {
        let mut state = self.state.write().await;
        self.maybe_reset_day_inner(&mut state);

        if state.killed {
            return RiskDecision::KillSwitch(
                "Kill switch active — trading halted for today".into(),
            );
        }

        // Each side must respect per-trade limit
        if cost_a > self.config.max_trade_usd {
            return RiskDecision::Rejected(format!(
                "Arb side A cost ${cost_a} exceeds max trade size ${}",
                self.config.max_trade_usd,
            ));
        }
        if cost_b > self.config.max_trade_usd {
            return RiskDecision::Rejected(format!(
                "Arb side B cost ${cost_b} exceeds max trade size ${}",
                self.config.max_trade_usd,
            ));
        }

        let total = cost_a + cost_b;
        if state.daily_exposure + total > self.config.max_daily_exposure {
            return RiskDecision::Rejected(format!(
                "Would exceed daily exposure limit: current ${} + ${total} > ${}",
                state.daily_exposure, self.config.max_daily_exposure,
            ));
        }

        // Need 2 position slots
        let position_count = state.active_positions.len() as u32;
        if position_count + 2 > self.config.max_active_positions {
            return RiskDecision::Rejected(format!(
                "Not enough position slots for arb: {position_count}+2 > {}",
                self.config.max_active_positions,
            ));
        }

        if state.daily_pnl < -self.config.kill_switch_loss {
            state.killed = true;
            return RiskDecision::KillSwitch(format!(
                "Daily loss ${} exceeds kill switch threshold ${}",
                state.daily_pnl, self.config.kill_switch_loss,
            ));
        }

        RiskDecision::Approved
    }

    /// Get current stats: (exposure, pnl, position_count).
    pub async fn stats(&self) -> (Decimal, Decimal, usize) {
        let state = self.state.read().await;
        (
            state.daily_exposure,
            state.daily_pnl,
            state.active_positions.len(),
        )
    }

    /// Reset state at midnight UTC if the day has changed.
    fn maybe_reset_day_inner(&self, state: &mut RiskState) {
        let today = Utc::now().date_naive();
        if state.tracking_date != today {
            tracing::info!(
                "Risk: new day ({today}) — resetting. Previous day: exposure=${}, pnl=${}",
                state.daily_exposure,
                state.daily_pnl,
            );
            state.daily_exposure = Decimal::ZERO;
            state.daily_pnl = Decimal::ZERO;
            state.active_positions.clear();
            state.tracking_date = today;
            state.killed = false;
        }
    }
}
