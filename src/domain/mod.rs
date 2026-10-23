//! The domain: ids, amounts, accounts and the transaction lifecycle. Nothing here does any I/O.

mod account;
pub mod lifecycle;

use std::fmt;

use rust_decimal::Decimal;

use crate::error::ParseError;

pub use account::Account;

/// Decimal places the engine works with.
pub const SCALE: u32 = 4;

/// [`max_value`] in units of 0.0001.
const MAX_UNITS: i128 = 10_i128.pow(28);

/// The largest amount or balance the engine handles: 10^24.
///
/// A `Decimal` holds about 7.9 × 10^28 units of 0.0001, so any two values up to this size add up
/// exactly. Keeping every amount and balance within it means no result is ever rounded.
pub fn max_value() -> Decimal {
    Decimal::from_i128_with_scale(MAX_UNITS, SCALE)
}

/// A client id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientId(pub u16);

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A transaction id. Unique across all transactions, rejected ones included (A7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TxId(pub u32);

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The amount of a deposit or withdrawal: greater than zero, at most 4 decimal places, and at
/// most [`max_value`]. The only way to get one is [`Amount::parse`], so every `Amount` in the
/// engine is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(Decimal);

/// An amount read from the input, and whether digits had to be cut from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedAmount {
    pub amount: Amount,
    /// A non-zero digit past the 4th decimal place was cut (A10). Trailing zeros don't count:
    /// they don't change the amount.
    pub truncated: bool,
}

impl Amount {
    /// Parses an amount like `1.5`, `+2` or `.25` (A10).
    ///
    /// Digits past the 4th decimal place are cut, never rounded, so the engine never moves more
    /// money than the partner asked for. Anything other than digits with at most one `.` and one
    /// leading sign is an [`ParseError::InvalidAmount`], and so is an amount above
    /// [`max_value`].
    pub fn parse(raw: &str) -> Result<ParsedAmount, ParseError> {
        let (negative, unsigned) = match raw.as_bytes().first() {
            Some(b'-') => (true, &raw[1..]),
            Some(b'+') => (false, &raw[1..]),
            _ => (false, raw),
        };
        let (int, frac) = unsigned.split_once('.').unwrap_or((unsigned, ""));
        let is_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if (int.is_empty() && frac.is_empty()) || !is_digits(int) || !is_digits(frac) {
            return Err(ParseError::InvalidAmount);
        }
        if negative {
            // Zero with a minus sign isn't positive either.
            return Err(ParseError::NonPositiveAmount);
        }

        let (kept, cut) = frac.split_at(frac.len().min(SCALE as usize));
        let truncated = cut.bytes().any(|b| b != b'0');
        // The amount in units of 0.0001, e.g. "1.5" becomes 15000. Too many digits for an i128
        // is just a very large amount.
        let width = SCALE as usize;
        let units: i128 = format!("{int}{kept:0<width$}")
            .parse()
            .map_err(|_| ParseError::InvalidAmount)?;
        if units > MAX_UNITS {
            return Err(ParseError::InvalidAmount);
        }
        if units == 0 {
            return Err(if truncated {
                ParseError::AmountTruncatedToZero
            } else {
                ParseError::NonPositiveAmount
            });
        }
        Ok(ParsedAmount {
            amount: Amount(Decimal::from_i128_with_scale(units, SCALE)),
            truncated,
        })
    }

    /// The amount as a decimal.
    pub fn value(self) -> Decimal {
        self.0
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> (Decimal, bool) {
        let parsed = Amount::parse(raw).unwrap_or_else(|e| panic!("{raw:?}: {e}"));
        (parsed.amount.value(), parsed.truncated)
    }

    fn dec(raw: &str) -> Decimal {
        raw.parse().unwrap()
    }

    #[test]
    fn parses_plain_amounts() {
        for (raw, expected) in [
            ("1", "1"),
            ("1.5", "1.5"),
            ("+2.25", "2.25"),
            (".5", "0.5"),
            ("5.", "5"),
            ("0001.0000", "1"),
            ("0.0001", "0.0001"),
        ] {
            assert_eq!(parsed(raw), (dec(expected), false), "{raw:?}");
        }
    }

    #[test]
    fn a10_cuts_past_four_decimals_without_rounding() {
        assert_eq!(parsed("1.23456789"), (dec("1.2345"), true));
        assert_eq!(parsed("0.99999"), (dec("0.9999"), true));
    }

    #[test]
    fn a10_trailing_zeros_are_not_a_cut() {
        assert_eq!(parsed("1.50000000"), (dec("1.5"), false));
    }

    #[test]
    fn a10_rejects_zero_and_negative_amounts() {
        for raw in ["0", "0.0000", "0.00000", "-1", "-0", "-0.00001"] {
            assert_eq!(
                Amount::parse(raw),
                Err(ParseError::NonPositiveAmount),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn a10_rejects_a_positive_amount_cut_to_zero() {
        assert_eq!(
            Amount::parse("0.00001"),
            Err(ParseError::AmountTruncatedToZero)
        );
    }

    #[test]
    fn rejects_anything_that_isnt_a_plain_decimal() {
        for raw in [
            "", ".", "+", "-", "abc", "1.2.3", "1,5", "1e5", "1_000", " 1", "1 ", "--1", "+-1", "١",
        ] {
            assert_eq!(
                Amount::parse(raw),
                Err(ParseError::InvalidAmount),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn accepts_up_to_the_largest_amount() {
        assert_eq!(parsed("1000000000000000000000000"), (max_value(), false));
        for raw in [
            "1000000000000000000000000.0001",
            "1000000000000000000000001",
            "1000000000000000000000000000000000000000000",
        ] {
            assert_eq!(
                Amount::parse(raw),
                Err(ParseError::InvalidAmount),
                "{raw:?}"
            );
        }
    }
}
