//! Counters kept in memory, printed in Prometheus text format.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use super::{Event, Family, Metrics};

/// Counters kept in memory. Its `Display` is the Prometheus text format.
#[derive(Debug, Default)]
pub struct MemoryMetrics {
    counts: HashMap<Event, u64>,
}

impl MemoryMetrics {
    /// How many times this event was recorded.
    pub fn count(&self, event: Event) -> u64 {
        self.counts.get(&event).copied().unwrap_or(0)
    }

    /// The total of a metric across all of its labels.
    pub fn total(&self, family: Family) -> u64 {
        self.counts
            .iter()
            .filter(|(event, _)| event.series().0 == family)
            .map(|(_, count)| count)
            .sum()
    }
}

impl Metrics for MemoryMetrics {
    fn record(&mut self, event: Event) {
        *self.counts.entry(event).or_default() += 1;
    }
}

/// Every counter in Prometheus text format, in a stable order: metrics in the order of
/// [`Family`], and each metric's series sorted by their labels. Metrics with no events are left
/// out.
impl fmt::Display for MemoryMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut families: BTreeMap<Family, BTreeMap<String, u64>> = BTreeMap::new();
        for (&event, &count) in &self.counts {
            let (family, labels) = event.series();
            *families
                .entry(family)
                .or_default()
                .entry(labels)
                .or_default() += count;
        }
        for (family, series) in families {
            let name = family.name();
            writeln!(f, "# HELP {name} {}", family.help())?;
            writeln!(f, "# TYPE {name} counter")?;
            for (labels, count) in series {
                writeln!(f, "{name}{labels} {count}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{ParseError, Rejection};
    use crate::transaction::TxType;

    #[test]
    fn counts_events() {
        let mut metrics = MemoryMetrics::default();
        metrics.record(Event::Applied(TxType::Deposit));
        metrics.record(Event::Applied(TxType::Deposit));
        metrics.record(Event::Applied(TxType::Withdrawal));
        assert_eq!(metrics.count(Event::Applied(TxType::Deposit)), 2);
        assert_eq!(metrics.count(Event::Applied(TxType::Dispute)), 0);
        assert_eq!(metrics.total(Family::TransactionsApplied), 3);
        assert_eq!(metrics.total(Family::RowsRead), 0);
    }

    #[test]
    fn prints_the_readme_example() {
        let mut metrics = MemoryMetrics::default();
        for _ in 0..5 {
            metrics.record(Event::RowRead);
        }
        for _ in 0..3 {
            metrics.record(Event::Applied(TxType::Deposit));
        }
        metrics.record(Event::Applied(TxType::Withdrawal));
        metrics.record(Event::Rejected(
            TxType::Withdrawal,
            Rejection::InsufficientFunds,
        ));
        assert_eq!(
            metrics.to_string(),
            "# HELP rows_read_total Rows read from the input.\n\
             # TYPE rows_read_total counter\n\
             rows_read_total 5\n\
             # HELP transactions_applied_total Transactions that went through, by type.\n\
             # TYPE transactions_applied_total counter\n\
             transactions_applied_total{type=\"deposit\"} 3\n\
             transactions_applied_total{type=\"withdrawal\"} 1\n\
             # HELP transactions_rejected_total Transactions refused by the rules, by type and reason.\n\
             # TYPE transactions_rejected_total counter\n\
             transactions_rejected_total{type=\"withdrawal\",reason=\"insufficient_funds\"} 1\n"
        );
    }

    #[test]
    fn sorts_series_by_their_labels() {
        let mut metrics = MemoryMetrics::default();
        metrics.record(Event::ParseFailed(ParseError::UnknownType));
        metrics.record(Event::ParseFailed(ParseError::InvalidAmount));
        let text = metrics.to_string();
        let invalid = text.find("invalid_amount").unwrap();
        let unknown = text.find("unknown_type").unwrap();
        assert!(invalid < unknown, "{text}");
    }

    #[test]
    fn prints_nothing_without_events() {
        assert_eq!(MemoryMetrics::default().to_string(), "");
    }

    #[test]
    fn every_event_has_a_metric_with_a_name_and_help() {
        let events = ParseError::ALL
            .map(Event::ParseFailed)
            .into_iter()
            .chain(
                TxType::ALL
                    .into_iter()
                    .flat_map(|t| Rejection::ALL.map(|r| Event::Rejected(t, r))),
            )
            .chain(TxType::ALL.into_iter().flat_map(|t| {
                [
                    Event::Applied(t),
                    Event::AmountTruncated(t),
                    Event::AmountIgnored(t),
                ]
            }))
            .chain([Event::RowRead, Event::HighRiskFlagged, Event::StoreError]);
        for event in events {
            let (family, labels) = event.series();
            assert!(family.name().ends_with("_total"), "{event:?}");
            assert!(!family.help().is_empty(), "{event:?}");
            assert!(labels.is_empty() || labels.starts_with('{'), "{event:?}");
        }
    }
}
