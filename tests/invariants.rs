//! Property tests for the README's invariants.
//!
//! proptest generates random transaction sequences over a few clients and a few tx ids, so
//! disputes, resolves and chargebacks usually point at real transactions (sometimes another
//! client's), and duplicate ids are common. Invariants 1-8 are checked after every single step.

use std::collections::BTreeMap;

use proptest::collection::vec;
use proptest::prelude::*;
use rust_decimal::Decimal;

use payments_engine::datastore::{MemoryStore, Store};
use payments_engine::domain::lifecycle::{StoredTx, TxKind, TxState};
use payments_engine::domain::{Account, Amount, ClientId, TxId};
use payments_engine::engine::{Engine, Outcome};
use payments_engine::metrics::{Family, MemoryMetrics};
use payments_engine::run;
use payments_engine::transaction::{
    Chargeback, Deposit, Dispute, Resolve, Transaction, Withdrawal,
};

const CLIENTS: u16 = 3;
const TX_IDS: u32 = 12;

/// One generated transaction. Amounts are in units of 0.0001.
#[derive(Debug, Clone, Copy)]
enum Op {
    Deposit(u16, u32, u64),
    Withdrawal(u16, u32, u64),
    Dispute(u16, u32),
    Resolve(u16, u32),
    Chargeback(u16, u32),
}

impl Op {
    fn client(self) -> u16 {
        match self {
            Op::Deposit(c, ..)
            | Op::Withdrawal(c, ..)
            | Op::Dispute(c, _)
            | Op::Resolve(c, _)
            | Op::Chargeback(c, _) => c,
        }
    }

    fn tx(self) -> Transaction {
        match self {
            Op::Deposit(c, t, u) => {
                Transaction::Deposit(Deposit::new(ClientId(c), TxId(t), amount(u)))
            }
            Op::Withdrawal(c, t, u) => {
                Transaction::Withdrawal(Withdrawal::new(ClientId(c), TxId(t), amount(u)))
            }
            Op::Dispute(c, t) => Transaction::Dispute(Dispute::new(ClientId(c), TxId(t))),
            Op::Resolve(c, t) => Transaction::Resolve(Resolve::new(ClientId(c), TxId(t))),
            Op::Chargeback(c, t) => Transaction::Chargeback(Chargeback::new(ClientId(c), TxId(t))),
        }
    }

    fn row(self) -> String {
        match self {
            Op::Deposit(c, t, u) => format!("deposit,{c},{t},{}", decimal(u)),
            Op::Withdrawal(c, t, u) => format!("withdrawal,{c},{t},{}", decimal(u)),
            Op::Dispute(c, t) => format!("dispute,{c},{t},"),
            Op::Resolve(c, t) => format!("resolve,{c},{t},"),
            Op::Chargeback(c, t) => format!("chargeback,{c},{t},"),
        }
    }
}

fn decimal(units: u64) -> String {
    format!("{}.{:04}", units / 10_000, units % 10_000)
}

fn amount(units: u64) -> Amount {
    Amount::parse(&decimal(units)).unwrap().amount
}

/// Random transactions. Deposits and withdrawals of 0.0001 to 50.0000.
fn op() -> impl Strategy<Value = Op> {
    let client = 1..=CLIENTS;
    let tx = 1..=TX_IDS;
    let units = 1..=500_000_u64;
    prop_oneof![
        4 => (client.clone(), tx.clone(), units.clone()).prop_map(|(c, t, u)| Op::Deposit(c, t, u)),
        3 => (client.clone(), tx.clone(), units).prop_map(|(c, t, u)| Op::Withdrawal(c, t, u)),
        3 => (client.clone(), tx.clone()).prop_map(|(c, t)| Op::Dispute(c, t)),
        2 => (client.clone(), tx.clone()).prop_map(|(c, t)| Op::Resolve(c, t)),
        1 => (client, tx).prop_map(|(c, t)| Op::Chargeback(c, t)),
    ]
}

