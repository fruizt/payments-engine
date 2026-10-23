//! Counting what happens, like a service exporting Prometheus metrics.
//!
//! The engine doesn't record metrics itself: it returns an outcome for each transaction, and the
//! caller turns it into [`Event`]s. [`MemoryMetrics`] keeps the counts in memory; a real
//! Prometheus exporter would be another implementation of [`Metrics`].
//!
//! Client ids are never labels: one series per client would make the metrics unusable at scale.

mod memory;

pub use memory::MemoryMetrics;

use crate::error::{ParseError, Rejection};
use crate::transaction::TxType;

/// Something worth counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// A row was read from the input.
    RowRead,
    /// A row never became a transaction.
    ParseFailed(ParseError),
    /// A transaction went through.
    Applied(TxType),
    /// The rules refused a transaction.
    Rejected(TxType, Rejection),
    /// An amount lost digits past the 4th decimal place (A10).
    AmountTruncated(TxType),
    /// A dispute, resolve or chargeback row carried an amount, which was ignored (A11).
    AmountIgnored(TxType),
    /// A dispute pushed available below zero and flagged the account (A6).
    HighRiskFlagged,
    /// The datastore failed; the transaction wasn't applied.
    StoreError,
}

impl Event {
    /// The metric this event counts in, and its labels in Prometheus text format.
    pub fn series(self) -> (Family, String) {
        match self {
            Self::RowRead => (Family::RowsRead, String::new()),
            Self::ParseFailed(reason) => (
                Family::RowsParseFailed,
                format!("{{reason=\"{}\"}}", reason.label()),
            ),
            Self::Applied(tx_type) => (Family::TransactionsApplied, type_label(tx_type)),
            Self::Rejected(tx_type, reason) => (
                Family::TransactionsRejected,
                format!(
                    "{{type=\"{}\",reason=\"{}\"}}",
                    tx_type.label(),
                    reason.label()
                ),
            ),
            Self::AmountTruncated(tx_type) => (Family::AmountsTruncated, type_label(tx_type)),
            Self::AmountIgnored(tx_type) => (Family::AmountsIgnored, type_label(tx_type)),
            Self::HighRiskFlagged => (Family::AccountsFlaggedHighRisk, String::new()),
            Self::StoreError => (Family::StoreErrors, String::new()),
        }
    }
}

fn type_label(tx_type: TxType) -> String {
    format!("{{type=\"{}\"}}", tx_type.label())
}

/// A metric. They're printed in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    RowsRead,
    RowsParseFailed,
    TransactionsApplied,
    TransactionsRejected,
    AmountsTruncated,
    AmountsIgnored,
    AccountsFlaggedHighRisk,
    StoreErrors,
}

impl Family {
    pub fn name(self) -> &'static str {
        match self {
            Self::RowsRead => "rows_read_total",
            Self::RowsParseFailed => "rows_parse_failed_total",
            Self::TransactionsApplied => "transactions_applied_total",
            Self::TransactionsRejected => "transactions_rejected_total",
            Self::AmountsTruncated => "amounts_truncated_total",
            Self::AmountsIgnored => "amounts_ignored_total",
            Self::AccountsFlaggedHighRisk => "accounts_flagged_high_risk_total",
            Self::StoreErrors => "store_errors_total",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Self::RowsRead => "Rows read from the input.",
            Self::RowsParseFailed => "Rows that never became a transaction, by reason.",
            Self::TransactionsApplied => "Transactions that went through, by type.",
            Self::TransactionsRejected => "Transactions refused by the rules, by type and reason.",
            Self::AmountsTruncated => "Amounts cut to 4 decimal places, by type.",
            Self::AmountsIgnored => {
                "Amounts sent on dispute, resolve and chargeback rows and ignored, by type."
            }
            Self::AccountsFlaggedHighRisk => {
                "Accounts flagged because a dispute pushed available below zero."
            }
            Self::StoreErrors => "Transactions not applied because the datastore failed.",
        }
    }
}

/// Where events are counted.
pub trait Metrics {
    fn record(&mut self, event: Event);
}
