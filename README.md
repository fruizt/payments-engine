# payments-engine

A small payments engine in Rust. It reads a CSV of transactions (deposits, withdrawals, disputes,
resolves and chargebacks), applies them to client accounts one at a time, and prints the final
state of every account as CSV.

## Usage

```sh
cargo run -- transactions.csv > accounts.csv
```

- **Input:** the path to a CSV file is the first and only argument.
- **Output:** the accounts CSV goes to **stdout**. Metrics go to **stderr**, so they never mix
  with the CSV.
- **Exit code:** `0` when the file was processed, even if some rows were rejected. Non-zero only
  when the input can't be read or the output can't be written (see [A14](#a14-only-inputoutput-failures-can-stop-the-run)).

### Input

Columns `type, client, tx, amount`. Rows are in chronological order.

```csv
type, client, tx, amount
deposit, 1, 1, 1.0
deposit, 2, 2, 2.0
deposit, 1, 3, 2.0
withdrawal, 1, 4, 1.5
withdrawal, 2, 5, 3.0
```

| Column | Type | Notes |
|---|---|---|
| `type` | string | `deposit`, `withdrawal`, `dispute`, `resolve`, `chargeback` |
| `client` | `u16` | client id |
| `tx` | `u32` | transaction id, globally unique |
| `amount` | decimal | only for `deposit` and `withdrawal`; up to 4 decimal places |

### Output

Columns `client, available, held, total, locked`, one row per client, sorted by client id, with
amounts shown to 4 decimal places. For the input above:

```csv
client,available,held,total,locked
1,1.5000,0.0000,1.5000,false
2,2.0000,0.0000,2.0000,false
```

Client 2's withdrawal of 3.0 is rejected for insufficient funds.

## Design

### Principles

- **One transaction at a time, as it arrives.** In a real payments system transactions arrive at
  any moment and each one needs an answer right away, the way a card authorizer approves or
  declines within seconds. So the engine processes each transaction in order and returns an
  outcome for it (applied, or rejected with a reason). The file is streamed row by row and never
  loaded whole.
- **Fail open per row.** A bad row is rejected and counted. It never stops the batch.
- **Measure every outcome.** Successes are counted as well as every kind of failure, like a
  service exporting Prometheus metrics.
- **The engine doesn't know where data lives.** The datastore (a stand-in for a database) and the
  metrics (a stand-in for Prometheus) are separate modules behind traits. Replacing the in-memory
  versions with a file, a database or a real exporter doesn't change the engine.
- **All or nothing.** Everything one transaction writes is committed in one atomic step (see
  [Atomic writes](#atomic-writes)).
- **Exact money.** Amounts are decimals, never floating point.

### Project layout

```
payments-engine/
├── Cargo.toml
├── README.md
├── src/
│   ├── main.rs               # CLI only: read the argument, open the file, wire things up, exit codes
│   ├── lib.rs                # module declarations + run(input, output, metrics)
│   ├── domain/
│   │   ├── mod.rs            # ClientId(u16), TxId(u32), Amount
│   │   ├── account.rs        # Account: balances + one overflow-checked adjust()
│   │   └── lifecycle.rs      # single source of truth: states, steps, money per step
│   ├── io/
│   │   ├── reader.rs         # CSV → stream of Result<Transaction, ParseError>
│   │   └── writer.rs         # accounts → CSV on stdout
│   ├── transaction/
│   │   ├── mod.rs            # Transaction enum + the verify/apply trait
│   │   ├── deposit.rs
│   │   ├── withdrawal.rs
│   │   ├── dispute.rs
│   │   ├── resolve.rs
│   │   └── chargeback.rs
│   ├── engine.rs             # the core: process(tx) → Applied | Rejected(reason)
│   ├── datastore/
│   │   ├── mod.rs            # Store trait: two reads + one atomic commit(WriteBatch)
│   │   └── memory.rs         # in-memory version: Vec + HashMap for txs, HashMap for clients
│   ├── metrics/
│   │   ├── mod.rs            # Metrics trait + Event enum
│   │   └── memory.rs         # counters, printed to stderr in Prometheus text format
│   ├── error.rs              # ParseError, Rejection, StoreError
│   └── test_support.rs       # helpers shared by the unit tests
└── tests/
    ├── fixtures/             # input CSVs + expected outputs
    ├── assumptions.rs        # one test per documented assumption
    ├── invariants.rs         # property tests
    └── cli.rs                # end-to-end tests on the binary
```

The crate is a library plus a thin `main`. Tests, and later a network server, call the engine
directly; the CSV file is just one way to feed it.

### How one row flows through

```
CSV row ──► reader ──► Err(ParseError) ──────────────────────────────► metric: parse_failed{reason}
                  └──► Ok(Transaction) ──► engine.process(tx)
                                             1. load    the client's account (new if unknown, A9)
                                                        + the stored tx with this row's tx id
                                             2. decide  verify → apply              (no store access)
                                                          ├─ Ok  → batch: updated account + tx write
                                                          └─ Err → batch: new client (A9)
                                                                        + rejected record (A7)
                                             3. commit  store.commit(batch)         (all or nothing)
                                                  │
                                                  ├─ Outcome::Applied ─────────────► metric: applied{type}
                                                  │                                  (+ high_risk if flagged, A6)
                                                  ├─ Outcome::Rejected(reason) ────► metric: rejected{type, reason}
                                                  └─ StoreError ───────────────────► metric: store_errors
end of input ──► accounts printed to stdout, metrics printed to stderr
```

### Transactions

Each row becomes a `Transaction` enum with one variant per type, and each variant wraps its own
struct. A type that isn't in the enum is rejected while parsing.

Every transaction struct implements a trait with two steps. Neither one touches the datastore:
the engine loads the records they need and writes what they return.

- **`verify`** checks the rules against a copy of the records (account locked, insufficient
  funds, transaction not found, …). It can't change anything. If it passes, it returns a
  `Verified` value holding the records it checked.
- **`apply`** takes that `Verified` value and calculates the new records as a `WriteBatch`,
  without writing them. Only `verify` can create a `Verified`, so `apply` can never run on
  unchecked records. The only way `apply` can still fail is an arithmetic overflow
  (`balance_overflow`).

Each struct also has its own `new` constructor. It isn't part of the trait because each type takes
different inputs: a dispute has no amount, and a deposit must always have one.

What each transaction does to the account, and to the transaction it references, is defined in
one place: [Transaction lifecycle](#transaction-lifecycle).

### Transaction lifecycle

Everything that can happen to a stored deposit or withdrawal lives in one module,
`src/domain/lifecycle.rs`: the states it can be in, the steps it can take, the reason every other
step is refused, and the money each step moves. It's the single source of truth: no other code
sets a transaction's state or changes a balance.

```
            dispute               resolve
  Normal ────────────► Disputed ────────────► Resolved      (final for now, see A5)
                          │
                          │ chargeback
                          ▼
                      ChargedBack                            (final, account locked)

  Rejected    the deposit/withdrawal itself failed; final, but its tx id stays taken (A7)
```

**Creation.** An applied deposit starts as `Normal` and credits the account; an applied withdrawal
starts as `Normal` and debits it. A deposit or withdrawal that was rejected is stored as `Rejected`
and moves no money.

**Every later step.** Each cell says where the transaction goes and which money moves, or why the
step is refused:

| State ↓ / event → | dispute | resolve | chargeback |
|---|---|---|---|
| **Normal** | → Disputed · *hold* | `not_under_dispute` | `not_under_dispute` |
| **Disputed** | `already_disputed` | → Resolved · *release* | → ChargedBack · *reverse* |
| **Resolved** | `already_resolved` (A5) | `not_under_dispute` | `not_under_dispute` |
| **ChargedBack** | `already_charged_back` | `already_charged_back` | `already_charged_back` |
| **Rejected** | `tx_rejected` | `tx_rejected` | `tx_rejected` |

A dispute on a withdrawal is refused before its state is looked at: `not_disputable`
([A2](#a2-only-deposits-can-be-disputed)).

**Money per step** (`x` is the amount of the deposit or withdrawal):

| Step | available | held | Also |
|---|---|---|---|
| credit (deposit) | +x | | |
| debit (withdrawal) | −x | | |
| hold (dispute) | −x | +x | flags the account high-risk if available drops below zero (A6) |
| release (resolve) | +x | −x | |
| reverse (chargeback) | | −x | locks the account |

`total` follows on its own, since it's always `available + held`.

The compiler enforces the single source of truth:

- `StoredTx` has private fields, so only the lifecycle module can create a record or change its
  state.
- A `Step` (an allowed move) can only be created by the lifecycle's transition table, so a step
  that wasn't allowed can't be taken.
- `Account` balances change only through one overflow-checked `adjust`, which only code inside
  `domain` can call.

What stays outside: the checks on the row and the account before any step (duplicate id, owner of
the transaction, locked account, enough funds) live in each transaction type's `verify`.

### Datastore

- **Transactions** are kept in a `Vec` in arrival order, plus a `HashMap` from transaction id to
  position. That gives both an order (any two transactions can be compared by position) and O(1)
  lookup by id.
- **Only deposits and withdrawals are stored.** Disputes, resolves and chargebacks reuse the id of
  the transaction they point to, so they aren't stored as their own entries. They move the state of
  the transaction they reference instead (see [Transaction lifecycle](#transaction-lifecycle)).
- **Accounts** are kept in a `HashMap` keyed by client id. An account stores `available`, `held`,
  `locked` and a `high_risk` flag. `total` is always calculated as `available + held`, so the three
  numbers can never disagree. The balance fields are private and change only through one
  overflow-checked `adjust`, called by the lifecycle module.
- **The `Store` trait is shaped like a database**, so it can be swapped for a real one:
  - every method can fail (`Result<_, StoreError>`), because a file or database version will
    sometimes fail;
  - records are read and written by value. Nothing holds a reference into the storage, because a
    real database can't hand one out;
  - there are two reads, `get_account` and `get_tx`, and **one write**, `commit`
    (see [Atomic writes](#atomic-writes)).

### Atomic writes

One transaction can write two records: its client's account, and a transaction record (a new
deposit/withdrawal, or the new state of a disputed one). Even a rejected transaction can write
both: the new client (A9) and the rejected record that keeps its id taken (A7).

If those were two separate writes, the first could succeed and the second fail. For example, a
deposit could be marked `Disputed` while its funds were never held; a later resolve would then
release funds that were never held, creating money out of nothing.

So the store has a single write method, `commit(WriteBatch)`, which applies the whole batch or
nothing, like a database transaction. Each transaction produces exactly one batch (at most one
account and one transaction record), and the engine commits it once.

The in-memory store gets the same guarantee in two phases:

1. **Check:** every write in the batch is validated first. A new transaction id must not exist yet
   (like a unique key), and an update must point at an existing record. An error here returns
   before anything has changed.
2. **Apply:** the writes are made. Nothing in this phase can fail, so the batch lands completely.
   (The one exception is running out of memory, which aborts the whole process, so no half-written
   state survives it.)

This covers atomicity. Isolation (nobody else changes the records between the load and the
commit) comes from having a single engine writing to the store. With several writers, the commit
would also have to check that the records haven't changed since they were loaded, or clients would
be split across workers (see [Future work](#future-work)).

### Metrics

Counters are kept in memory and printed to stderr in Prometheus text format at the end of the run.
The engine doesn't record metrics itself: it returns an `Outcome` for each transaction (applied, or
rejected with a reason), and the caller counts it. A server would send that same outcome back as
the reply.

| Metric | Labels | Counts |
|---|---|---|
| `rows_read_total` | | every row read from the file |
| `rows_parse_failed_total` | `reason` | rows that never became a transaction |
| `transactions_applied_total` | `type` | transactions that went through |
| `transactions_rejected_total` | `type`, `reason` | valid transactions refused by the rules |
| `amounts_truncated_total` | `type` | amounts cut to 4 decimal places ([A10](#a10-amounts)) |
| `amounts_ignored_total` | `type` | amounts sent on dispute/resolve/chargeback rows ([A11](#a11-amounts-on-dispute-resolve-and-chargeback-rows-are-ignored)) |
| `accounts_flagged_high_risk_total` | | accounts flagged by [A6](#a6-a-dispute-may-push-available-below-zero-and-flags-the-account) |
| `store_errors_total` | | failed reads or commits: a system error, not the partner's fault. The transaction was not applied and nothing was written |

Client ids are never used as labels: one series per client would make the metrics unusable at
scale. Which accounts are high-risk is kept on the account record in the datastore.

For the example input above, stderr shows:

```
# TYPE rows_read_total counter
rows_read_total 5
# TYPE transactions_applied_total counter
transactions_applied_total{type="deposit"} 3
transactions_applied_total{type="withdrawal"} 1
# TYPE transactions_rejected_total counter
transactions_rejected_total{type="withdrawal",reason="insufficient_funds"} 1
```

### Error cases

Every failure has its own enum variant and its own metric label. Each variant maps to its label
through a `match` that must cover every variant, so a new error case without a label won't compile.

**Parse errors.** The row never becomes a transaction.

| Reason | Meaning |
|---|---|
| `malformed_row` | the CSV row can't be read (invalid UTF-8, or more fields than the header) |
| `unknown_type` | `type` isn't one of the five transaction types |
| `invalid_client_id` | `client` is missing or isn't a `u16` |
| `invalid_tx_id` | `tx` is missing or isn't a `u32` |
| `missing_amount` | a deposit or withdrawal has no amount |
| `invalid_amount` | the amount isn't a decimal number, or is larger than 10^24 ([A10](#a10-amounts)) |
| `non_positive_amount` | the amount is zero or negative |
| `amount_truncated_to_zero` | the amount is positive but becomes 0 when cut to 4 decimal places |

**Rejections.** A valid transaction that the rules refuse.

| Reason | Applies to | Meaning |
|---|---|---|
| `account_locked` | all | the client's account is locked ([A4](#a4-a-locked-account-rejects-everything)) |
| `insufficient_funds` | withdrawal | available < amount |
| `duplicate_tx_id` | deposit, withdrawal | the tx id was already used ([A7](#a7-transaction-ids-are-unique-even-for-rejected-transactions)) |
| `tx_not_found` | dispute, resolve, chargeback | no stored transaction has that id |
| `client_mismatch` | dispute, resolve, chargeback | the referenced tx belongs to another client ([A3](#a3-only-the-owner-of-a-transaction-can-dispute-resolve-or-charge-it-back)) |
| `tx_rejected` | dispute, resolve, chargeback | the referenced deposit/withdrawal had itself been rejected |
| `not_disputable` | dispute | the referenced tx is a withdrawal ([A2](#a2-only-deposits-can-be-disputed)) |
| `already_disputed` | dispute | the referenced tx is already under dispute |
| `already_resolved` | dispute | the referenced tx was disputed and resolved before ([A5](#a5-a-resolved-transaction-cant-be-disputed-again-for-now)) |
| `already_charged_back` | dispute, resolve, chargeback | the referenced tx was charged back |
| `not_under_dispute` | resolve, chargeback | the referenced tx isn't under dispute |
| `balance_overflow` | any that changes a balance | available, held or total would go beyond ±10^24 ([A10](#a10-amounts)) |

Checks run in a fixed order, so each rejection gets exactly one reason. First the row must match
our records (a new id, or a referenced transaction that exists and belongs to this client), then
the account must not be locked, then the rules of the transaction itself apply:

- **Deposit / withdrawal:** `duplicate_tx_id` → `account_locked` → `insufficient_funds` →
  `balance_overflow`.
- **Dispute / resolve / chargeback:** `tx_not_found` → `client_mismatch` → `account_locked` →
  `not_disputable` (dispute only) → state of the referenced transaction (`tx_rejected`,
  `already_disputed`, `already_resolved`, `already_charged_back`, `not_under_dispute`) →
  `balance_overflow`.

For dispute, resolve and chargeback, `not_disputable` and the state checks come from the
[lifecycle table](#transaction-lifecycle).

`already_charged_back` can't actually happen today: a chargeback locks the account, so any later
row from that client stops at `account_locked` before the state check, and rows from other clients
stop at `client_mismatch`. It's kept so the state check covers every state, and it becomes reachable if
accounts can ever be unlocked.

## Assumptions

The spec leaves these cases open. Each one is cited in the code where it's enforced, and has a test
in `tests/assumptions.rs` named after its id (for example `a03_dispute_on_other_clients_tx_is_rejected`).

### A1. Rows are processed in file order, one at a time

The file order is the chronological order. A dispute that points at a transaction appearing later
in the file is rejected as `tx_not_found`, because at that moment the transaction doesn't exist.

### A2. Only deposits can be disputed

A dispute on a withdrawal is rejected as `not_disputable`.

**Why.** The spec's dispute rules (available goes down, held goes up) are written for money coming
in. Applied literally to a withdrawal, they punish the client twice. Example: deposit 100, withdraw
60, then dispute the withdrawal:

| Model | After dispute (avail / held / total) | After chargeback |
|---|---|---|
| Spec rules applied literally | −20 / 60 / 40 | −20 / 0 / −20, locked: the client loses 120 |
| Hold the claim (held +x, total +x) | 40 / 60 / 100 | 100 / 0 / 100, locked |
| Flag only (no change until decided) | 40 / 0 / 40 | 100 / 0 / 100, locked |

Disputing a withdrawal does make sense in the real world, for example "my phone was stolen and
someone withdrew my money". The preferred model for a later version is **flag only**: nothing moves
while the dispute is open, because the money has already left the platform; a chargeback gives it
back and locks the account, which also fits a compromised account. "Hold the claim" would show money
in `total` that the platform doesn't have.

### A3. Only the owner of a transaction can dispute, resolve or charge it back

If the client on a dispute, resolve or chargeback row isn't the client of the referenced
transaction, the row is rejected as `client_mismatch`.

**Why.** Otherwise one client could dispute another client's transaction. It also means a
transaction only ever touches its own client's account (see [Future work](#future-work)).

### A4. A locked account rejects everything

After a chargeback, every later transaction for that client is rejected as `account_locked`:
deposits, withdrawals, disputes, resolves and chargebacks.

**Why.** The spec says the account is "immediately frozen", and nothing in it unlocks an account.

### A5. A resolved transaction can't be disputed again (for now)

A dispute on a transaction that was already disputed and resolved is rejected as `already_resolved`.

**Why.** Disputing again does make sense in the real world, for example when a client comes back
with new evidence. It shouldn't be unlimited, though: a dispute/resolve loop could freeze the same
funds over and over. A limit such as two disputes per transaction is likely. It isn't done now
because the spec doesn't need it and it needs a policy decision (how many times, within what time
window). The code marks where a dispute counter with a maximum would go.

### A6. A dispute may push available below zero, and flags the account

If a client deposits, withdraws the money and then disputes the deposit, their available balance
goes negative. This is allowed: the disputed funds are held as the spec says, and the negative
balance is what the client owes the platform. A withdrawal can never make available negative, only
a dispute can.

When it happens, the account is flagged as **high-risk** and `accounts_flagged_high_risk_total`
goes up. The flag is kept on the account in the datastore but isn't in the output CSV, whose columns
are fixed by the spec.

**Why.** This is the pattern of "friendly fraud": the client gets the money once through the
withdrawal and again through the chargeback, and the platform takes the loss. Blocking it isn't the
engine's job, but it must be visible. With available below zero, every withdrawal already fails for
insufficient funds, and deposits still go through (they pay down what the client owes).

### A7. Transaction ids are unique, even for rejected transactions

A deposit or withdrawal whose id was already used is rejected as `duplicate_tx_id`, even if the
first transaction with that id was itself rejected. The first one keeps the id.

**Why.** An id identifies one transaction. If an audit query for a transaction id ever returned two
results, the record would be worthless. It also gives a dispute on a failed transaction a clear
reason (`tx_rejected`) instead of `tx_not_found`.

### A8. A row that fails parsing has no effect

It doesn't create its client and doesn't take its tx id. It's only counted in
`rows_parse_failed_total`.

**Why.** If any field of a row is broken, none of its fields can be trusted, including the client
and tx ids.

### A9. Clients are created on their first valid row, even if it's rejected

A client appears in the output as soon as one of its rows parses, even if every one of its
transactions is rejected. Such a client shows all zeros.

**Why.** The partner sent us that client, so the client exists. A more complete view of the
clients is not worse than a smaller one.

### A10. Amounts

- A deposit or withdrawal amount must be greater than zero (`non_positive_amount`).
- An amount with more than 4 decimal places is **cut** to 4 (rounded toward zero), not rejected and
  not rounded. Each cut that drops a non-zero digit is counted in `amounts_truncated_total`
  (`1.50000` loses nothing, so it isn't counted).
- An amount that becomes 0 after cutting is rejected (`amount_truncated_to_zero`).
- An amount is digits with at most one `.` and an optional leading sign (`1`, `1.5`, `+2`, `.25`),
  and at most 10^24. Anything else is `invalid_amount`. The limit keeps every balance small enough
  that decimal arithmetic is always exact, never rounded.

**Why.** The spec says to *assume* 4 decimal places. The engine may have many partners sending
different precision; the API promises 4, but sending more shouldn't break anything. Cutting
guarantees the engine never moves more money than a partner asked for, which rounding can't (it
sometimes rounds up). The metric shows which partners send extra digits.

### A11. Amounts on dispute, resolve and chargeback rows are ignored

These rows refer to the amount of the transaction they reference. If an amount is sent anyway, it's
ignored and counted in `amounts_ignored_total`.

### A12. Type names ignore case and surrounding spaces

`Deposit`, ` deposit` and `DEPOSIT` are all deposits. All fields are trimmed, since the spec's own
example has spaces after the commas.

### A13. The header row is required

Columns are matched by header name.

### A14. Only input/output failures can stop the run

The run stops with an error on stderr and a non-zero exit code only when the input can't be read
(missing argument, file can't be opened, read error) or the output can't be written. A row with bad
content never stops it: rows fail open.

## Invariants

Property tests (`tests/invariants.rs`, using `proptest`) generate random transaction sequences. They
are weighted so that disputes, resolves and chargebacks usually point at real transactions. After
**every** step they check:

1. `total == available + held` for every account.
2. `held >= 0`, always.
3. `held` equals the sum of that client's deposits currently under dispute.
4. Money is conserved: `total == applied deposits − applied withdrawals − charged-back deposits`.
5. A withdrawal never makes `available` negative. Only a dispute can (A6).
6. A rejected transaction changes no balance and no transaction state. Its only writes are
   creating a new client (A9) and recording a rejected deposit/withdrawal (A7).
7. A locked account never changes again.
8. A transaction's state only moves `Normal → Disputed → Resolved | ChargedBack`. Final states and
   rejected transactions never change.
9. Every row is counted exactly once:
   `rows_read == applied + rejected + parse_failed + store_errors`.
10. Clients are independent: running only client A's rows gives client A the same final account as
    running the whole file, as long as no tx id is used by two different clients (a reused id is
    rejected as a duplicate in the full run but not in the filtered one).

Invariant 1 holds by construction, because `total` is calculated, never stored; the tests still
check it on the output. Invariant 2 is also checked at runtime with `debug_assert!`, so every debug
test run catches a violation where it happens.

## Testing

- **Unit tests** next to the code: parsing of each field, and `verify`/`apply` for each transaction
  type. Both are plain functions, so these tests need no store.
- **Lifecycle table test:** one test goes through every state × event cell of the
  [lifecycle table](#transaction-lifecycle), plus disputes on withdrawals, and checks each result.
- **Atomicity tests:** a batch whose transaction write fails (for example a tx id that already
  exists) leaves the store exactly as it was, account included. An engine on a store that fails
  one commit returns a store error, and the next transaction sees the state from before the
  failure.
- **`tests/assumptions.rs`:** one test per assumption above.
- **`tests/invariants.rs`:** the property tests above.
- **`tests/cli.rs`:** runs the real binary on the files in `tests/fixtures/` and compares stdout
  (ignoring row order) and exit codes.

```sh
cargo test
```

## Dependencies

| Crate | Used for |
|---|---|
| `csv` | streaming CSV reading and writing |
| `serde` | mapping CSV columns to row structs by header name, and writing output rows |
| `rust_decimal` | exact decimal math, cutting to 4 places, overflow-checked arithmetic |
| `thiserror` | error enums with readable messages |
| `proptest` (dev) | property tests for the invariants |
| `assert_cmd` (dev) | end-to-end tests on the binary |

## Future work

- **Withdrawal disputes** with the "flag only" model ([A2](#a2-only-deposits-can-be-disputed)).
- **Disputing again** with a limit per transaction ([A5](#a5-a-resolved-transaction-cant-be-disputed-again-for-now)).
- **Memory at scale.** Memory grows with the stored transactions, not with the file. At the `u32`
  limit (about 4.3 billion transactions) and roughly 40 bytes per stored transaction, that's well over
  100 GB. Smaller records (for example amounts as a fixed-point `i64` instead of a 16-byte decimal)
  and a disk-backed `Store` would fix this without touching the engine.
- **Concurrency.** For thousands of concurrent streams, clients can be split across workers by
  client id. Thanks to [A3](#a3-only-the-owner-of-a-transaction-can-dispute-resolve-or-charge-it-back), a transaction only touches its own client, so each
  worker owns its clients and needs no locks. The one shared piece is checking that tx ids are
  unique across all clients.
- **Retries.** A transaction whose commit fails is counted in `store_errors_total` and skipped. A
  real engine would set it aside and retry it later.
- **A real Prometheus exporter** behind the `Metrics` trait.
