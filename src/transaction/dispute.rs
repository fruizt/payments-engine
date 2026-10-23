//! A dispute: a client's claim that a deposit was wrong.

use super::{Operation, Snapshot, StepChecked, Verified, apply_step, verify_step};
use crate::datastore::WriteBatch;
use crate::domain::lifecycle::Event;
use crate::domain::{ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// A client's claim that a deposit was wrong and should be reversed. The deposit's funds are held
/// until the dispute is resolved or charged back. Only deposits can be disputed (A2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dispute {
    pub(super) client: ClientId,
    pub(super) tx: TxId,
}

impl Dispute {
    pub fn new(client: ClientId, tx: TxId) -> Self {
        Self { client, tx }
    }
}

impl Operation for Dispute {
    type Checked = StepChecked;

    fn verify(&self, snap: Snapshot) -> Result<Verified<StepChecked>, Rejection> {
        verify_step(self.client, snap, Event::Dispute)
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
    use crate::test_support::{amount, dec, funded, locked_account};
    use crate::transaction::Transaction;

    fn dispute(client: u16, tx: u32) -> Transaction {
        Transaction::Dispute(Dispute::new(ClientId(client), TxId(tx)))
    }

    fn snapshot(account: Account, stored_tx: Option<StoredTx>) -> Snapshot {
        Snapshot { account, stored_tx }
    }

    #[test]
    fn holds_the_deposits_funds() {
        let (account, deposit) = funded(ClientId(1), 1, "10");
        let batch = dispute(1, 1)
            .evaluate(snapshot(account, Some(deposit)))
            .unwrap();
        let account = batch.account.unwrap();
        assert_eq!(account.available(), dec("0"));
        assert_eq!(account.held(), dec("10"));
        assert_eq!(account.total(), dec("10"));
        let Some(TxWrite::Update(record)) = batch.tx else {
            panic!("expected an update, got {:?}", batch.tx);
        };
        assert_eq!(record.id(), TxId(1));
        assert_eq!(record.state(), TxState::Disputed);
    }

    #[test]
    fn rejects_an_unknown_transaction() {
        let snap = snapshot(Account::new(ClientId(1)), None);
        assert_eq!(dispute(1, 1).evaluate(snap), Err(Rejection::TxNotFound));
    }

    #[test]
    fn a3_rejects_another_clients_transaction() {
        let (_, theirs) = funded(ClientId(2), 1, "10");
        let snap = snapshot(Account::new(ClientId(1)), Some(theirs));
        assert_eq!(dispute(1, 1).evaluate(snap), Err(Rejection::ClientMismatch));
    }

    #[test]
    fn a4_rejects_a_locked_account() {
        let (_, deposit) = funded(ClientId(1), 1, "10");
        let snap = snapshot(locked_account(ClientId(1)), Some(deposit));
        assert_eq!(dispute(1, 1).evaluate(snap), Err(Rejection::AccountLocked));
    }

    #[test]
    fn a2_rejects_a_withdrawal() {
        let (account, _) = funded(ClientId(1), 1, "10");
        let (account, withdrawal) =
            lifecycle::open(account, TxKind::Withdrawal, TxId(2), amount("4")).unwrap();
        let snap = snapshot(account, Some(withdrawal));
        assert_eq!(dispute(1, 2).evaluate(snap), Err(Rejection::NotDisputable));
    }

    #[test]
    fn checks_the_reference_before_the_lock() {
        let locked = locked_account(ClientId(1));
        assert_eq!(
            dispute(1, 1).evaluate(snapshot(locked, None)),
            Err(Rejection::TxNotFound)
        );
        let (_, theirs) = funded(ClientId(2), 1, "10");
        assert_eq!(
            dispute(1, 1).evaluate(snapshot(locked, Some(theirs))),
            Err(Rejection::ClientMismatch)
        );
    }

    #[test]
    fn rejects_a_second_dispute() {
        let (account, deposit) = funded(ClientId(1), 1, "10");
        let batch = dispute(1, 1)
            .evaluate(snapshot(account, Some(deposit)))
            .unwrap();
        let Some(TxWrite::Update(disputed)) = batch.tx else {
            panic!("expected an update");
        };
        let snap = snapshot(batch.account.unwrap(), Some(disputed));
        assert_eq!(
            dispute(1, 1).evaluate(snap),
            Err(Rejection::AlreadyDisputed)
        );
    }
}
