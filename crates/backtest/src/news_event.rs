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
}

/// One event that was actually available in the requested trading session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EligibleNewsBacktestEvent {
    pub revision_id: String,
    pub session: EffectiveNewsSession,
}

/// Select news for one trading date using the exact same classifier as the
/// live Agent and UI. `event_time_utc` cannot make an item visible before its
/// first-seen/revision availability.
pub fn news_events_for_session(
    rules: &RuleSet,
    events: &[NewsBacktestEvent],
    target: NaiveDate,
) -> astock_trading_rules::Result<Vec<EligibleNewsBacktestEvent>> {
    let mut eligible = Vec::new();
    for event in events {
        let session = classify_news_session(rules, &event.clocks)?;
        if session.target_trading_date == target
            && session.role != EffectiveSessionRole::HistoricalOnly
        {
            eligible.push(EligibleNewsBacktestEvent {
                revision_id: event.revision_id.clone(),
                session,
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
}