/// Like [`op`], but no tx id is used by two clients' deposits or withdrawals: a client's ids are
/// `n * CLIENTS + client`. Disputes, resolves and chargebacks can still point at any client's
/// ids.
fn op_with_ids_per_client() -> impl Strategy<Value = Op> {
    let owned = |n: u32, client: u16| n * u32::from(CLIENTS) + u32::from(client);
    (op(), 1..=CLIENTS).prop_map(move |(op, owner)| match op {
        Op::Deposit(c, n, u) => Op::Deposit(c, owned(n, c), u),
        Op::Withdrawal(c, n, u) => Op::Withdrawal(c, owned(n, c), u),
        Op::Dispute(c, n) => Op::Dispute(c, owned(n, owner)),
        Op::Resolve(c, n) => Op::Resolve(c, owned(n, owner)),
        Op::Chargeback(c, n) => Op::Chargeback(c, owned(n, owner)),
    })
}

/// A CSV row: usually a valid transaction, sometimes one that fails parsing.
fn row() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => op().prop_map(Op::row),
        1 => prop_oneof![
            Just("transfer,1,1,1.0".to_owned()),
            Just("deposit,x,1,1.0".to_owned()),
            Just("deposit,1,1,abc".to_owned()),
            Just("deposit,1,1,1.0,extra".to_owned()),
            Just("deposit,1,1,0.00001".to_owned()),
            Just("withdrawal,1,1,-2".to_owned()),
        ],
    ]
}

/// Everything in the store at one moment.
#[derive(Debug, Clone, PartialEq)]
struct State {
    accounts: BTreeMap<ClientId, Account>,
    txs: Vec<StoredTx>,
}

fn state(engine: &Engine<MemoryStore>) -> State {
    let store = engine.store();
    State {
        accounts: store
            .all_accounts()
            .unwrap()
            .into_iter()
            .map(|account| (account.client(), account))
            .collect(),
        txs: store.transactions().copied().collect(),
    }
}

fn final_accounts(ops: &[Op]) -> BTreeMap<ClientId, Account> {
    let mut engine = Engine::new(MemoryStore::default());
    for op in ops {
        engine.process(&op.tx()).unwrap();
    }
    state(&engine).accounts
}

/// The README's state moves: Normal → Disputed → Resolved | ChargedBack.
fn allowed_move(from: TxState, to: TxState) -> bool {
    use TxState::{ChargedBack, Disputed, Normal, Resolved};
    from == to
        || matches!(
            (from, to),
            (Normal, Disputed) | (Disputed, Resolved) | (Disputed, ChargedBack)
        )
}

