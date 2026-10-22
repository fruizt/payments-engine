//! A chargeback: the end of a dispute that reverses the deposit.

use super::{Operation, Snapshot, StepChecked, Verified, apply_step, verify_step};
use crate::datastore::WriteBatch;
use crate::domain::lifecycle::Event;
use crate::domain::{ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// The end of a dispute in the client's favour: the held funds leave the account, and the account
/// is locked immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chargeback {
    pub(super) client: ClientId,
    pub(super) tx: TxId,
}

impl Chargeback {
    pub fn new(client: ClientId, tx: TxId) -> Self {
        Self { client, tx }
    }
}

impl Operation for Chargeback {
    type Checked = StepChecked;

    fn verify(&self, snap: Snapshot) -> Result<Verified<StepChecked>, Rejection> {
        verify_step(self.client, snap, Event::Chargeback)
    }

    fn apply(&self, checked: Verified<StepChecked>) -> Result<WriteBatch, Overflow> {
        apply_step(checked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastore::TxWrite;
    use crate::domain::Account;
    use crate::domain::lifecycle::{self, StoredTx, TxKind, TxState};
    use crate::test_support::{amount, dec, funded, take};
    use crate::transaction::Transaction;

    fn chargeback(client: u16, tx: u32) -> Transaction {
        Transaction::Chargeback(Chargeback::new(ClientId(client), TxId(tx)))
    }

    /// Client `client` with 10.0 (tx 1) and 5.0 (tx 2) deposited, and tx 1 under dispute.
    fn disputed(client: u16) -> (Account, StoredTx) {
        let (account, first) = funded(ClientId(client), 1, "10");
        let (account, _) = lifecycle::open(account, TxKind::Deposit, TxId(2), amount("5")).unwrap();
        take(account, first, Event::Dispute)
    }

    #[test]
    fn reverses_the_held_funds_and_locks_the_account() {
        let (account, deposit) = disputed(1);
        let snap = Snapshot {
            account,
            stored_tx: Some(deposit),
        };
        let batch = chargeback(1, 1).evaluate(snap).unwrap();
        let account = batch.account.unwrap();
        assert_eq!(account.available(), dec("5"));
        assert_eq!(account.held(), dec("0"));
        assert_eq!(account.total(), dec("5"));
        assert!(account.is_locked());
        let Some(TxWrite::Update(record)) = batch.tx else {
            panic!("expected an update, got {:?}", batch.tx);
        };
        assert_eq!(record.state(), TxState::ChargedBack);
    }

    #[test]
    fn rejects_a_transaction_that_isnt_under_dispute() {
        let (account, deposit) = funded(ClientId(1), 1, "10");
        let snap = Snapshot {
            account,
            stored_tx: Some(deposit),
        };
        assert_eq!(
            chargeback(1, 1).evaluate(snap),
            Err(Rejection::NotUnderDispute)
        );
    }

    #[test]
    fn rejects_an_unknown_transaction() {
        let snap = Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: None,
        };
        assert_eq!(chargeback(1, 1).evaluate(snap), Err(Rejection::TxNotFound));
    }

    #[test]
    fn a3_rejects_another_clients_transaction() {
        let (_, theirs) = disputed(2);
        let snap = Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: Some(theirs),
        };
        assert_eq!(
            chargeback(1, 1).evaluate(snap),
            Err(Rejection::ClientMismatch)
        );
    }
}
