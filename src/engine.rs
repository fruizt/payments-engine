//! The core: processes one transaction at a time.
//!
//! Every transaction goes through three steps, and only the first and the last touch the store:
//!
//! 1. **load** the client's account and the stored transaction with the row's tx id;
//! 2. **decide** with `verify` and `apply`, which only see a copy of those records;
//! 3. **commit** the resulting batch in one atomic write.
//!
//! The answer is an [`Outcome`]: applied, or rejected with a reason, like the reply of a card
//! authorizer. The engine records no metrics; the caller counts the outcome.
//!
//! Load, decide and commit are safe because the engine is the only writer of its store. With
//! several writers, the commit would also have to check that the records didn't change since they
//! were loaded.

use crate::datastore::{Store, TxWrite, WriteBatch};
use crate::domain::Account;
use crate::error::{Rejection, StoreError};
use crate::transaction::{Snapshot, Transaction};

/// The answer to one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The transaction went through. `high_risk_flagged` is true if it just flagged the account
    /// as high-risk (A6).
    Applied { high_risk_flagged: bool },
    /// The rules refused the transaction.
    Rejected(Rejection),
}

/// Processes transactions against a store, one at a time.
#[derive(Debug)]
pub struct Engine<S> {
    store: S,
}

impl<S: Store> Engine<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// The store, to read the state after processing.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Processes one transaction.
    ///
    /// A rejection is a normal answer, not an error: `Err` only means the store failed, and then
    /// nothing was written.
    pub fn process(&mut self, tx: &Transaction) -> Result<Outcome, StoreError> {
        // 1. Load: the only reads.
        let known = self.store.get_account(tx.client())?;
        let account = known.unwrap_or_else(|| Account::new(tx.client())); // A9
        let snap = Snapshot {
            account,
            stored_tx: self.store.get_tx(tx.tx_id())?,
        };

        // 2. Decide: nothing is written yet. Applied or rejected, the result is one batch.
        let (batch, outcome) = match tx.evaluate(snap) {
            Ok(batch) => {
                let flagged =
                    batch.account.is_some_and(|a| a.is_high_risk()) && !account.is_high_risk();
                let outcome = Outcome::Applied {
                    high_risk_flagged: flagged,
                };
                (batch, outcome)
            }
            Err(rejection) => {
                let batch = rejected_writes(tx, rejection, known.is_none(), account);
                (batch, Outcome::Rejected(rejection))
            }
        };

        // 3. Commit: the only write, all or nothing.
        if !batch.is_empty() {
            self.store.commit(batch)?;
        }
        Ok(outcome)
    }
}

