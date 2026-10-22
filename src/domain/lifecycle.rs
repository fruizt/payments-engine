//! The single source of truth for a transaction's life: the states a stored deposit or withdrawal
//! can be in, the steps it can take, the reason every other step is refused, and the money each
//! step moves. No other code sets a transaction's state or changes a balance.
//!
//! ```text
//!             dispute               resolve
//!   Normal ────────────► Disputed ────────────► Resolved      (final for now, A5)
//!                           │
//!                           │ chargeback
//!                           ▼
//!                       ChargedBack                            (final, account locked)
//!
//!   Rejected    the deposit/withdrawal itself failed; final, but its tx id stays taken (A7)
//! ```
//!
//! The compiler enforces it: [`StoredTx`] and [`Step`] have private fields, so only this module
//! can create a record, change its state or allow a step, and the balances of an
//! [`Account`] can only be changed from inside `domain`.
//!
//! What stays outside: the checks on the row and the account before any step (duplicate id, owner
//! of the transaction, locked account, enough funds) live in each transaction type's `verify`.

use rust_decimal::Decimal;

use super::{Account, Amount, ClientId, TxId};
use crate::error::{Overflow, Rejection};

/// Whether a stored transaction is a deposit or a withdrawal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxKind {
    Deposit,
    Withdrawal,
}

/// The states of a stored deposit or withdrawal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxState {
    /// Applied, and not under dispute.
    Normal,
    /// Under dispute: its funds are held.
    Disputed,
    /// The dispute ended and the funds were released.
    Resolved,
    /// The dispute ended and the deposit was reversed.
    ChargedBack,
    /// The deposit or withdrawal itself was rejected. It moved no money, but keeps its id (A7).
    Rejected,
}

/// What can happen to a stored transaction after it's created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    Dispute,
    Resolve,
    Chargeback,
}

/// A step that [`next`] allowed. Only this module can create one, so [`advance`] can only take
/// allowed steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    to: TxState,
    money: Money,
}

impl Step {
    const fn new(to: TxState, money: Money) -> Self {
        Self { to, money }
    }
}

/// The money a step moves. `x` is the amount of the deposit or withdrawal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Money {
    /// available +x
    Credit,
    /// available −x
    Debit,
    /// available −x, held +x
    Hold,
    /// held −x, available +x
    Release,
    /// held −x: the held funds leave the account
    Reverse,
}

/// A stored deposit or withdrawal. Only this module can create one or change its state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredTx {
    id: TxId,
    client: ClientId,
    kind: TxKind,
    amount: Amount,
    state: TxState,
}

