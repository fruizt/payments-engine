//! The in-memory datastore.

use std::collections::HashMap;

use super::{Store, TxWrite, WriteBatch};
use crate::domain::lifecycle::StoredTx;
use crate::domain::{Account, ClientId, TxId};
use crate::error::StoreError;

/// Keeps everything in memory.
///
/// Transactions are kept in arrival order in a `Vec`, with a `HashMap` from id to position: that
/// gives both an order (any two transactions can be compared by position) and O(1) lookup by id.
#[derive(Debug, Default)]
pub struct MemoryStore {
    txs: Vec<StoredTx>,
    tx_index: HashMap<TxId, usize>,
    accounts: HashMap<ClientId, Account>,
}

impl MemoryStore {
    /// Every stored transaction, in arrival order.
    pub fn transactions(&self) -> impl Iterator<Item = &StoredTx> {
        self.txs.iter()
    }
}

impl Store for MemoryStore {
    fn get_account(&self, client: ClientId) -> Result<Option<Account>, StoreError> {
        Ok(self.accounts.get(&client).copied())
    }

    fn get_tx(&self, id: TxId) -> Result<Option<StoredTx>, StoreError> {
        Ok(self.tx_index.get(&id).map(|&pos| self.txs[pos]))
    }

    fn all_accounts(&self) -> Result<Vec<Account>, StoreError> {
        Ok(self.accounts.values().copied().collect())
    }

    /// Commits in two phases: first every write is checked, then all of them are made. Only the
    /// first phase can fail, and it changes nothing, so a batch lands completely or not at all.
    /// (Running out of memory in the second phase aborts the whole process, so no half-written
    /// state survives that either.)
    fn commit(&mut self, batch: WriteBatch) -> Result<(), StoreError> {
        // Phase 1: check. Returning here leaves the store exactly as it was.
        match &batch.tx {
            Some(TxWrite::Insert(record)) if self.tx_index.contains_key(&record.id()) => {
                return Err(StoreError::DuplicateKey(record.id()));
            }
            Some(TxWrite::Update(record)) if !self.tx_index.contains_key(&record.id()) => {
                return Err(StoreError::MissingRecord(record.id()));
            }
            _ => {}
        }

        // Phase 2: write. Nothing below can fail.
        match batch.tx {
            Some(TxWrite::Insert(record)) => {
                self.tx_index.insert(record.id(), self.txs.len());
                self.txs.push(record);
            }
            Some(TxWrite::Update(record)) => {
                let pos = self.tx_index[&record.id()]; // checked in phase 1
                self.txs[pos] = record;
            }
            None => {}
        }
        if let Some(account) = batch.account {
            self.accounts.insert(account.client(), account);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::lifecycle::{self, Event, TxKind, TxState};
    use crate::test_support::amount;

    /// A deposit of 10.0 by client 1, and the account after it.
    fn deposit(id: u32) -> (Account, StoredTx) {
        lifecycle::open(
            Account::new(ClientId(1)),
            TxKind::Deposit,
            TxId(id),
            amount("10"),
        )
        .unwrap()
    }

    fn store_with_deposit() -> MemoryStore {
        let mut store = MemoryStore::default();
        let (account, record) = deposit(1);
        store
            .commit(WriteBatch::new(account, TxWrite::Insert(record)))
            .unwrap();
        store
    }

    #[test]
    fn a_commit_writes_the_account_and_the_record() {
        let store = store_with_deposit();
        let (account, record) = deposit(1);
        assert_eq!(store.get_account(ClientId(1)).unwrap(), Some(account));
        assert_eq!(store.get_tx(TxId(1)).unwrap(), Some(record));
        assert_eq!(store.get_tx(TxId(2)).unwrap(), None);
        assert_eq!(store.all_accounts().unwrap(), vec![account]);
    }

    #[test]
    fn an_update_replaces_the_record() {
        let mut store = store_with_deposit();
        let (account, record) = deposit(1);
        let step = lifecycle::next(&record, Event::Dispute).unwrap();
        let (account, disputed) = lifecycle::advance(account, record, step).unwrap();
        store
            .commit(WriteBatch::new(account, TxWrite::Update(disputed)))
            .unwrap();
        assert_eq!(
            store.get_tx(TxId(1)).unwrap().unwrap().state(),
            TxState::Disputed
        );
        assert_eq!(store.transactions().count(), 1);
    }

    #[test]
    fn a_duplicate_insert_writes_nothing_not_even_the_account() {
        let mut store = store_with_deposit();
        let other_client = Account::new(ClientId(7));
        let (_, same_id) = deposit(1);
        let batch = WriteBatch::new(other_client, TxWrite::Insert(same_id));
        assert_eq!(store.commit(batch), Err(StoreError::DuplicateKey(TxId(1))));
        assert_eq!(store.get_account(ClientId(7)).unwrap(), None);
        assert_eq!(store.transactions().count(), 1);
    }

    #[test]
    fn an_update_of_a_missing_record_writes_nothing() {
        let mut store = MemoryStore::default();
        let (account, record) = deposit(1);
        let batch = WriteBatch::new(account, TxWrite::Update(record));
        assert_eq!(store.commit(batch), Err(StoreError::MissingRecord(TxId(1))));
        assert_eq!(store.get_account(ClientId(1)).unwrap(), None);
    }

    #[test]
    fn keeps_transactions_in_arrival_order() {
        let mut store = MemoryStore::default();
        for id in [5, 2, 9] {
            let (account, record) = deposit(id);
            store
                .commit(WriteBatch::new(account, TxWrite::Insert(record)))
                .unwrap();
        }
        let ids: Vec<_> = store.transactions().map(StoredTx::id).collect();
        assert_eq!(ids, [TxId(5), TxId(2), TxId(9)]);
    }

    #[test]
    fn an_empty_batch_changes_nothing() {
        let mut store = store_with_deposit();
        store.commit(WriteBatch::default()).unwrap();
        assert_eq!(store.transactions().count(), 1);
        assert_eq!(store.all_accounts().unwrap().len(), 1);
    }
}
