//! The five transaction types.
//!
//! Each row becomes a [`Transaction`]: one variant per type, each wrapping its own struct. Every
//! struct implements [`Operation`], in two steps:
//!
//! - `verify` checks the rules against a [`Snapshot`] of the records. It can't change anything.
//! - `apply` takes what `verify` returned and calculates the [`WriteBatch`] to commit.
//!
//! Neither one touches the datastore: the engine loads the snapshot and commits the batch. What a
//! step does to a stored transaction and to the balances is decided by the
//! [lifecycle](crate::domain::lifecycle); the checks here are about the row and the account.

mod chargeback;
mod deposit;
mod dispute;
mod resolve;
mod withdrawal;

pub use deposit::Deposit;
pub use withdrawal::Withdrawal;

use crate::datastore::WriteBatch;
use crate::domain::lifecycle::{self, StoredTx, TxKind};
use crate::domain::{Account, ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// The type of a transaction, as named in the input's `type` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TxType {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

impl TxType {
    pub const ALL: [Self; 5] = [
        Self::Deposit,
        Self::Withdrawal,
        Self::Dispute,
        Self::Resolve,
        Self::Chargeback,
    ];

    /// The name in the input, and the `type` label in the metrics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
            Self::Withdrawal => "withdrawal",
            Self::Dispute => "dispute",
            Self::Resolve => "resolve",
            Self::Chargeback => "chargeback",
        }
    }

    /// The type with this name, ignoring case (A12).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(name))
    }
}

/// One transaction from the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transaction {
    Deposit(Deposit),
    Withdrawal(Withdrawal),
}

impl Transaction {
    pub fn tx_type(&self) -> TxType {
        match self {
            Self::Deposit(_) => TxType::Deposit,
            Self::Withdrawal(_) => TxType::Withdrawal,
        }
    }

    pub fn client(&self) -> ClientId {
        match self {
            Self::Deposit(t) => t.client,
            Self::Withdrawal(t) => t.client,
        }
    }

    /// The row's tx id: the id of a new deposit or withdrawal, or the id of the transaction a
    /// dispute, resolve or chargeback refers to.
    pub fn tx_id(&self) -> TxId {
        match self {
            Self::Deposit(t) => t.tx,
            Self::Withdrawal(t) => t.tx,
        }
    }

    /// Runs `verify`, then `apply`. Nothing is written: the engine commits the batch.
    pub fn evaluate(&self, snap: Snapshot) -> Result<WriteBatch, Rejection> {
        match self {
            Self::Deposit(t) => run(t, snap),
            Self::Withdrawal(t) => run(t, snap),
        }
    }

    /// The record a rejected deposit or withdrawal still leaves, so its id stays taken (A7).
    pub fn rejected_record(&self) -> Option<StoredTx> {
        match self {
            Self::Deposit(t) => Some(lifecycle::rejected(
                TxKind::Deposit,
                t.tx,
                t.client,
                t.amount,
            )),
            Self::Withdrawal(t) => Some(lifecycle::rejected(
                TxKind::Withdrawal,
                t.tx,
                t.client,
                t.amount,
            )),
        }
    }
}

/// What a transaction gets to look at. The engine loads it from the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Snapshot {
    /// The row's client account: new and empty if the client wasn't known yet (A9).
    pub account: Account,
    /// The stored transaction with the row's tx id, if there is one.
    /// - deposit/withdrawal: must be `None`, otherwise the id is a duplicate (A7)
    /// - dispute/resolve/chargeback: the transaction being referenced
    pub stored_tx: Option<StoredTx>,
}

/// Proof that `verify` passed, holding the records it checked. Only code in this module can
/// create one, so `apply` can never run on records that weren't checked.
#[derive(Debug)]
pub struct Verified<T>(T);

/// The two steps every transaction type implements.
pub trait Operation {
    /// The records `verify` checked and hands to `apply`.
    type Checked;

    /// Checks the rules against a copy of the records. Can't change anything.
    fn verify(&self, snap: Snapshot) -> Result<Verified<Self::Checked>, Rejection>;

    /// Calculates what to write. The only way it can still fail is an arithmetic overflow.
    fn apply(&self, checked: Verified<Self::Checked>) -> Result<WriteBatch, Overflow>;
}

fn run<O: Operation>(op: &O, snap: Snapshot) -> Result<WriteBatch, Rejection> {
    let verified = op.verify(snap)?;
    op.apply(verified)
        .map_err(|Overflow| Rejection::BalanceOverflow)
}

/// A7: a deposit or withdrawal needs an id nobody has used, not even a rejected transaction.
fn ensure_new_id(snap: &Snapshot) -> Result<(), Rejection> {
    match snap.stored_tx {
        Some(_) => Err(Rejection::DuplicateTxId),
        None => Ok(()),
    }
}

/// A4: a locked account rejects every transaction.
fn ensure_unlocked(account: &Account) -> Result<(), Rejection> {
    if account.is_locked() {
        Err(Rejection::AccountLocked)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::amount;

    #[test]
    fn type_names_are_the_labels() {
        for t in TxType::ALL {
            assert_eq!(TxType::from_name(t.label()), Some(t));
        }
    }

    #[test]
    fn a12_type_names_ignore_case() {
        assert_eq!(TxType::from_name("DEPOSIT"), Some(TxType::Deposit));
        assert_eq!(TxType::from_name("ChargeBack"), Some(TxType::Chargeback));
    }

    #[test]
    fn unknown_type_names_are_none() {
        for name in ["", "transfer", "deposits", "with drawal"] {
            assert_eq!(TxType::from_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn a_transaction_knows_its_type_client_and_id() {
        let tx = Transaction::Withdrawal(Withdrawal::new(ClientId(4), TxId(9), amount("1")));
        assert_eq!(tx.tx_type(), TxType::Withdrawal);
        assert_eq!((tx.client(), tx.tx_id()), (ClientId(4), TxId(9)));
    }
}
