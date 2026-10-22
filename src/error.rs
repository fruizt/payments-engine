//! Every way a row or a transaction can fail, each with the label it gets in the metrics.
//!
//! Each enum maps its variants to labels with a `match` that lists every variant, so a new error
//! case without a label doesn't compile.

use thiserror::Error;

/// Why a CSV row never became a transaction. Counted in `rows_parse_failed_total{reason}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum ParseError {
    /// The row can't be read: invalid UTF-8, or more fields than the header.
    #[error("the row can't be read")]
    MalformedRow,
    /// `type` isn't one of the five transaction types.
    #[error("unknown transaction type")]
    UnknownType,
    /// `client` is missing or isn't a `u16`.
    #[error("the client id is missing or isn't a u16")]
    InvalidClientId,
    /// `tx` is missing or isn't a `u32`.
    #[error("the tx id is missing or isn't a u32")]
    InvalidTxId,
    /// A deposit or withdrawal has no amount.
    #[error("a deposit or withdrawal has no amount")]
    MissingAmount,
    /// The amount isn't a decimal number, or is too large to represent.
    #[error("the amount isn't a decimal number, or is too large")]
    InvalidAmount,
    /// The amount is zero or negative.
    #[error("the amount is zero or negative")]
    NonPositiveAmount,
    /// The amount is positive but becomes zero when cut to 4 decimal places (A10).
    #[error("the amount becomes zero when cut to 4 decimal places")]
    AmountTruncatedToZero,
}

impl ParseError {
    /// Every variant, in the order of the README's table.
    pub const ALL: [Self; 8] = [
        Self::MalformedRow,
        Self::UnknownType,
        Self::InvalidClientId,
        Self::InvalidTxId,
        Self::MissingAmount,
        Self::InvalidAmount,
        Self::NonPositiveAmount,
        Self::AmountTruncatedToZero,
    ];

    /// The `reason` label in the metrics.
    pub fn label(self) -> &'static str {
        match self {
            Self::MalformedRow => "malformed_row",
            Self::UnknownType => "unknown_type",
            Self::InvalidClientId => "invalid_client_id",
            Self::InvalidTxId => "invalid_tx_id",
            Self::MissingAmount => "missing_amount",
            Self::InvalidAmount => "invalid_amount",
            Self::NonPositiveAmount => "non_positive_amount",
            Self::AmountTruncatedToZero => "amount_truncated_to_zero",
        }
    }
}

/// Why a valid transaction was refused by the rules. Counted in
/// `transactions_rejected_total{type, reason}`.
///
/// A rejection is a normal answer, not a failure of the engine: the row was fine, the rules said
/// no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Error)]
pub enum Rejection {
    /// Any type: the client's account is locked (A4).
    #[error("the account is locked")]
    AccountLocked,
    /// Withdrawal: available funds are less than the amount.
    #[error("insufficient available funds")]
    InsufficientFunds,
    /// Deposit or withdrawal: the tx id was already used, even by a rejected transaction (A7).
    #[error("the tx id was already used")]
    DuplicateTxId,
    /// Dispute, resolve or chargeback: no stored transaction has that id.
    #[error("the referenced transaction doesn't exist")]
    TxNotFound,
    /// Dispute, resolve or chargeback: the referenced transaction belongs to another client (A3).
    #[error("the referenced transaction belongs to another client")]
    ClientMismatch,
    /// Dispute, resolve or chargeback: the referenced deposit or withdrawal was itself rejected.
    #[error("the referenced transaction was rejected")]
    TxRejected,
    /// Dispute: the referenced transaction is a withdrawal (A2).
    #[error("only deposits can be disputed")]
    NotDisputable,
    /// Dispute: the referenced transaction is already under dispute.
    #[error("the referenced transaction is already under dispute")]
    AlreadyDisputed,
    /// Dispute: the referenced transaction was disputed and resolved before (A5).
    #[error("the referenced transaction was already disputed and resolved")]
    AlreadyResolved,
    /// Dispute, resolve or chargeback: the referenced transaction was charged back.
    ///
    /// Unreachable today, because a chargeback locks the account and `AccountLocked` is checked
    /// first. Kept so the lifecycle covers every state.
    #[error("the referenced transaction was charged back")]
    AlreadyChargedBack,
    /// Resolve or chargeback: the referenced transaction isn't under dispute.
    #[error("the referenced transaction isn't under dispute")]
    NotUnderDispute,
    /// Any type that changes a balance: the new balance would be too large to represent.
    #[error("the balance would be too large to represent")]
    BalanceOverflow,
}

impl Rejection {
    /// Every variant, in the order of the README's table.
    pub const ALL: [Self; 12] = [
        Self::AccountLocked,
        Self::InsufficientFunds,
        Self::DuplicateTxId,
        Self::TxNotFound,
        Self::ClientMismatch,
        Self::TxRejected,
        Self::NotDisputable,
        Self::AlreadyDisputed,
        Self::AlreadyResolved,
        Self::AlreadyChargedBack,
        Self::NotUnderDispute,
        Self::BalanceOverflow,
    ];

    /// The `reason` label in the metrics.
    pub fn label(self) -> &'static str {
        match self {
            Self::AccountLocked => "account_locked",
            Self::InsufficientFunds => "insufficient_funds",
            Self::DuplicateTxId => "duplicate_tx_id",
            Self::TxNotFound => "tx_not_found",
            Self::ClientMismatch => "client_mismatch",
            Self::TxRejected => "tx_rejected",
            Self::NotDisputable => "not_disputable",
            Self::AlreadyDisputed => "already_disputed",
            Self::AlreadyResolved => "already_resolved",
            Self::AlreadyChargedBack => "already_charged_back",
            Self::NotUnderDispute => "not_under_dispute",
            Self::BalanceOverflow => "balance_overflow",
        }
    }
}

/// A balance would grow beyond what the engine represents exactly. The only way `apply` can
/// still fail; it becomes [`Rejection::BalanceOverflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the balance would be too large to represent")]
pub struct Overflow;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn assert_valid_labels(labels: &[&str]) {
        let unique: HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "duplicate label in {labels:?}");
        for label in labels {
            assert!(
                !label.is_empty() && label.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "label {label:?} isn't snake_case"
            );
        }
    }

    #[test]
    fn parse_error_labels_are_unique_and_snake_case() {
        let labels: Vec<_> = ParseError::ALL.iter().map(|e| e.label()).collect();
        assert_valid_labels(&labels);
    }

    #[test]
    fn rejection_labels_are_unique_and_snake_case() {
        let labels: Vec<_> = Rejection::ALL.iter().map(|r| r.label()).collect();
        assert_valid_labels(&labels);
    }
}
