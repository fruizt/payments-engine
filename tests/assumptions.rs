//! One test per assumption in the README (A1-A14), each named after its id. They go through the
//! library's `run`, the same path the binary takes, and check the output CSV and the metrics.

use std::collections::BTreeMap;
use std::io;

use payments_engine::error::{ParseError, Rejection};
use payments_engine::metrics::{Event, MemoryMetrics};
use payments_engine::transaction::TxType;
use payments_engine::{RunError, run};

/// One output row: available, held, total, locked.
type Row = (String, String, String, bool);

struct Processed {
    accounts: BTreeMap<u16, Row>,
    metrics: MemoryMetrics,
}

impl Processed {
    fn account(&self, client: u16) -> (&str, &str, &str, bool) {
        let (available, held, total, locked) = self
            .accounts
            .get(&client)
            .unwrap_or_else(|| panic!("client {client} isn't in the output"));
        (available, held, total, *locked)
    }

    fn rejected(&self, tx_type: TxType, reason: Rejection) -> u64 {
        self.metrics.count(Event::Rejected(tx_type, reason))
    }

    fn parse_failed(&self, reason: ParseError) -> u64 {
        self.metrics.count(Event::ParseFailed(reason))
    }
}

/// Runs the engine on `rows` under the usual header.
fn process(rows: &str) -> Processed {
    process_input(&format!("type,client,tx,amount\n{rows}"))
}

fn process_input(input: &str) -> Processed {
    let mut output = Vec::new();
    let mut metrics = MemoryMetrics::default();
    run(input.as_bytes(), &mut output, &mut metrics).expect("the run succeeds");
    let accounts = csv::Reader::from_reader(output.as_slice())
        .records()
        .map(|record| {
            let r = record.unwrap();
            let row = (
                r[1].to_owned(),
                r[2].to_owned(),
                r[3].to_owned(),
                &r[4] == "true",
            );
            (r[0].parse().unwrap(), row)
        })
        .collect();
    Processed { accounts, metrics }
}

const ZERO: (&str, &str, &str, bool) = ("0.0000", "0.0000", "0.0000", false);

#[test]
fn a01_rows_are_processed_in_file_order() {
    let run = process(
        "dispute,1,1,\n\
         deposit,1,1,5.0\n\
         dispute,1,1,\n",
    );
    // The first dispute came before the deposit existed.
    assert_eq!(run.rejected(TxType::Dispute, Rejection::TxNotFound), 1);
    assert_eq!(run.metrics.count(Event::Applied(TxType::Dispute)), 1);
    assert_eq!(run.account(1), ("0.0000", "5.0000", "5.0000", false));
}

#[test]
fn a02_only_deposits_can_be_disputed() {
    let run = process(
        "deposit,1,1,10.0\n\
         withdrawal,1,2,4.0\n\
         dispute,1,2,\n",
    );
    assert_eq!(run.rejected(TxType::Dispute, Rejection::NotDisputable), 1);
    assert_eq!(run.account(1), ("6.0000", "0.0000", "6.0000", false));
}

#[test]
fn a03_dispute_on_other_clients_tx_is_rejected() {
    let run = process(
        "deposit,1,1,10.0\n\
         dispute,2,1,\n",
    );
    assert_eq!(run.rejected(TxType::Dispute, Rejection::ClientMismatch), 1);
    assert_eq!(run.account(1), ("10.0000", "0.0000", "10.0000", false));
    assert_eq!(run.account(2), ZERO);
}

#[test]
fn a03_resolve_on_other_clients_tx_is_rejected() {
    let run = process(
        "deposit,1,1,10.0\n\
         dispute,1,1,\n\
         resolve,2,1,\n",
    );
    assert_eq!(run.rejected(TxType::Resolve, Rejection::ClientMismatch), 1);
    assert_eq!(run.account(1), ("0.0000", "10.0000", "10.0000", false));
}