impl StoredTx {
    pub fn id(&self) -> TxId {
        self.id
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    pub fn kind(&self) -> TxKind {
        self.kind
    }

    pub fn amount(&self) -> Amount {
        self.amount
    }

    pub fn state(&self) -> TxState {
        self.state
    }
}

/// The transition table: every step a stored transaction can take after it's created, and the
/// reason for every step that isn't allowed.
///
/// There is no catch-all arm, so adding a state or an event doesn't compile until every new
/// combination has been decided.
pub fn next(tx: &StoredTx, event: Event) -> Result<Step, Rejection> {
    use Event::{Chargeback, Dispute, Resolve};
    use TxState::{ChargedBack, Disputed, Normal, Rejected, Resolved};

    match (tx.state, event) {
        // A2: only deposits can be disputed, whatever their state.
        (_, Dispute) if tx.kind == TxKind::Withdrawal => Err(Rejection::NotDisputable),

        (Normal, Dispute) => Ok(Step::new(Disputed, Money::Hold)),
        (Disputed, Resolve) => Ok(Step::new(Resolved, Money::Release)),
        (Disputed, Chargeback) => Ok(Step::new(ChargedBack, Money::Reverse)),

        (Disputed, Dispute) => Err(Rejection::AlreadyDisputed),
        // A5: disputing again would be one more arm, placed above this one:
        // (Resolved, Dispute) if tx.disputes < MAX_DISPUTES => Ok(Step::new(Disputed, Money::Hold)),
        (Resolved, Dispute) => Err(Rejection::AlreadyResolved),
        (Normal | Resolved, Resolve | Chargeback) => Err(Rejection::NotUnderDispute),
        (ChargedBack, _) => Err(Rejection::AlreadyChargedBack),
        (Rejected, _) => Err(Rejection::TxRejected),
    }
}

/// Creates the record of a deposit or withdrawal that passed its checks, and moves its money: a
/// deposit credits the account, a withdrawal debits it.
pub fn open(
    account: Account,
    kind: TxKind,
    id: TxId,
    amount: Amount,
) -> Result<(Account, StoredTx), Overflow> {
    let money = match kind {
        TxKind::Deposit => Money::Credit,
        TxKind::Withdrawal => Money::Debit,
    };
    let account = move_money(account, money, amount)?;
    let record = StoredTx {
        id,
        client: account.client(),
        kind,
        amount,
        state: TxState::Normal,
    };
    Ok((account, record))
}

/// Creates the record of a deposit or withdrawal that was rejected. No money moves, but the
/// record keeps its id taken (A7).
pub fn rejected(kind: TxKind, id: TxId, client: ClientId, amount: Amount) -> StoredTx {
    StoredTx {
        id,
        client,
        kind,
        amount,
        state: TxState::Rejected,
    }
}

/// Takes a step that [`next`] allowed: moves its money and moves the transaction to its new state.
pub fn advance(
    account: Account,
    tx: StoredTx,
    step: Step,
) -> Result<(Account, StoredTx), Overflow> {
    debug_assert_eq!(
        account.client(),
        tx.client,
        "A3: a step only moves its owner's money"
    );
    let account = move_money(account, step.money, tx.amount)?;
    Ok((
        account,
        StoredTx {
            state: step.to,
            ..tx
        },
    ))
}

/// The only place a balance changes. The arguments of `adjust` are (available, held).
fn move_money(account: Account, money: Money, amount: Amount) -> Result<Account, Overflow> {
    let (x, zero) = (amount.value(), Decimal::ZERO);
    let mut account = match money {
        Money::Credit => account.adjust(x, zero)?,
        Money::Debit => account.adjust(-x, zero)?,
        Money::Hold => account.adjust(-x, x)?,
        Money::Release => account.adjust(x, -x)?,
        Money::Reverse => account.adjust(zero, -x)?,
    };
    match money {
        // Spec: a chargeback freezes the account immediately.
        Money::Reverse => account.lock(),
        // A6: the client withdrew funds that are now disputed, and owes them to the platform.
        Money::Hold if account.available() < zero => account.flag_high_risk(),
        _ => {}
    }
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{amount, dec, take};

    fn record(kind: TxKind, state: TxState) -> StoredTx {
        StoredTx {
            id: TxId(1),
            client: ClientId(1),
            kind,
            amount: amount("10"),
            state,
        }
    }

    /// An account with a 10.0 deposit, and that deposit.
    fn deposited() -> (Account, StoredTx) {
        open(
            Account::new(ClientId(1)),
            TxKind::Deposit,
            TxId(1),
            amount("10"),
        )
        .unwrap()
    }

    #[test]
    fn every_state_and_event_of_a_deposit() {
        use Event::{Chargeback, Dispute, Resolve};
        use TxState::{ChargedBack, Disputed, Normal, Rejected, Resolved};

        // The README's lifecycle table, cell by cell.
        let table = [
            (Normal, Dispute, Ok(Disputed)),
            (Normal, Resolve, Err(Rejection::NotUnderDispute)),
            (Normal, Chargeback, Err(Rejection::NotUnderDispute)),
            (Disputed, Dispute, Err(Rejection::AlreadyDisputed)),
            (Disputed, Resolve, Ok(Resolved)),
            (Disputed, Chargeback, Ok(ChargedBack)),
            (Resolved, Dispute, Err(Rejection::AlreadyResolved)),
            (Resolved, Resolve, Err(Rejection::NotUnderDispute)),
            (Resolved, Chargeback, Err(Rejection::NotUnderDispute)),
            (ChargedBack, Dispute, Err(Rejection::AlreadyChargedBack)),
            (ChargedBack, Resolve, Err(Rejection::AlreadyChargedBack)),
            (ChargedBack, Chargeback, Err(Rejection::AlreadyChargedBack)),
            (Rejected, Dispute, Err(Rejection::TxRejected)),
            (Rejected, Resolve, Err(Rejection::TxRejected)),
            (Rejected, Chargeback, Err(Rejection::TxRejected)),
        ];
        for (state, event, expected) in table {
            let got = next(&record(TxKind::Deposit, state), event).map(|step| step.to);
            assert_eq!(got, expected, "{state:?} + {event:?}");
        }
    }

    #[test]
    fn a2_a_withdrawal_is_never_disputable() {
        use TxState::{ChargedBack, Disputed, Normal, Rejected, Resolved};

        for state in [Normal, Disputed, Resolved, ChargedBack, Rejected] {
            let got = next(&record(TxKind::Withdrawal, state), Event::Dispute);
            assert_eq!(got, Err(Rejection::NotDisputable), "{state:?}");
        }
    }

    #[test]
    fn resolve_and_chargeback_of_a_withdrawal_follow_its_state() {
        let normal = record(TxKind::Withdrawal, TxState::Normal);
        assert_eq!(
            next(&normal, Event::Resolve),
            Err(Rejection::NotUnderDispute)
        );
        let rejected = record(TxKind::Withdrawal, TxState::Rejected);
        assert_eq!(
            next(&rejected, Event::Chargeback),
            Err(Rejection::TxRejected)
        );
    }

    #[test]
    fn a_deposit_credits_and_starts_normal() {
        let (account, tx) = deposited();
        assert_eq!((account.available(), account.held()), (dec("10"), dec("0")));
        assert_eq!(tx.state(), TxState::Normal);
        assert_eq!(
            (tx.id(), tx.client(), tx.kind()),
            (TxId(1), ClientId(1), TxKind::Deposit)
        );
        assert_eq!(tx.amount(), amount("10"));
    }

    #[test]
    fn a_withdrawal_debits_and_starts_normal() {
        let (account, _) = deposited();
        let (account, tx) = open(account, TxKind::Withdrawal, TxId(2), amount("4")).unwrap();
        assert_eq!(account.available(), dec("6"));
        assert_eq!(tx.state(), TxState::Normal);
    }

    #[test]
    fn a7_a_rejected_record_moves_no_money_and_keeps_its_id() {
        let tx = rejected(TxKind::Withdrawal, TxId(9), ClientId(3), amount("5"));
        assert_eq!(tx.state(), TxState::Rejected);
        assert_eq!((tx.id(), tx.client()), (TxId(9), ClientId(3)));
    }

    #[test]
    fn a_dispute_holds_the_funds() {
        let (account, tx) = deposited();
        let (account, tx) = take(account, tx, Event::Dispute);
        assert_eq!((account.available(), account.held()), (dec("0"), dec("10")));
        assert_eq!(account.total(), dec("10"));
        assert_eq!(tx.state(), TxState::Disputed);
        assert!(!account.is_high_risk());
    }

    #[test]
    fn a_resolve_releases_the_funds() {
        let (account, tx) = deposited();
        let (account, tx) = take(account, tx, Event::Dispute);
        let (account, tx) = take(account, tx, Event::Resolve);
        assert_eq!((account.available(), account.held()), (dec("10"), dec("0")));
        assert_eq!(tx.state(), TxState::Resolved);
        assert!(!account.is_locked());
    }

    #[test]
    fn a_chargeback_reverses_the_funds_and_locks_the_account() {
        let (account, tx) = deposited();
        let (account, tx) = take(account, tx, Event::Dispute);
        let (account, tx) = take(account, tx, Event::Chargeback);
        assert_eq!((account.available(), account.held()), (dec("0"), dec("0")));
        assert_eq!(tx.state(), TxState::ChargedBack);
        assert!(account.is_locked());
    }

    #[test]
    fn a6_a_dispute_below_zero_flags_the_account() {
        let (account, deposit) = deposited();
        let (account, _) = open(account, TxKind::Withdrawal, TxId(2), amount("10")).unwrap();
        let (account, _) = take(account, deposit, Event::Dispute);
        assert_eq!(
            (account.available(), account.held()),
            (dec("-10"), dec("10"))
        );
        assert!(account.is_high_risk());
        assert!(!account.is_locked());
    }

    #[test]
    fn a_step_that_would_overflow_moves_nothing() {
        let (full, _) = open(
            Account::new(ClientId(1)),
            TxKind::Deposit,
            TxId(1),
            amount("1000000000000000000000000"),
        )
        .unwrap();
        assert_eq!(
            open(full, TxKind::Deposit, TxId(2), amount("0.0001")),
            Err(Overflow)
        );
    }
}
