//! A deposit: a credit to the client's account.

use super::{Operation, Snapshot, Verified, ensure_new_id, ensure_unlocked};
use crate::datastore::{TxWrite, WriteBatch};
use crate::domain::lifecycle::{self, TxKind};
use crate::domain::{Account, Amount, ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// A credit to the client's account: available and total go up by the amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Deposit {
    pub(super) client: ClientId,
    pub(super) tx: TxId,
    pub(super) amount: Amount,
}

impl Deposit {
    pub fn new(client: ClientId, tx: TxId, amount: Amount) -> Self {
        Self { client, tx, amount }
    }
}

impl Operation for Deposit {
    type Checked = Account;

    fn verify(&self, snap: Snapshot) -> Result<Verified<Account>, Rejection> {
        ensure_new_id(&snap)?; // duplicate_tx_id (A7)
        ensure_unlocked(&snap.account)?; // account_locked (A4)
        Ok(Verified(snap.account))
    }

    fn apply(&self, Verified(account): Verified<Account>) -> Result<WriteBatch, Overflow> {
        let (account, record) = lifecycle::open(account, TxKind::Deposit, self.tx, self.amount)?;
        Ok(WriteBatch::new(account, TxWrite::Insert(record)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lifecycle::TxState;
    use crate::test_support::{amount, dec, funded, locked_account};
    use crate::transaction::Transaction;

    fn deposit(tx: u32, raw: &str) -> Transaction {
        Transaction::Deposit(Deposit::new(ClientId(1), TxId(tx), amount(raw)))
    }

    fn fresh() -> Snapshot {
        Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: None,
        }
    }

    #[test]
    fn credits_the_account_and_records_the_deposit() {
        let batch = deposit(1, "2.5").evaluate(fresh()).unwrap();
        let account = batch.account.unwrap();
        assert_eq!(account.available(), dec("2.5"));
        assert_eq!(account.held(), dec("0"));
        assert_eq!(account.total(), dec("2.5"));
        let Some(TxWrite::Insert(record)) = batch.tx else {
            panic!("expected an insert, got {:?}", batch.tx);
        };
        assert_eq!(record.id(), TxId(1));
        assert_eq!(record.kind(), TxKind::Deposit);
        assert_eq!(record.state(), TxState::Normal);
    }

    #[test]
    fn a7_rejects_an_id_that_was_already_used() {
        let (_, used) = funded(ClientId(2), 1, "1");
        let snap = Snapshot {
            stored_tx: Some(used),
            ..fresh()
        };
        assert_eq!(
            deposit(1, "2.5").evaluate(snap),
            Err(Rejection::DuplicateTxId)
        );
    }

    #[test]
    fn a4_rejects_a_locked_account() {
        let snap = Snapshot {
            account: locked_account(ClientId(1)),
            stored_tx: None,
        };
        assert_eq!(
            deposit(1, "2.5").evaluate(snap),
            Err(Rejection::AccountLocked)
        );
    }

    #[test]
    fn checks_the_id_before_the_lock() {
        let (_, used) = funded(ClientId(1), 1, "1");
        let snap = Snapshot {
            account: locked_account(ClientId(1)),
            stored_tx: Some(used),
        };
        assert_eq!(
            deposit(1, "2.5").evaluate(snap),
            Err(Rejection::DuplicateTxId)
        );
    }

    #[test]
    fn rejects_a_deposit_that_would_overflow() {
        let (full, _) = funded(ClientId(1), 1, "1000000000000000000000000");
        let snap = Snapshot {
            account: full,
            stored_tx: None,
        };
        assert_eq!(
            deposit(2, "0.0001").evaluate(snap),
            Err(Rejection::BalanceOverflow)
        );
    }

    #[test]
    fn a7_a_rejected_deposit_leaves_a_rejected_record() {
        let record = deposit(3, "1").rejected_record().unwrap();
        assert_eq!(record.id(), TxId(3));
        assert_eq!(record.kind(), TxKind::Deposit);
        assert_eq!(record.state(), TxState::Rejected);
    }
}
