//! Writes the accounts as CSV.

use std::io;

use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;

use crate::domain::{Account, SCALE};

/// The output can't be written, so the run must stop (A14).
#[derive(Debug, Error)]
#[error("can't write the output: {0}")]
pub struct OutputError(#[from] csv::Error);

/// One row of the output.
#[derive(Serialize)]
struct AccountRow {
    client: u16,
    available: String,
    held: String,
    total: String,
    locked: bool,
}

impl From<&Account> for AccountRow {
    fn from(account: &Account) -> Self {
        Self {
            client: account.client().0,
            available: format_amount(account.available()),
            held: format_amount(account.held()),
            total: format_amount(account.total()),
            locked: account.is_locked(),
        }
    }
}

/// Writes one row per account, sorted by client id, with amounts to 4 decimal places. The
/// header is written even when there are no accounts.
pub fn write_accounts<W: io::Write>(
    output: W,
    mut accounts: Vec<Account>,
) -> Result<(), OutputError> {
    // The spec doesn't care about order; sorting makes the output the same on every run.
    accounts.sort_by_key(Account::client);
    let mut csv = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(output);
    csv.write_record(["client", "available", "held", "total", "locked"])?;
    for account in &accounts {
        csv.serialize(AccountRow::from(account))?;
    }
    csv.flush().map_err(csv::Error::from)?;
    Ok(())
}

/// An amount with exactly 4 decimal places, e.g. `1.5000`.
fn format_amount(value: Decimal) -> String {
    // A subtraction can give a negative zero; print it as plain zero.
    let mut value = if value.is_zero() {
        Decimal::ZERO
    } else {
        value
    };
    value.rescale(SCALE);
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ClientId;
    use crate::domain::TxId;
    use crate::domain::lifecycle::{self, Event, TxKind};
    use crate::test_support::{amount, dec, funded, locked_account, take};

    fn written(accounts: Vec<Account>) -> String {
        let mut out = Vec::new();
        write_accounts(&mut out, accounts).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn writes_the_spec_example_sorted_with_four_decimals() {
        let (one, _) = funded(ClientId(1), 1, "1.5");
        let (two, _) = funded(ClientId(2), 2, "2");
        assert_eq!(
            written(vec![two, one]),
            "client,available,held,total,locked\n\
             1,1.5000,0.0000,1.5000,false\n\
             2,2.0000,0.0000,2.0000,false\n"
        );
    }

    #[test]
    fn writes_only_the_header_without_accounts() {
        assert_eq!(written(vec![]), "client,available,held,total,locked\n");
    }

    #[test]
    fn writes_held_negative_and_locked_accounts() {
        // Deposit 10, withdraw 10, dispute the deposit: -10 available, 10 held (A6).
        let (account, deposit) = funded(ClientId(3), 1, "10");
        let (account, _) =
            lifecycle::open(account, TxKind::Withdrawal, TxId(2), amount("10")).unwrap();
        let (owing, _) = take(account, deposit, Event::Dispute);
        let locked = locked_account(ClientId(4));
        assert_eq!(
            written(vec![owing, locked]),
            "client,available,held,total,locked\n\
             3,-10.0000,10.0000,0.0000,false\n\
             4,0.0000,0.0000,0.0000,true\n"
        );
    }

    #[test]
    fn never_prints_a_negative_zero() {
        assert_eq!(format_amount(-dec("0")), "0.0000");
        assert_eq!(format_amount(dec("-0.0000")), "0.0000");
    }

    #[test]
    fn keeps_all_four_decimals() {
        assert_eq!(format_amount(dec("1.2345")), "1.2345");
        assert_eq!(
            format_amount(dec("1000000000000000000000000")),
            "1000000000000000000000000.0000"
        );
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
    fn a14_failing_to_write_is_an_error() {
        let (account, _) = funded(ClientId(1), 1, "1");
        assert!(write_accounts(ClosedOutput, vec![account]).is_err());
    }
}
