//! A withdrawal: a debit from the client's account.

use super::{Operation, Snapshot, Verified, ensure_new_id, ensure_unlocked};
use crate::datastore::{TxWrite, WriteBatch};
use crate::domain::lifecycle::{self, TxKind};
use crate::domain::{Account, Amount, ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// A debit from the client's account: available and total go down by the amount. Fails if the
/// client doesn't have enough available funds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Withdrawal {
    pub(super) client: ClientId,
    pub(super) tx: TxId,
    pub(super) amount: Amount,
}

impl Withdrawal {
    pub fn new(client: ClientId, tx: TxId, amount: Amount) -> Self {
        Self { client, tx, amount }
    }
}

impl Operation for Withdrawal {
    type Checked = Account;

    fn verify(&self, snap: Snapshot) -> Result<Verified<Account>, Rejection> {
        ensure_new_id(&snap)?; // duplicate_tx_id (A7)
        ensure_unlocked(&snap.account)?; // account_locked (A4)
        // A6: a withdrawal can never make available negative.
        if snap.account.available() < self.amount.value() {
            return Err(Rejection::InsufficientFunds);
        }
        Ok(Verified(snap.account))
    }

    fn apply(&self, Verified(account): Verified<Account>) -> Result<WriteBatch, Overflow> {
        let (account, record) = lifecycle::open(account, TxKind::Withdrawal, self.tx, self.amount)?;
        Ok(WriteBatch::new(account, TxWrite::Insert(record)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lifecycle::{Event, TxState};
    use crate::test_support::{amount, dec, funded, locked_account};
    use crate::transaction::Transaction;

    fn withdrawal(tx: u32, raw: &str) -> Transaction {
        Transaction::Withdrawal(Withdrawal::new(ClientId(1), TxId(tx), amount(raw)))
    }

    /// A snapshot of client 1 with `raw` available and a new tx id.
    fn with_funds(raw: &str) -> Snapshot {
        Snapshot {
            account: funded(ClientId(1), 1, raw).0,
            stored_tx: None,
        }
    }

    #[test]
    fn debits_the_account_and_records_the_withdrawal() {
        let batch = withdrawal(2, "4").evaluate(with_funds("10")).unwrap();
        let account = batch.account.unwrap();
        assert_eq!(account.available(), dec("6"));
        assert_eq!(account.total(), dec("6"));
        let Some(TxWrite::Insert(record)) = batch.tx else {
            panic!("expected an insert, got {:?}", batch.tx);
        };
        assert_eq!(record.id(), TxId(2));
        assert_eq!(record.kind(), TxKind::Withdrawal);
        assert_eq!(record.state(), TxState::Normal);
    }

    #[test]
    fn can_withdraw_everything_available() {
        let batch = withdrawal(2, "1.5").evaluate(with_funds("1.5")).unwrap();
        assert_eq!(batch.account.unwrap().available(), dec("0"));
    }

    #[test]
    fn rejects_insufficient_funds() {
        assert_eq!(
            withdrawal(2, "1.5001").evaluate(with_funds("1.5")),
            Err(Rejection::InsufficientFunds)
        );
        let empty = Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: None,
        };
        assert_eq!(
            withdrawal(2, "0.0001").evaluate(empty),
            Err(Rejection::InsufficientFunds)
        );
    }

    #[test]
    fn a6_rejects_any_withdrawal_while_available_is_negative() {
        // Deposit 10, withdraw 10, dispute the deposit: available is -10.
        let (account, deposit) = funded(ClientId(1), 1, "10");
        let (account, _) =
            lifecycle::open(account, TxKind::Withdrawal, TxId(2), amount("10")).unwrap();
        let step = lifecycle::next(&deposit, Event::Dispute).unwrap();
        let (owing, _) = lifecycle::advance(account, deposit, step).unwrap();
        assert_eq!(owing.available(), dec("-10"));

        let snap = Snapshot {
            account: owing,
            stored_tx: None,
        };
        assert_eq!(
            withdrawal(3, "1").evaluate(snap),
            Err(Rejection::InsufficientFunds)
        );
    }

    #[test]
    fn checks_the_id_then_the_lock_then_the_funds() {
        let (_, used) = funded(ClientId(1), 2, "1");
        let locked = locked_account(ClientId(1));

        let duplicate_and_locked = Snapshot {
            account: locked,
            stored_tx: Some(used),
        };
        assert_eq!(
            withdrawal(2, "5").evaluate(duplicate_and_locked),
            Err(Rejection::DuplicateTxId)
        );

        let locked_and_broke = Snapshot {
            account: locked,
            stored_tx: None,
        };
        assert_eq!(
            withdrawal(2, "5").evaluate(locked_and_broke),
            Err(Rejection::AccountLocked)
        );
    }

    #[test]
    fn a7_a_rejected_withdrawal_leaves_a_rejected_record() {
        let record = withdrawal(3, "1").rejected_record().unwrap();
        assert_eq!(record.kind(), TxKind::Withdrawal);
        assert_eq!(record.state(), TxState::Rejected);
    }
}
