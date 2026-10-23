//! Streams transactions out of a CSV, one row at a time. The input is never loaded whole: one
//! record buffer is reused for every row.

use std::io;

use serde::Deserialize;
use thiserror::Error;

use crate::domain::{Amount, ClientId, ParsedAmount, TxId};
use crate::error::ParseError;
use crate::transaction::{Chargeback, Deposit, Dispute, Resolve, Transaction, TxType, Withdrawal};

/// The input can't be read at all, so the run must stop (A14). A row with bad content is a
/// [`ParseError`] instead, and never stops the run.
#[derive(Debug, Error)]
#[error("can't read the input: {0}")]
pub struct InputError(#[from] csv::Error);

/// A row that became a transaction, and what parsing had to adjust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedRow {
    pub tx: Transaction,
    /// The amount lost a non-zero digit past the 4th decimal place (A10).
    pub amount_truncated: bool,
    /// The row carried an amount that its type ignores (A11).
    pub amount_ignored: bool,
}

/// Reads transactions from CSV.
///
/// Each item is either an [`InputError`], which stops the run, or the result of one row: the
/// transaction it became, or the [`ParseError`] that kept it from becoming one.
pub struct TransactionReader<R> {
    csv: csv::Reader<R>,
    headers: csv::StringRecord,
    record: csv::StringRecord,
}

impl<R: io::Read> TransactionReader<R> {
    /// Reads the header row (A13). Columns are matched by name, ignoring case, so their order
    /// doesn't matter.
    pub fn new(input: R) -> Result<Self, InputError> {
        let mut csv = csv::ReaderBuilder::new()
            // A12: the spec's own example has spaces after the commas.
            .trim(csv::Trim::All)
            // Dispute, resolve and chargeback rows may leave out the amount column.
            .flexible(true)
            .from_reader(input);
        let headers = csv.headers()?.iter().map(str::to_ascii_lowercase).collect();
        Ok(Self {
            csv,
            headers,
            record: csv::StringRecord::new(),
        })
    }
}

impl<R: io::Read> Iterator for TransactionReader<R> {
    type Item = Result<Result<ParsedRow, ParseError>, InputError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.csv.read_record(&mut self.record) {
            Ok(false) => None,
            Ok(true) => Some(Ok(parse_record(&self.record, &self.headers))),
            // Only failing to read the input is fatal (A14); a row that isn't valid CSV, such as
            // one with invalid UTF-8, fails open.
            Err(err) if err.is_io_error() => Some(Err(InputError(err))),
            Err(_) => Some(Ok(Err(ParseError::MalformedRow))),
        }
    }
}

/// A row as text, matched to the columns by header name. Every field is optional, so each
/// missing or broken field gets its own error. An empty field counts as missing.
#[derive(Deserialize)]
struct RawRow<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a str>,
    #[serde(borrow)]
    client: Option<&'a str>,
    #[serde(borrow)]
    tx: Option<&'a str>,
    #[serde(borrow)]
    amount: Option<&'a str>,
}

fn parse_record(
    record: &csv::StringRecord,
    headers: &csv::StringRecord,
) -> Result<ParsedRow, ParseError> {
    if record.len() > headers.len() {
        return Err(ParseError::MalformedRow);
    }
    let raw: RawRow = record
        .deserialize(Some(headers))
        .map_err(|_| ParseError::MalformedRow)?;
    raw.parse()
}

