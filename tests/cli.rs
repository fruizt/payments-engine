//! End-to-end tests: runs the real binary on the files in `tests/fixtures/`.
//!
//! Each `<name>.csv` has a `<name>.expected.csv` next to it. Outputs are compared ignoring row
//! order and line endings, as the spec allows.

use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_payments-engine"))
}

/// The header and the sorted data rows of a CSV.
fn rows(csv: &str) -> (String, Vec<String>) {
    let mut lines = csv.lines().map(str::to_owned);
    let header = lines.next().unwrap_or_default();
    let mut rows: Vec<_> = lines.filter(|line| !line.is_empty()).collect();
    rows.sort();
    (header, rows)
}

/// Runs the binary on `<name>.csv`, checks stdout against `<name>.expected.csv`, and returns
/// stderr (the metrics).
fn assert_fixture(name: &str) -> String {
    let assert = engine()
        .arg(fixture(&format!("{name}.csv")))
        .assert()
        .success();
    let output = assert.get_output();
    let expected = fs::read_to_string(fixture(&format!("{name}.expected.csv"))).unwrap();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(rows(&stdout), rows(&expected), "{name}");
    String::from_utf8(output.stderr.clone()).unwrap()
}

fn assert_contains(text: &str, needle: &str) {
    assert!(text.contains(needle), "{needle:?} not in:\n{text}");
}

#[test]
fn spec_example() {
    let metrics = assert_fixture("spec_example");
    assert_contains(&metrics, "rows_read_total 5");
    assert_contains(&metrics, r#"transactions_applied_total{type="deposit"} 3"#);
    assert_contains(
        &metrics,
        r#"transactions_rejected_total{type="withdrawal",reason="insufficient_funds"} 1"#,
    );
}

#[test]
fn disputes_resolves_and_chargebacks() {
    let metrics = assert_fixture("disputes");
    assert_contains(
        &metrics,
        r#"transactions_applied_total{type="chargeback"} 1"#,
    );
    assert_contains(&metrics, r#"transactions_applied_total{type="resolve"} 1"#);
    assert_contains(
        &metrics,
        r#"transactions_rejected_total{type="deposit",reason="account_locked"} 1"#,
    );
}

#[test]
fn bad_rows_fail_open() {
    let metrics = assert_fixture("bad_rows");
    assert_contains(&metrics, "rows_read_total 10");
    for reason in [
        "malformed_row",
        "unknown_type",
        "invalid_client_id",
        "invalid_tx_id",
        "missing_amount",
        "invalid_amount",
        "non_positive_amount",
        "amount_truncated_to_zero",
    ] {
        assert_contains(
            &metrics,
            &format!("rows_parse_failed_total{{reason=\"{reason}\"}} 1"),
        );
    }
}

#[test]
fn messy_formatting() {
    let metrics = assert_fixture("messy_formatting");
    assert_contains(&metrics, r#"amounts_truncated_total{type="withdrawal"} 1"#);
}

#[test]
fn header_only() {
    let metrics = assert_fixture("header_only");
    assert_eq!(metrics, "");
}

#[test]
fn wrong_arguments_exit_with_2() {
    let cases: [&[&str]; 2] = [&[], &["a.csv", "b.csv"]];
    for args in cases {
        let assert = engine().args(args).assert().code(2);
        let output = assert.get_output();
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_contains(&String::from_utf8_lossy(&output.stderr), "usage");
    }
}

#[test]
fn a_missing_file_exits_with_1() {
    let assert = engine().arg(fixture("does_not_exist.csv")).assert().code(1);
    let output = assert.get_output();
    assert!(output.stdout.is_empty());
    assert_contains(&String::from_utf8_lossy(&output.stderr), "can't open");
}
