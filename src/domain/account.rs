//! A client's account.

use rust_decimal::Decimal;

use super::{ClientId, max_value};
use crate::error::Overflow;

/// A client's account.
///
/// `total` isn't stored: it's always `available + held`, so the three numbers can never disagree.
/// The balances change only through `adjust`, which only code inside `domain` can
/// call. In practice that's the lifecycle, the single source of truth for which money moves when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Account {
    client: ClientId,
    available: Decimal,
    held: Decimal,
    locked: bool,
    high_risk: bool,
}

impl Account {
    /// A new, empty account.
    pub fn new(client: ClientId) -> Self {
        Self {
            client,
            available: Decimal::ZERO,
            held: Decimal::ZERO,
            locked: false,
            high_risk: false,
        }
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    /// Funds available for trading, staking, withdrawal, etc. Negative when a dispute held funds
    /// the client had already withdrawn: what the client owes the platform (A6).
    pub fn available(&self) -> Decimal {
        self.available
    }

    /// Funds held for dispute.
    pub fn held(&self) -> Decimal {
        self.held
    }

    /// Available plus held funds.
    pub fn total(&self) -> Decimal {
        // Can't overflow: `adjust` keeps the total within `max_value()`.
        self.available + self.held
    }

    /// Locked by a chargeback. Every later transaction for this client is rejected (A4).
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Flagged because a dispute pushed available below zero (A6).
    pub fn is_high_risk(&self) -> bool {
        self.high_risk
    }

    /// Changes the balances by the given amounts (negative to take money away).
    ///
    /// Fails if available, held or total would go beyond `max_value()`. Every value stays within
    /// that limit, so the arithmetic itself is always exact.
    pub(super) fn adjust(
        self,
        available_change: Decimal,
        held_change: Decimal,
    ) -> Result<Self, Overflow> {
        let available = self
            .available
            .checked_add(available_change)
            .ok_or(Overflow)?;
        let held = self.held.checked_add(held_change).ok_or(Overflow)?;
        let total = available.checked_add(held).ok_or(Overflow)?;
        let max = max_value();
        if available.abs() > max || held.abs() > max || total.abs() > max {
            return Err(Overflow);
        }
        // Invariant 2: held only ever holds the amount of disputes that are still open.
        debug_assert!(held >= Decimal::ZERO, "held funds went negative: {held}");
        Ok(Self {
            available,
            held,
            ..self
        })
    }

    /// Freezes the account (spec: a chargeback freezes the account immediately).
    pub(super) fn lock(&mut self) {
        self.locked = true;
    }

    /// Marks the account as high-risk (A6).
    pub(super) fn flag_high_risk(&mut self) {
        self.high_risk = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(raw: &str) -> Decimal {
        raw.parse().unwrap()
    }

    fn account() -> Account {
        Account::new(ClientId(1))
    }

    #[test]
    fn a_new_account_is_empty() {
        let account = account();
        assert_eq!(account.client(), ClientId(1));
        assert_eq!(account.available(), Decimal::ZERO);
        assert_eq!(account.held(), Decimal::ZERO);
        assert_eq!(account.total(), Decimal::ZERO);
        assert!(!account.is_locked());
        assert!(!account.is_high_risk());
    }

    #[test]
    fn adjust_moves_both_balances_and_total_follows() {
        let account = account().adjust(dec("10"), Decimal::ZERO).unwrap();
        let account = account.adjust(dec("-4"), dec("4")).unwrap();
        assert_eq!(account.available(), dec("6"));
        assert_eq!(account.held(), dec("4"));
        assert_eq!(account.total(), dec("10"));
    }

    #[test]
    fn available_may_go_negative() {
        let account = account().adjust(dec("-2.5"), dec("2.5")).unwrap();
        assert_eq!(account.available(), dec("-2.5"));
        assert_eq!(account.total(), Decimal::ZERO);
    }

    #[test]
    fn adjust_accepts_the_largest_value() {
        let account = account().adjust(max_value(), Decimal::ZERO).unwrap();
        assert_eq!(account.total(), max_value());
    }

    #[test]
    fn adjust_fails_beyond_the_largest_value_and_changes_nothing() {
        let full = account().adjust(max_value(), Decimal::ZERO).unwrap();
        assert_eq!(full.adjust(dec("0.0001"), Decimal::ZERO), Err(Overflow));
        assert_eq!(full.available(), max_value());

        let owing = account().adjust(-max_value(), max_value()).unwrap();
        assert_eq!(owing.adjust(dec("-0.0001"), Decimal::ZERO), Err(Overflow));
    }

    #[test]
    fn adjust_fails_when_only_the_total_would_be_too_large() {
        let half = max_value() / dec("2");
        let account = account().adjust(half, half).unwrap();
        assert_eq!(account.adjust(Decimal::ZERO, dec("0.0001")), Err(Overflow));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "held funds went negative")]
    fn held_going_negative_is_a_bug() {
        let _ = account().adjust(Decimal::ZERO, dec("-1"));
    }

    #[test]
    fn lock_and_flag_only_change_their_flag() {
        let mut account = account().adjust(dec("3"), Decimal::ZERO).unwrap();
        account.lock();
        account.flag_high_risk();
        assert!(account.is_locked());
        assert!(account.is_high_risk());
        assert_eq!(account.total(), dec("3"));
    }
}