impl RawRow<'_> {
    /// Checks the fields in the order of the README's table: type, client, tx, amount. A row
    /// that fails any of them has no effect at all (A8).
    fn parse(self) -> Result<ParsedRow, ParseError> {
        let tx_type = self
            .kind
            .and_then(TxType::from_name)
            .ok_or(ParseError::UnknownType)?;
        let client = self
            .client
            .and_then(|s| s.parse().ok())
            .map(ClientId)
            .ok_or(ParseError::InvalidClientId)?;
        let tx = self
            .tx
            .and_then(|s| s.parse().ok())
            .map(TxId)
            .ok_or(ParseError::InvalidTxId)?;
        let amount = || Amount::parse(self.amount.ok_or(ParseError::MissingAmount)?);

        let (tx, amount_truncated) = match tx_type {
            TxType::Deposit => {
                let ParsedAmount { amount, truncated } = amount()?;
                let deposit = Deposit::new(client, tx, amount);
                (Transaction::Deposit(deposit), truncated)
            }
            TxType::Withdrawal => {
                let ParsedAmount { amount, truncated } = amount()?;
                let withdrawal = Withdrawal::new(client, tx, amount);
                (Transaction::Withdrawal(withdrawal), truncated)
            }
            TxType::Dispute => (Transaction::Dispute(Dispute::new(client, tx)), false),
            TxType::Resolve => (Transaction::Resolve(Resolve::new(client, tx)), false),
            TxType::Chargeback => (Transaction::Chargeback(Chargeback::new(client, tx)), false),
        };
        // A11: dispute, resolve and chargeback use the amount of the transaction they refer to.
        let has_amount = matches!(tx_type, TxType::Deposit | TxType::Withdrawal);
        Ok(ParsedRow {
            tx,
            amount_truncated,
            amount_ignored: !has_amount && self.amount.is_some(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::amount;

    const HEADER: &str = "type,client,tx,amount\n";

    fn read_bytes(input: &[u8]) -> Vec<Result<ParsedRow, ParseError>> {
        TransactionReader::new(input)
            .unwrap()
            .map(|row| row.expect("readable input"))
            .collect()
    }

    fn read(input: &str) -> Vec<Result<ParsedRow, ParseError>> {
        read_bytes(input.as_bytes())
    }

    /// The single row of `HEADER` followed by `row`.
    fn row(row: &str) -> Result<ParsedRow, ParseError> {
        let mut rows = read(&format!("{HEADER}{row}\n"));
        assert_eq!(rows.len(), 1, "{row:?}");
        rows.remove(0)
    }

    fn tx(row_text: &str) -> Transaction {
        row(row_text)
            .unwrap_or_else(|e| panic!("{row_text:?}: {e}"))
            .tx
    }

    fn deposit(client: u16, tx: u32, raw: &str) -> Transaction {
        Transaction::Deposit(Deposit::new(ClientId(client), TxId(tx), amount(raw)))
    }

    #[test]
    fn reads_the_spec_example() {
        let input = "type, client, tx, amount\n\
                     deposit, 1, 1, 1.0\n\
                     deposit, 2, 2, 2.0\n\
                     deposit, 1, 3, 2.0\n\
                     withdrawal, 1, 4, 1.5\n\
                     withdrawal, 2, 5, 3.0\n";
        let txs: Vec<_> = read(input).into_iter().map(|r| r.unwrap().tx).collect();
        assert_eq!(txs.len(), 5);
        assert_eq!(txs[0], deposit(1, 1, "1.0"));
        assert_eq!(
            txs[4],
            Transaction::Withdrawal(Withdrawal::new(ClientId(2), TxId(5), amount("3.0")))
        );
    }

    #[test]
    fn reads_every_type() {
        assert_eq!(tx("deposit,1,2,3.5"), deposit(1, 2, "3.5"));
        assert_eq!(
            tx("dispute,1,2,"),
            Transaction::Dispute(Dispute::new(ClientId(1), TxId(2)))
        );
        assert_eq!(
            tx("resolve,1,2,"),
            Transaction::Resolve(Resolve::new(ClientId(1), TxId(2)))
        );
        assert_eq!(
            tx("chargeback,1,2,"),
            Transaction::Chargeback(Chargeback::new(ClientId(1), TxId(2)))
        );
    }

    #[test]
    fn a_dispute_row_may_leave_out_the_amount_column() {
        let parsed = row("dispute,1,2").unwrap();
        assert_eq!(
            parsed.tx,
            Transaction::Dispute(Dispute::new(ClientId(1), TxId(2)))
        );
        assert!(!parsed.amount_ignored);
    }

    #[test]
    fn every_parse_error() {
        for (text, expected) in [
            ("deposit,1,1,1.0,extra", ParseError::MalformedRow),
            ("transfer,1,1,1.0", ParseError::UnknownType),
            (",1,1,1.0", ParseError::UnknownType),
            ("deposit,70000,1,1.0", ParseError::InvalidClientId),
            ("deposit,-1,1,1.0", ParseError::InvalidClientId),
            ("deposit,,1,1.0", ParseError::InvalidClientId),
            ("deposit,1,4294967296,1.0", ParseError::InvalidTxId),
            ("deposit,1,abc,1.0", ParseError::InvalidTxId),
            ("dispute,1", ParseError::InvalidTxId),
            ("deposit,1,1", ParseError::MissingAmount),
            ("withdrawal,1,1,", ParseError::MissingAmount),
            ("deposit,1,1,abc", ParseError::InvalidAmount),
            ("deposit,1,1,-1.0", ParseError::NonPositiveAmount),
            ("withdrawal,1,1,0", ParseError::NonPositiveAmount),
            ("deposit,1,1,0.00001", ParseError::AmountTruncatedToZero),
        ] {
            assert_eq!(row(text), Err(expected), "{text:?}");
        }
    }

    #[test]
    fn checks_type_then_client_then_tx_then_amount() {
        assert_eq!(row("transfer,x,y,z"), Err(ParseError::UnknownType));
        assert_eq!(row("deposit,x,y,z"), Err(ParseError::InvalidClientId));
        assert_eq!(row("deposit,1,y,z"), Err(ParseError::InvalidTxId));
        assert_eq!(row("deposit,1,1,z"), Err(ParseError::InvalidAmount));
    }

    #[test]
    fn invalid_utf8_is_a_malformed_row_and_reading_goes_on() {
        let input = b"type,client,tx,amount\ndeposit,1,1,\xff\ndeposit,1,2,1.0\n";
        let rows = read_bytes(input);
        assert_eq!(rows[0], Err(ParseError::MalformedRow));
        assert_eq!(rows[1].unwrap().tx, deposit(1, 2, "1.0"));
    }

    #[test]
    fn a_bad_row_doesnt_stop_the_next_one() {
        let rows = read(&format!("{HEADER}transfer,1,1,1\ndeposit,1,2,1.0\n"));
        assert_eq!(rows[0], Err(ParseError::UnknownType));
        assert_eq!(rows[1].unwrap().tx, deposit(1, 2, "1.0"));
    }

    #[test]
    fn a10_reports_a_truncated_amount() {
        let parsed = row("deposit,1,1,1.23456").unwrap();
        assert_eq!(parsed.tx, deposit(1, 1, "1.2345"));
        assert!(parsed.amount_truncated);
        assert!(!row("deposit,1,1,1.50000").unwrap().amount_truncated);
    }

    #[test]
    fn a11_reports_an_ignored_amount() {
        let parsed = row("dispute,1,1,5.0").unwrap();
        assert_eq!(
            parsed.tx,
            Transaction::Dispute(Dispute::new(ClientId(1), TxId(1)))
        );
        assert!(parsed.amount_ignored);
        assert!(!row("deposit,1,1,5.0").unwrap().amount_ignored);
    }

    #[test]
    fn a12_ignores_case_and_spaces() {
        assert_eq!(tx("  DePoSiT ,  1 , 7 ,  2.5  "), deposit(1, 7, "2.5"));
    }

    #[test]
    fn a13_matches_columns_by_name_ignoring_case() {
        let rows = read(" Amount , TX , Client , Type \n2.5,7,1,deposit\n");
        assert_eq!(rows[0].unwrap().tx, deposit(1, 7, "2.5"));
    }

    #[test]
    fn a13_without_a_header_the_first_row_is_taken_as_the_header() {
        let rows = read("deposit,1,1,1.0\ndeposit,1,2,2.0\n");
        assert_eq!(rows, [Err(ParseError::UnknownType)]);
    }

    #[test]
    fn skips_empty_lines() {
        let rows = read(&format!("{HEADER}\ndeposit,1,1,1.0\n\n"));
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn handles_windows_line_endings() {
        let rows = read("type,client,tx,amount\r\ndeposit,1,1,1.0\r\n");
        assert_eq!(rows[0].unwrap().tx, deposit(1, 1, "1.0"));
    }

    /// Serves `data`, then fails, like a disk that goes away halfway through a file.
    struct FailingInput {
        data: &'static [u8],
    }

    impl io::Read for FailingInput {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.data.is_empty() {
                return Err(io::Error::other("disk gone"));
            }
            let n = self.data.len().min(buf.len());
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    #[test]
    fn a14_failing_to_read_the_input_is_fatal() {
        let mut reader = TransactionReader::new(FailingInput {
            data: b"type,client,tx,amount\n",
        })
        .unwrap();
        assert!(matches!(reader.next(), Some(Err(InputError(_)))));

        let empty = FailingInput { data: b"" };
        assert!(TransactionReader::new(empty).is_err());
    }
}