/// What a rejected transaction still has to write.
fn rejected_writes(
    tx: &Transaction,
    rejection: Rejection,
    new_client: bool,
    account: Account,
) -> WriteBatch {
    WriteBatch {
        // A9: the client exists even if its first transaction was rejected.
        account: new_client.then_some(account),
        // A7: a rejected deposit or withdrawal keeps its id taken, unless the id already belonged
        // to another transaction.
        tx: match rejection {
            Rejection::DuplicateTxId => None,
            _ => tx.rejected_record().map(TxWrite::Insert),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastore::MemoryStore;
    use crate::domain::lifecycle::{StoredTx, TxKind, TxState};
    use crate::domain::{ClientId, TxId};
    use crate::test_support::{amount, dec};
    use crate::transaction::{Chargeback, Deposit, Dispute, Resolve, Withdrawal};

    const APPLIED: Outcome = Outcome::Applied {
        high_risk_flagged: false,
    };

    fn deposit(client: u16, tx: u32, raw: &str) -> Transaction {
        Transaction::Deposit(Deposit::new(ClientId(client), TxId(tx), amount(raw)))
    }

    fn withdrawal(client: u16, tx: u32, raw: &str) -> Transaction {
        Transaction::Withdrawal(Withdrawal::new(ClientId(client), TxId(tx), amount(raw)))
    }

    fn dispute(client: u16, tx: u32) -> Transaction {
        Transaction::Dispute(Dispute::new(ClientId(client), TxId(tx)))
    }

    fn resolve(client: u16, tx: u32) -> Transaction {
        Transaction::Resolve(Resolve::new(ClientId(client), TxId(tx)))
    }

    fn chargeback(client: u16, tx: u32) -> Transaction {
        Transaction::Chargeback(Chargeback::new(ClientId(client), TxId(tx)))
    }

    fn engine() -> Engine<MemoryStore> {
        Engine::new(MemoryStore::default())
    }

    fn account(engine: &Engine<impl Store>, client: u16) -> Account {
        engine
            .store()
            .get_account(ClientId(client))
            .unwrap()
            .expect("known client")
    }

    fn record(engine: &Engine<impl Store>, tx: u32) -> StoredTx {
        engine.store().get_tx(TxId(tx)).unwrap().expect("stored tx")
    }

    fn rejected(rejection: Rejection) -> Result<Outcome, StoreError> {
        Ok(Outcome::Rejected(rejection))
    }

    #[test]
    fn processes_the_spec_example() {
        let mut engine = engine();
        for tx in [
            deposit(1, 1, "1.0"),
            deposit(2, 2, "2.0"),
            deposit(1, 3, "2.0"),
            withdrawal(1, 4, "1.5"),
        ] {
            assert_eq!(engine.process(&tx), Ok(APPLIED), "{tx:?}");
        }
        assert_eq!(
            engine.process(&withdrawal(2, 5, "3.0")),
            rejected(Rejection::InsufficientFunds)
        );
        assert_eq!(account(&engine, 1).total(), dec("1.5"));
        assert_eq!(account(&engine, 2).total(), dec("2"));
    }

    #[test]
    fn a_dispute_moves_the_stored_transaction_through_its_states() {
        let mut engine = engine();
        engine.process(&deposit(1, 1, "10")).unwrap();

        assert_eq!(engine.process(&dispute(1, 1)), Ok(APPLIED));
        assert_eq!(record(&engine, 1).state(), TxState::Disputed);
        assert_eq!(account(&engine, 1).held(), dec("10"));

        assert_eq!(engine.process(&resolve(1, 1)), Ok(APPLIED));
        assert_eq!(record(&engine, 1).state(), TxState::Resolved);
        assert_eq!(account(&engine, 1).available(), dec("10"));
    }

    #[test]
    fn a4_after_a_chargeback_the_account_rejects_everything() {
        let mut engine = engine();
        engine.process(&deposit(1, 1, "10")).unwrap();
        engine.process(&dispute(1, 1)).unwrap();
        assert_eq!(engine.process(&chargeback(1, 1)), Ok(APPLIED));
        assert!(account(&engine, 1).is_locked());
        assert_eq!(
            engine.process(&deposit(1, 2, "5")),
            rejected(Rejection::AccountLocked)
        );
    }

    #[test]
    fn a7_a9_a_rejected_withdrawal_creates_the_client_and_keeps_its_id() {
        let mut engine = engine();
        assert_eq!(
            engine.process(&withdrawal(3, 1, "5")),
            rejected(Rejection::InsufficientFunds)
        );
        assert_eq!(account(&engine, 3), Account::new(ClientId(3)));
        assert_eq!(record(&engine, 1).state(), TxState::Rejected);

        assert_eq!(
            engine.process(&deposit(3, 1, "5")),
            rejected(Rejection::DuplicateTxId)
        );
        assert_eq!(
            engine.process(&resolve(3, 1)),
            rejected(Rejection::TxRejected)
        );
    }

    #[test]
    fn a7_a_duplicate_keeps_the_first_record() {
        let mut engine = engine();
        engine.process(&deposit(1, 1, "10")).unwrap();
        assert_eq!(
            engine.process(&deposit(2, 1, "5")),
            rejected(Rejection::DuplicateTxId)
        );
        let first = record(&engine, 1);
        assert_eq!(first.client(), ClientId(1));
        assert_eq!(first.amount(), amount("10"));
        assert_eq!(engine.store().transactions().count(), 1);
        // A9: client 2 still exists.
        assert_eq!(account(&engine, 2), Account::new(ClientId(2)));
    }

    #[test]
    fn a_rejection_for_a_known_client_writes_nothing() {
        let mut engine = engine();
        engine.process(&deposit(1, 1, "10")).unwrap();
        let accounts = engine.store().all_accounts().unwrap();
        let txs: Vec<StoredTx> = engine.store().transactions().copied().collect();

        assert_eq!(
            engine.process(&dispute(1, 99)),
            rejected(Rejection::TxNotFound)
        );
        assert_eq!(engine.store().all_accounts().unwrap(), accounts);
        assert!(engine.store().transactions().copied().eq(txs));
    }

    #[test]
    fn a6_reports_the_high_risk_flag_only_when_it_is_set() {
        let mut engine = engine();
        engine.process(&deposit(1, 1, "10")).unwrap();
        engine.process(&deposit(1, 2, "10")).unwrap();
        engine.process(&withdrawal(1, 3, "20")).unwrap();

        assert_eq!(
            engine.process(&dispute(1, 1)),
            Ok(Outcome::Applied {
                high_risk_flagged: true
            })
        );
        // Already flagged: the second dispute doesn't report it again.
        assert_eq!(engine.process(&dispute(1, 2)), Ok(APPLIED));
        let account = account(&engine, 1);
        assert_eq!(
            (account.available(), account.held()),
            (dec("-20"), dec("20"))
        );
        assert!(account.is_high_risk());
    }

    #[test]
    fn an_overflowing_deposit_is_recorded_as_rejected() {
        let mut engine = engine();
        engine
            .process(&deposit(1, 1, "1000000000000000000000000"))
            .unwrap();
        assert_eq!(
            engine.process(&deposit(1, 2, "1")),
            rejected(Rejection::BalanceOverflow)
        );
        let second = record(&engine, 2);
        assert_eq!(
            (second.kind(), second.state()),
            (TxKind::Deposit, TxState::Rejected)
        );
        assert_eq!(
            engine.process(&dispute(1, 2)),
            rejected(Rejection::TxRejected)
        );
    }

    /// A store that fails one chosen commit, like a database that briefly goes away.
    struct FlakyStore {
        inner: MemoryStore,
        commits: usize,
        fail_at: usize,
    }

    impl Store for FlakyStore {
        fn get_account(&self, client: ClientId) -> Result<Option<Account>, StoreError> {
            self.inner.get_account(client)
        }

        fn get_tx(&self, id: TxId) -> Result<Option<StoredTx>, StoreError> {
            self.inner.get_tx(id)
        }

        fn all_accounts(&self) -> Result<Vec<Account>, StoreError> {
            self.inner.all_accounts()
        }

        fn commit(&mut self, batch: WriteBatch) -> Result<(), StoreError> {
            self.commits += 1;
            if self.commits == self.fail_at {
                return Err(StoreError::Unavailable("connection lost".into()));
            }
            self.inner.commit(batch)
        }
    }

    #[test]
    fn a_failed_commit_leaves_no_trace() {
        let mut engine = Engine::new(FlakyStore {
            inner: MemoryStore::default(),
            commits: 0,
            fail_at: 1,
        });
        assert_eq!(
            engine.process(&deposit(1, 1, "10")),
            Err(StoreError::Unavailable("connection lost".into()))
        );
        assert_eq!(engine.store().get_account(ClientId(1)).unwrap(), None);

        // The next transaction sees the state from before the failure...
        assert_eq!(
            engine.process(&withdrawal(1, 2, "10")),
            rejected(Rejection::InsufficientFunds)
        );
        // ...and the failed deposit never took its id.
        assert_eq!(engine.process(&deposit(1, 1, "10")), Ok(APPLIED));
        assert_eq!(account(&engine, 1).available(), dec("10"));
    }
}
