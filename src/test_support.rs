//! Helpers shared by the unit tests.

use rust_decimal::Decimal;

use crate::domain::lifecycle::{self, Event, StoredTx, TxKind};
use crate::domain::{Account, Amount, ClientId, TxId};

/// An amount from a literal, e.g. `amount("1.5")`.
pub fn amount(raw: &str) -> Amount {
    Amount::parse(raw).expect("valid amount").amount
}

/// A decimal from a literal, e.g. `dec("-2.5")`.
pub fn dec(raw: &str) -> Decimal {
    raw.parse().expect("valid decimal")
}

/// A new account for `client` after one deposit, and that deposit.
pub fn funded(client: ClientId, tx: u32, raw: &str) -> (Account, StoredTx) {
    lifecycle::open(Account::new(client), TxKind::Deposit, TxId(tx), amount(raw))
        .expect("deposit fits")
}

/// A stored transaction after taking the step for `event`, and its account.
pub fn take(account: Account, tx: StoredTx, event: Event) -> (Account, StoredTx) {
    let step = lifecycle::next(&tx, event).expect("allowed step");
    lifecycle::advance(account, tx, step).expect("step fits")
}

/// An account for `client` whose only deposit was charged back: locked, with nothing left.
pub fn locked_account(client: ClientId) -> Account {
    let (account, deposit) = funded(client, u32::MAX, "1");
    let (account, deposit) = take(account, deposit, Event::Dispute);
    take(account, deposit, Event::Chargeback).0
}

/// A client with two deposits, 10.0 (tx 1) and 5.0 (tx 2), after tx 1 was disputed and charged
/// back: the account is locked with 5.0 available. Returns the account, tx 1 (charged back) and
/// tx 2 (still normal).
pub fn locked_after_chargeback(client: ClientId) -> (Account, StoredTx, StoredTx) {
    let (account, first) = funded(client, 1, "10");
    let (account, second) =
        lifecycle::open(account, TxKind::Deposit, TxId(2), amount("5")).expect("deposit fits");
    let (account, first) = take(account, first, Event::Dispute);
    let (account, first) = take(account, first, Event::Chargeback);
    (account, first, second)
}