/// Invariants 1-8, checked after one step from `before` to `after`.
fn check_step(tx: &Transaction, outcome: Outcome, before: &State, after: &State) {
    for account in after.accounts.values() {
        let own = || after.txs.iter().filter(|t| t.client() == account.client());
        let sum = |kind: TxKind, counts: fn(TxState) -> bool| -> Decimal {
            own()
                .filter(|t| t.kind() == kind && counts(t.state()))
                .map(|t| t.amount().value())
                .sum()
        };

        // 1. total == available + held.
        assert_eq!(account.total(), account.available() + account.held());
        // 2. held >= 0.
        assert!(account.held() >= Decimal::ZERO, "{account:?}");
        // 3. held is the sum of the client's deposits under dispute.
        assert_eq!(
            account.held(),
            sum(TxKind::Deposit, |s| s == TxState::Disputed)
        );
        // 4. Money is conserved.
        let applied = |s| s != TxState::Rejected;
        let deposits = sum(TxKind::Deposit, applied);
        let withdrawals = sum(TxKind::Withdrawal, applied);
        let charged_back = sum(TxKind::Deposit, |s| s == TxState::ChargedBack);
        assert_eq!(
            account.total(),
            deposits - withdrawals - charged_back,
            "{account:?}"
        );
    }

    // A3: a transaction never touches another client's account.
    for (client, was) in &before.accounts {
        if *client != tx.client() {
            assert_eq!(&after.accounts[client], was, "{tx:?}");
        }
    }

    // 5. Only an applied dispute can push available (further) below zero.
    let now = after.accounts[&tx.client()];
    let was = before
        .accounts
        .get(&tx.client())
        .map_or(Decimal::ZERO, Account::available);
    if now.available() < Decimal::ZERO && now.available() < was {
        assert!(
            matches!(tx, Transaction::Dispute(_)) && matches!(outcome, Outcome::Applied { .. }),
            "{tx:?} made available {} ({outcome:?})",
            now.available()
        );
    }

    // 6. A rejection changes no balance and no state. It may only create the client (A9) and
    // record a rejected deposit or withdrawal (A7).
    if let Outcome::Rejected(_) = outcome {
        for (client, account) in &after.accounts {
            match before.accounts.get(client) {
                Some(was) => assert_eq!(account, was, "{tx:?}"),
                None => assert_eq!(*account, Account::new(*client), "{tx:?}"),
            }
        }
        assert_eq!(after.txs[..before.txs.len()], before.txs[..], "{tx:?}");
        let added = &after.txs[before.txs.len()..];
        assert!(added.len() <= 1, "{tx:?}");
        assert!(
            added.iter().all(|t| t.state() == TxState::Rejected),
            "{tx:?}"
        );
    }

    // 7. A locked account stays locked, and only the dispute process can change it (A4).
    for (client, was) in &before.accounts {
        if was.is_locked() {
            let now = &after.accounts[client];
            assert!(now.is_locked(), "{tx:?} unlocked the account");
            if now != was {
                let dispute_process = matches!(
                    tx,
                    Transaction::Dispute(_) | Transaction::Resolve(_) | Transaction::Chargeback(_)
                );
                assert!(
                    dispute_process && matches!(outcome, Outcome::Applied { .. }),
                    "{tx:?} changed a locked account ({outcome:?})"
                );
            }
        }
    }

    // 8. Records are never removed, only their state changes, and only along allowed moves. New
    // records start as Normal or Rejected.
    assert!(after.txs.len() >= before.txs.len());
    for (was, now) in before.txs.iter().zip(&after.txs) {
        assert_eq!(
            (was.id(), was.client(), was.kind(), was.amount()),
            (now.id(), now.client(), now.kind(), now.amount())
        );
        assert!(
            allowed_move(was.state(), now.state()),
            "{:?} -> {:?} by {tx:?}",
            was.state(),
            now.state()
        );
    }
    for added in &after.txs[before.txs.len()..] {
        assert!(matches!(added.state(), TxState::Normal | TxState::Rejected));
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Invariants 1-8, after every step.
    #[test]
    fn invariants_hold_after_every_step(ops in vec(op(), 1..100)) {
        let mut engine = Engine::new(MemoryStore::default());
        let mut before = state(&engine);
        for op in ops {
            let tx = op.tx();
            let outcome = engine.process(&tx).expect("the in-memory store never fails");
            let after = state(&engine);
            check_step(&tx, outcome, &before, &after);
            before = after;
        }
    }

    /// Invariant 9: every row is counted exactly once.
    #[test]
    fn every_row_is_counted_exactly_once(rows in vec(row(), 0..100)) {
        let input = format!("type,client,tx,amount\n{}\n", rows.join("\n"));
        let mut metrics = MemoryMetrics::default();
        run(input.as_bytes(), Vec::new(), &mut metrics).unwrap();

        let read = metrics.total(Family::RowsRead);
        prop_assert_eq!(read, rows.len() as u64);
        prop_assert_eq!(
            read,
            metrics.total(Family::RowsParseFailed)
                + metrics.total(Family::TransactionsApplied)
                + metrics.total(Family::TransactionsRejected)
                + metrics.total(Family::StoreErrors)
        );
    }

    /// Invariant 10: running only one client's rows gives that client the same account as running
    /// all of them, as long as no tx id is used by two clients.
    #[test]
    fn clients_are_independent(ops in vec(op_with_ids_per_client(), 1..100)) {
        let all = final_accounts(&ops);
        for client in 1..=CLIENTS {
            let own: Vec<Op> = ops.iter().copied().filter(|op| op.client() == client).collect();
            let alone = final_accounts(&own);
            prop_assert_eq!(all.get(&ClientId(client)), alone.get(&ClientId(client)));
        }
    }
}
