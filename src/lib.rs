//! A small payments engine: reads transactions from CSV, applies them to client accounts one at
//! a time, and writes the final state of every account as CSV.
//!
//! The design, and the assumptions (A1-A14) cited throughout the code, are in the README.

pub mod datastore;
pub mod domain;
pub mod engine;
pub mod error;
pub mod io;
pub mod metrics;
pub mod transaction;

#[cfg(test)]
mod test_support;

use std::io::{Read, Write};

use thiserror::Error;

use crate::datastore::{MemoryStore, Store};
use crate::engine::{Engine, Outcome};
use crate::error::StoreError;
use crate::io::reader::{InputError, ParsedRow, TransactionReader};
use crate::io::writer::{OutputError, write_accounts};
use crate::metrics::{Event, Metrics};

/// Why a run stopped (A14). A row with bad content never stops it.
#[derive(Debug, Error)]
pub enum RunError {
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Output(#[from] OutputError),
    /// The final accounts couldn't be read from the store.
    #[error("can't read the accounts: {0}")]
    Store(#[from] StoreError),
}

/// Streams `input` through the engine one row at a time, writes every account to `output`, and
/// counts what happened in `metrics`.
///
/// Rows fail open: a row that doesn't parse, a rejected transaction or a failed commit is counted,
/// and the run goes on with the next row. Only failing to read the input or to write the output
/// stops it (A14).
pub fn run(
    input: impl Read,
    output: impl Write,
    metrics: &mut impl Metrics,
) -> Result<(), RunError> {
    let mut engine = Engine::new(MemoryStore::default());
    for row in TransactionReader::new(input)? {
        let row = row?;
        metrics.record(Event::RowRead);
        match row {
            Ok(parsed) => process_row(&mut engine, parsed, metrics),
            Err(reason) => metrics.record(Event::ParseFailed(reason)),
        }
    }
    write_accounts(output, engine.store().all_accounts()?)?;
    Ok(())
}

/// Processes one parsed row and counts what happened to it.
fn process_row(engine: &mut Engine<impl Store>, row: ParsedRow, metrics: &mut impl Metrics) {
    let tx_type = row.tx.tx_type();
    if row.amount_truncated {
        metrics.record(Event::AmountTruncated(tx_type));
    }
    if row.amount_ignored {
        metrics.record(Event::AmountIgnored(tx_type));
    }
    match engine.process(&row.tx) {
        Ok(Outcome::Applied { high_risk_flagged }) => {
            metrics.record(Event::Applied(tx_type));
            if high_risk_flagged {
                metrics.record(Event::HighRiskFlagged);
            }
        }
        Ok(Outcome::Rejected(reason)) => metrics.record(Event::Rejected(tx_type, reason)),
        // Nothing was written. Count it and go on; a real engine would set it aside to retry.
        Err(_) => metrics.record(Event::StoreError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastore::WriteBatch;
    use crate::domain::lifecycle::StoredTx;
    use crate::domain::{Account, ClientId, TxId};
    use crate::error::Rejection;
    use crate::metrics::{Family, MemoryMetrics};
    use crate::test_support::amount;
    use crate::transaction::{Deposit, Transaction, TxType};

    fn run_str(input: &str) -> (String, MemoryMetrics) {
        let mut output = Vec::new();
        let mut metrics = MemoryMetrics::default();
        run(input.as_bytes(), &mut output, &mut metrics).unwrap();
        (String::from_utf8(output).unwrap(), metrics)
    }

    #[test]
    fn runs_the_spec_example() {
        let (output, metrics) = run_str(
            "type, client, tx, amount\n\
             deposit, 1, 1, 1.0\n\
             deposit, 2, 2, 2.0\n\
             deposit, 1, 3, 2.0\n\
             withdrawal, 1, 4, 1.5\n\
             withdrawal, 2, 5, 3.0\n",
        );
        assert_eq!(
            output,
            "client,available,held,total,locked\n\
             1,1.5000,0.0000,1.5000,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
        assert_eq!(metrics.count(Event::RowRead), 5);
        assert_eq!(metrics.count(Event::Applied(TxType::Deposit)), 3);
        assert_eq!(
            metrics.count(Event::Rejected(
                TxType::Withdrawal,
                Rejection::InsufficientFunds
            )),
            1
        );
    }

    #[test]
    fn counts_every_row_once() {
        let (_, metrics) = run_str(
            "type,client,tx,amount\n\
             deposit,1,1,1.23456\n\
             transfer,1,2,1.0\n\
             dispute,1,1,9.9\n\
             dispute,1,1,\n",
        );
        assert_eq!(metrics.total(Family::RowsRead), 4);
        assert_eq!(metrics.total(Family::RowsParseFailed), 1);
        assert_eq!(metrics.total(Family::TransactionsApplied), 2);
        assert_eq!(metrics.total(Family::TransactionsRejected), 1);
        assert_eq!(metrics.count(Event::AmountTruncated(TxType::Deposit)), 1);
        assert_eq!(metrics.count(Event::AmountIgnored(TxType::Dispute)), 1);
    }

    #[test]
    fn counts_the_high_risk_flag() {
        let (output, metrics) = run_str(
            "type,client,tx,amount\n\
             deposit,1,1,10\n\
             withdrawal,1,2,10\n\
             dispute,1,1,\n",
        );
        assert!(
            output.contains("1,-10.0000,10.0000,0.0000,false"),
            "{output}"
        );
        assert_eq!(metrics.count(Event::HighRiskFlagged), 1);
    }

    /// A store whose commits always fail.
    #[derive(Default)]
    struct BrokenStore;

    impl Store for BrokenStore {
        fn get_account(&self, _: ClientId) -> Result<Option<Account>, StoreError> {
            Ok(None)
        }

        fn get_tx(&self, _: TxId) -> Result<Option<StoredTx>, StoreError> {
            Ok(None)
        }

        fn all_accounts(&self) -> Result<Vec<Account>, StoreError> {
            Ok(Vec::new())
        }

        fn commit(&mut self, _: WriteBatch) -> Result<(), StoreError> {
            Err(StoreError::Unavailable("down".into()))
        }
    }

    #[test]
    fn a_store_error_is_counted_instead_of_the_outcome() {
        let mut engine = Engine::new(BrokenStore);
        let mut metrics = MemoryMetrics::default();
        let deposit = Transaction::Deposit(Deposit::new(ClientId(1), TxId(1), amount("1")));
        let row = ParsedRow {
            tx: deposit,
            amount_truncated: false,
            amount_ignored: false,
        };
        process_row(&mut engine, row, &mut metrics);
        assert_eq!(metrics.count(Event::StoreError), 1);
        assert_eq!(metrics.total(Family::TransactionsApplied), 0);
        assert_eq!(metrics.total(Family::TransactionsRejected), 0);
    }
}
