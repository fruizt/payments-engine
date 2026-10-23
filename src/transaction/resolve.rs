//! A resolve: the end of a dispute that releases the held funds.

use super::{Operation, Snapshot, StepChecked, Verified, apply_step, verify_step};
use crate::datastore::WriteBatch;
use crate::domain::lifecycle::Event;
use crate::domain::{ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// The end of a dispute in the client's disfavour: the deposit stands, and its held funds are
/// available again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolve {
    pub(super) client: ClientId,
    pub(super) tx: TxId,
}

impl Resolve {
    pub fn new(client: ClientId, tx: TxId) -> Self {
        Self { client, tx }
    }
}

impl Operation for Resolve {
    type Checked = StepChecked;

    fn verify(&self, snap: Snapshot) -> Result<Verified<StepChecked>, Rejection> {
        verify_step(self.client, snap, Event::Resolve)
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
    use crate::domain::lifecycle::{StoredTx, TxState};
    use crate::test_support::{dec, funded, locked_after_chargeback, take};
    use crate::transaction::Transaction;

    fn resolve(client: u16, tx: u32) -> Transaction {
        Transaction::Resolve(Resolve::new(ClientId(client), TxId(tx)))
    }

    #[test]
    fn a4_a_dispute_on_a_locked_account_can_still_be_resolved() {
        let (locked, _, open) = locked_after_chargeback(ClientId(1));
        let (locked, disputed) = take(locked, open, Event::Dispute);
        let snap = Snapshot {
            account: locked,
            stored_tx: Some(disputed),
        };
        let account = resolve(1, 2).evaluate(snap).unwrap().account.unwrap();
        // The funds are released, but stay in an account the client can't withdraw from.
        assert_eq!(account.available(), dec("5"));
        assert_eq!(account.held(), dec("0"));
        assert!(account.is_locked());
    }

    /// Client `client` with one 10.0 deposit (tx 1) under dispute.
    fn disputed(client: u16) -> (Account, StoredTx) {
        let (account, deposit) = funded(ClientId(client), 1, "10");
        take(account, deposit, Event::Dispute)
    }

    #[test]
    fn releases_the_held_funds() {
        let (account, deposit) = disputed(1);
        let snap = Snapshot {
            account,
            stored_tx: Some(deposit),
        };
        let batch = resolve(1, 1).evaluate(snap).unwrap();
        let account = batch.account.unwrap();
        assert_eq!(account.available(), dec("10"));
        assert_eq!(account.held(), dec("0"));
        let Some(TxWrite::Update(record)) = batch.tx else {
            panic!("expected an update, got {:?}", batch.tx);
        };
        assert_eq!(record.state(), TxState::Resolved);
    }

    #[test]
    fn rejects_a_transaction_that_isnt_under_dispute() {
        let (account, deposit) = funded(ClientId(1), 1, "10");
        let snap = Snapshot {
            account,
            stored_tx: Some(deposit),
        };
        assert_eq!(
            resolve(1, 1).evaluate(snap),
            Err(Rejection::NotUnderDispute)
        );
    }

    #[test]
    fn rejects_an_unknown_transaction() {
        let snap = Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: None,
        };
        assert_eq!(resolve(1, 1).evaluate(snap), Err(Rejection::TxNotFound));
    }

    #[test]
    fn a3_rejects_another_clients_transaction() {
        let (_, theirs) = disputed(2);
        let snap = Snapshot {
            account: Account::new(ClientId(1)),
            stored_tx: Some(theirs),
        };
        assert_eq!(resolve(1, 1).evaluate(snap), Err(Rejection::ClientMismatch));
    }
}
