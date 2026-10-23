//! The datastore: a stand-in for a real database.
//!
//! The engine only talks to it through the [`Store`] trait, which is shaped like a database: every
//! call can fail, records go in and out by value, and every write goes through one atomic
//! [`Store::commit`]. Replacing [`MemoryStore`] with a file or a real database doesn't change the
//! engine.

mod memory;

pub use memory::MemoryStore;

use crate::domain::lifecycle::StoredTx;
use crate::domain::{Account, ClientId, TxId};
use crate::error::StoreError;

/// Everything one transaction writes. [`Store::commit`] applies all of it or none of it.
///
/// A transaction writes at most one account and one transaction record, so the batch has exactly
/// those two slots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteBatch {
    pub account: Option<Account>,
    pub tx: Option<TxWrite>,
}

impl WriteBatch {
    /// The batch of an applied transaction, which always writes both.
    pub fn new(account: Account, tx: TxWrite) -> Self {
        Self {
            account: Some(account),
            tx: Some(tx),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.account.is_none() && self.tx.is_none()
    }
}

/// A write to the transaction records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxWrite {
    /// A new deposit or withdrawal. Fails if its id already exists, like a unique key.
    Insert(StoredTx),
    /// A stored transaction in its new state. Fails if it doesn't exist.
    Update(StoredTx),
}

/// The datastore, shaped like a database so a real one could replace the in-memory version.
pub trait Store {
    /// The account of a client, if the client is known.
    fn get_account(&self, client: ClientId) -> Result<Option<Account>, StoreError>;

    /// The stored deposit or withdrawal with this id, if there is one.
    fn get_tx(&self, id: TxId) -> Result<Option<StoredTx>, StoreError>;

    /// Every account, in no particular order. Read once, for the final report.
    fn all_accounts(&self) -> Result<Vec<Account>, StoreError>;

    /// The only write: applies the whole batch, or nothing if any part of it fails.
    ///
    /// Why a single write method: one transaction can write an account and a transaction record.
    /// As two separate writes, the first could succeed and the second fail. A deposit could then
    /// be marked `Disputed` while its funds were never held, and a later resolve would release
    /// funds that were never held, creating money out of nothing. One all-or-nothing commit rules
    /// that out, like a database transaction.
    fn commit(&mut self, batch: WriteBatch) -> Result<(), StoreError>;
}
