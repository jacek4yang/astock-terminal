//! PIT-safe bridge from immutable news revisions to event backtests.

use astock_trading_rules::{
    classify_news_session, EffectiveNewsSession, EffectiveSessionRole, NewsSessionInput, RuleSet,
};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Minimal immutable news input accepted by event-driven backtests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsBacktestEvent {
    pub revision_id: String,
    pub clocks: NewsSessionInput,
    /// Required bitemporal graph snapshot selected by the experiment.
    pub graph_snapshot_id: String,
    /// Knowledge clock used to materialise `graph_snapshot_id`.
    pub graph_knowledge_time: i64,
}

/// One event that was actually available in the requested trading session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibleNewsBacktestEvent {
    pub revision_id: String,
    pub session: EffectiveNewsSession,
    pub graph_snapshot_id: String,
    pub graph_knowledge_time: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum EventBacktestError {
    #[error(transparent)]
    TradingRules(#[from] astock_trading_rules::Error),
    #[error("event backtest must select a graph-snapshot:* id")]
    MissingGraphSnapshot,
    #[error(
        "graph knowledge time {knowledge_time} is later than event availability {effective_at}"
    )]
    FutureGraphKnowledge {
        knowledge_time: i64,
        effective_at: i64,
    },
}

/// Select news for one trading date using the exact same classifier as the
/// live Agent and UI. `event_time_utc` cannot make an item visible before its
/// first-seen/revision availability.
pub fn news_events_for_session(
    rules: &RuleSet,
    events: &[NewsBacktestEvent],
    target: NaiveDate,
) -> Result<Vec<EligibleNewsBacktestEvent>, EventBacktestError> {
    let mut eligible = Vec::new();
    for event in events {
        let session = classify_news_session(rules, &event.clocks)?;
        if !event.graph_snapshot_id.starts_with("graph-snapshot:") {
            return Err(EventBacktestError::MissingGraphSnapshot);
        }
        if event.graph_knowledge_time > session.effective_at_utc {
            return Err(EventBacktestError::FutureGraphKnowledge {
                knowledge_time: event.graph_knowledge_time,
                effective_at: session.effective_at_utc,
            });
        }
        if session.target_trading_date == target
            && session.role != EffectiveSessionRole::HistoricalOnly
        {
            eligible.push(EligibleNewsBacktestEvent {
                revision_id: event.revision_id.clone(),
                session,
                graph_snapshot_id: event.graph_snapshot_id.clone(),
                graph_knowledge_time: event.graph_knowledge_time,
            });
        }
    }
    eligible.sort_by_key(|event| (event.session.effective_at_utc, event.revision_id.clone()));
    Ok(eligible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use astock_trading_rules::PublicationPrecision;
    use chrono::{FixedOffset, TimeZone};

    fn china(value: &str) -> i64 {
        let local = chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").unwrap();
        FixedOffset::east_opt(8 * 3_600)
            .unwrap()
            .from_local_datetime(&local)
            .single()
            .unwrap()
            .timestamp()
    }

    #[test]
    fn event_time_never_backfills_information_before_first_seen() {
        let rules = RuleSet::from_json(astock_trading_rules::EMBEDDED_RULES_JSON).unwrap();
        let event = NewsBacktestEvent {
            revision_id: "rev:correction".into(),
            clocks: NewsSessionInput {
                event_time_utc: Some(china("2026-08-20 09:30:00")),
                publish_time_utc: Some(china("2026-08-21 10:00:00")),
                first_seen_time_utc: china("2026-08-21 10:01:00"),
                revision_time_utc: Some(china("2026-08-21 15:01:00")),
                publication_precision: PublicationPrecision::ExactTime,
                stale: false,
                verified: true,
                discovery_only: false,
                old_republication: false,
            },
            graph_snapshot_id: "graph-snapshot:test".into(),
            graph_knowledge_time: china("2026-08-21 10:01:00"),
        };
        let friday = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();
        assert!(
            news_events_for_session(&rules, std::slice::from_ref(&event), friday)
                .unwrap()
                .is_empty()
        );
        let eligible = news_events_for_session(&rules, &[event], monday).unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(
            eligible[0].session.effective_at_utc,
            china("2026-08-21 15:01:00")
        );
    }

    #[test]
    fn future_graph_knowledge_is_rejected_instead_of_leaking() {
        let rules = RuleSet::from_json(astock_trading_rules::EMBEDDED_RULES_JSON).unwrap();
        let event = NewsBacktestEvent {
            revision_id: "rev:future-graph".into(),
            clocks: NewsSessionInput {
                event_time_utc: Some(china("2026-08-21 09:00:00")),
                publish_time_utc: Some(china("2026-08-21 10:00:00")),
                first_seen_time_utc: china("2026-08-21 10:01:00"),
                revision_time_utc: None,
                publication_precision: PublicationPrecision::ExactTime,
                stale: false,
                verified: true,
                discovery_only: false,
                old_republication: false,
            },
            graph_snapshot_id: "graph-snapshot:future".into(),
            graph_knowledge_time: china("2026-08-24 10:01:00"),
        };
        let friday = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        assert!(matches!(
            news_events_for_session(&rules, &[event], friday),
            Err(EventBacktestError::FutureGraphKnowledge { .. })
        ));
    }
}
