//! Helpers shared by the unit tests.

use rust_decimal::Decimal;

use crate::domain::Amount;

/// An amount from a literal, e.g. `amount("1.5")`.
pub fn amount(raw: &str) -> Amount {
    Amount::parse(raw).expect("valid amount").amount
}

/// A decimal from a literal, e.g. `dec("-2.5")`.
pub fn dec(raw: &str) -> Decimal {
    raw.parse().expect("valid decimal")
}