#[test]
fn a03_chargeback_on_other_clients_tx_is_rejected() {
    let run = process(
        "deposit,1,1,10.0\n\
         dispute,1,1,\n\
         chargeback,2,1,\n",
    );
    assert_eq!(
        run.rejected(TxType::Chargeback, Rejection::ClientMismatch),
        1
    );
    assert_eq!(run.account(1), ("0.0000", "10.0000", "10.0000", false));
    assert_eq!(run.account(2), ZERO);
}

#[test]
fn a04_a_locked_account_rejects_everything() {
    let run = process(
        "deposit,1,1,10.0\n\
         deposit,1,2,5.0\n\
         dispute,1,1,\n\
         chargeback,1,1,\n\
         deposit,1,3,1.0\n\
         withdrawal,1,4,1.0\n\
         dispute,1,2,\n\
         resolve,1,2,\n\
         chargeback,1,2,\n",
    );
    for tx_type in TxType::ALL {
        assert_eq!(
            run.rejected(tx_type, Rejection::AccountLocked),
            1,
            "{tx_type:?}"
        );
    }
    assert_eq!(run.account(1), ("5.0000", "0.0000", "5.0000", true));
}

#[test]
fn a05_a_resolved_transaction_cant_be_disputed_again() {
    let run = process(
        "deposit,1,1,10.0\n\
         dispute,1,1,\n\
         resolve,1,1,\n\
         dispute,1,1,\n",
    );
    assert_eq!(run.rejected(TxType::Dispute, Rejection::AlreadyResolved), 1);
    assert_eq!(run.account(1), ("10.0000", "0.0000", "10.0000", false));
}

#[test]
fn a06_a_dispute_may_push_available_below_zero_and_flags_the_account() {
    let run = process(
        "deposit,1,1,10.0\n\
         withdrawal,1,2,10.0\n\
         dispute,1,1,\n\
         withdrawal,1,3,1.0\n\
         deposit,1,4,3.0\n",
    );
    assert_eq!(run.metrics.count(Event::HighRiskFlagged), 1);
    // While available is negative no withdrawal goes through, but deposits do.
    assert_eq!(
        run.rejected(TxType::Withdrawal, Rejection::InsufficientFunds),
        1
    );
    assert_eq!(run.account(1), ("-7.0000", "10.0000", "3.0000", false));
}

#[test]
fn a07_tx_ids_are_unique_even_for_rejected_transactions() {
    let run = process(
        "withdrawal,1,1,5.0\n\
         deposit,1,1,5.0\n\
         resolve,1,1,\n\
         deposit,2,1,5.0\n",
    );
    assert_eq!(
        run.rejected(TxType::Withdrawal, Rejection::InsufficientFunds),
        1
    );
    // The rejected withdrawal keeps id 1, for its own client and for every other one.
    assert_eq!(run.rejected(TxType::Deposit, Rejection::DuplicateTxId), 2);
    assert_eq!(run.rejected(TxType::Resolve, Rejection::TxRejected), 1);
    assert_eq!(run.account(1), ZERO);
    assert_eq!(run.account(2), ZERO);
}

#[test]
fn a08_a_row_that_fails_parsing_has_no_effect() {
    let run = process(
        "deposit,9,1,abc\n\
         deposit,1,1,2.0\n",
    );
    assert_eq!(run.parse_failed(ParseError::InvalidAmount), 1);
    // Client 9 wasn't created, and tx id 1 was still free.
    assert!(!run.accounts.contains_key(&9));
    assert_eq!(run.account(1), ("2.0000", "0.0000", "2.0000", false));
}

#[test]
fn a09_clients_are_created_on_their_first_valid_row() {
    let run = process(
        "withdrawal,3,1,5.0\n\
         dispute,4,99,\n",
    );
    assert_eq!(run.account(3), ZERO);
    assert_eq!(run.account(4), ZERO);
}

#[test]
fn a10_extra_decimals_are_cut_not_rounded() {
    let run = process(
        "deposit,1,1,1.23456789\n\
         deposit,1,2,0.99999\n\
         deposit,1,3,2.50000\n",
    );
    // 1.2345 + 0.9999 + 2.5. Trailing zeros don't count as a cut.
    assert_eq!(run.account(1), ("4.7344", "0.0000", "4.7344", false));
    assert_eq!(
        run.metrics.count(Event::AmountTruncated(TxType::Deposit)),
        2
    );
}

#[test]
fn a10_amounts_must_be_positive() {
    let run = process(
        "deposit,1,1,0\n\
         withdrawal,1,2,-1\n\
         deposit,1,3,0.00001\n\
         deposit,1,4,1.0\n",
    );
    assert_eq!(run.parse_failed(ParseError::NonPositiveAmount), 2);
    assert_eq!(run.parse_failed(ParseError::AmountTruncatedToZero), 1);
    assert_eq!(run.account(1), ("1.0000", "0.0000", "1.0000", false));
}

#[test]
fn a10_amounts_and_balances_stay_within_the_limit() {
    let run = process(
        "deposit,1,1,1000000000000000000000000.0001\n\
         deposit,1,2,1000000000000000000000000\n\
         deposit,1,3,0.0001\n",
    );
    assert_eq!(run.parse_failed(ParseError::InvalidAmount), 1);
    assert_eq!(run.rejected(TxType::Deposit, Rejection::BalanceOverflow), 1);
    let limit = "1000000000000000000000000.0000";
    assert_eq!(run.account(1), (limit, "0.0000", limit, false));
}

#[test]
fn a11_amounts_on_dispute_rows_are_ignored() {
    let run = process(
        "deposit,1,1,10.0\n\
         dispute,1,1,999.0\n",
    );
    assert_eq!(run.metrics.count(Event::AmountIgnored(TxType::Dispute)), 1);
    assert_eq!(run.account(1), ("0.0000", "10.0000", "10.0000", false));
}

#[test]
fn a12_type_names_ignore_case_and_surrounding_spaces() {
    let run = process(
        "  DEPOSIT , 1 , 1 , 1.0 \n\
         Withdrawal,1,2,0.5\n",
    );
    assert_eq!(run.account(1), ("0.5000", "0.0000", "0.5000", false));
}

#[test]
fn a13_columns_are_matched_by_header_name() {
    let run = process_input("Amount, Tx, TYPE, client\n2.5,1,deposit,7\n");
    assert_eq!(run.account(7), ("2.5000", "0.0000", "2.5000", false));
}

#[test]
fn a13_without_a_header_row_nothing_is_processed() {
    let run = process_input("deposit,1,1,1.0\ndeposit,1,2,2.0\n");
    assert!(run.accounts.is_empty());
    assert_eq!(run.parse_failed(ParseError::UnknownType), 1);
}

#[test]
fn a14_bad_rows_never_stop_the_run() {
    let run = process(
        "transfer,1,1,1.0\n\
         deposit,x,2,1.0\n\
         deposit,1,3,1.0,extra\n\
         withdrawal,1,4,5.0\n\
         deposit,1,5,1.0\n",
    );
    assert_eq!(run.metrics.count(Event::Applied(TxType::Deposit)), 1);
    assert_eq!(run.account(1), ("1.0000", "0.0000", "1.0000", false));
}

/// Serves the header and one row, then fails, like a disk that goes away halfway through a file.
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

/// Refuses every write, like a closed pipe.
struct ClosedOutput;

impl io::Write for ClosedOutput {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[test]
fn a14_failing_to_read_the_input_stops_the_run() {
    let input = FailingInput {
        data: b"type,client,tx,amount\ndeposit,1,1,1.0\n",
    };
    let result = run(input, Vec::new(), &mut MemoryMetrics::default());
    assert!(matches!(result, Err(RunError::Input(_))), "{result:?}");
}

#[test]
fn a14_failing_to_write_the_output_stops_the_run() {
    let input = "type,client,tx,amount\ndeposit,1,1,1.0\n".as_bytes();
    let result = run(input, ClosedOutput, &mut MemoryMetrics::default());
    assert!(matches!(result, Err(RunError::Output(_))), "{result:?}");
}
