//! Point-in-time news availability mapped to A-share trading sessions.
//!
//! The same deterministic function is used by live Agent context, the news
//! centre and event-backtest eligibility. Event time never controls when a
//! researcher was allowed to know a fact; publication, observation and
//! revision clocks do.

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::{Error, Result, RuleSet};

/// Precision actually supplied by the upstream publication clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPrecision {
    /// Source supplied a timezone-aware or normalized exact time.
    ExactTime,
    /// Source supplied only a calendar date. It is conservatively treated as
    /// 15:00 China Standard Time on that date.
    DateOnly,
    /// No publication clock; first system observation is the earliest usable
    /// time and the uncertainty must remain visible.
    Missing,
}

/// Role of the item relative to its first usable A-share session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveSessionRole {
    SameDayPremarket,
    Intraday,
    NextTradingDay,
    HistoricalOnly,
}

/// Fine-grained market phase at the first decision-usable instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveMarketPhase {
    Premarket,
    OpeningAuction,
    MorningTrading,
    LunchBreak,
    AfternoonTrading,
    ClosingAuction,
    AfterClose,
    NonTradingDay,
}

/// Whether this item may strengthen a decision or is only supporting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewsEvidenceUse {
    DecisionEvidence,
    VerificationLead,
    HistoricalContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsSessionInput {
    /// Event occurrence is retained for explanation only and never moves an
    /// item earlier in a live or historical decision timeline.
    pub event_time_utc: Option<i64>,
    pub publish_time_utc: Option<i64>,
    pub first_seen_time_utc: i64,
    pub revision_time_utc: Option<i64>,
    pub publication_precision: PublicationPrecision,
    pub stale: bool,
    pub verified: bool,
    pub discovery_only: bool,
    pub old_republication: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveNewsSession {
    pub target_trading_date: NaiveDate,
    pub role: EffectiveSessionRole,
    pub phase: EffectiveMarketPhase,
    pub effective_at_utc: i64,
    pub effective_at_china: String,
    pub publication_precision: PublicationPrecision,
    pub time_uncertain: bool,
    pub evidence_use: NewsEvidenceUse,
    pub can_increase_confidence: bool,
    pub rationale: String,
    pub rules_version: String,
}

/// Infer only the timestamp precision, never the event meaning. A normalized
/// timestamp without an original string is treated as exact because the
/// provider already supplied a machine clock; a visibly date-only original
/// remains conservative.
pub fn publication_precision_from_source(
    publish_time_utc: Option<i64>,
    original: Option<&str>,
) -> PublicationPrecision {
    if publish_time_utc.is_none() {
        return PublicationPrecision::Missing;
    }
    match original.map(str::trim) {
        Some(value)
            if value.len() <= 10 && value.chars().filter(char::is_ascii_digit).count() == 8 =>
        {
            PublicationPrecision::DateOnly
        }
        _ => PublicationPrecision::ExactTime,
    }
}

fn china_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3_600).expect("UTC+8 is a valid fixed offset")
}

fn china_time(timestamp: i64) -> Result<DateTime<FixedOffset>> {
    Utc.timestamp_opt(timestamp, 0)
        .single()
        .map(|value| value.with_timezone(&china_offset()))
        .ok_or(Error::InvalidTimestamp(timestamp))
}

fn at_china_time(date: NaiveDate, time: NaiveTime) -> Result<i64> {
    china_offset()
        .from_local_datetime(&date.and_time(time))
        .single()
        .map(|value| value.timestamp())
        .ok_or(Error::InvalidTimestamp(0))
}

fn phase(rules: &RuleSet, date: NaiveDate, time: NaiveTime) -> EffectiveMarketPhase {
    if !rules.is_trading_day(date) {
        return EffectiveMarketPhase::NonTradingDay;
    }
    let windows = &rules.data.auction;
    let open_start = NaiveTime::parse_from_str(&windows.open_call_auction.start, "%H:%M")
        .expect("validated rule time");
    let open_end = NaiveTime::parse_from_str(&windows.open_call_auction.end, "%H:%M")
        .expect("validated rule time");
    let morning_start = NaiveTime::parse_from_str(&windows.continuous_morning.start, "%H:%M")
        .expect("validated rule time");
    let morning_end = NaiveTime::parse_from_str(&windows.continuous_morning.end, "%H:%M")
        .expect("validated rule time");
    let afternoon_start = NaiveTime::parse_from_str(&windows.continuous_afternoon.start, "%H:%M")
        .expect("validated rule time");
    let afternoon_end = NaiveTime::parse_from_str(&windows.continuous_afternoon.end, "%H:%M")
        .expect("validated rule time");
    let close_start = NaiveTime::parse_from_str(&windows.close_call_auction.start, "%H:%M")
        .expect("validated rule time");
    let close_end = NaiveTime::parse_from_str(&windows.close_call_auction.end, "%H:%M")
        .expect("validated rule time");

    if time < open_start || (time >= open_end && time < morning_start) {
        EffectiveMarketPhase::Premarket
    } else if time < open_end {
        EffectiveMarketPhase::OpeningAuction
    } else if time < morning_end {
        EffectiveMarketPhase::MorningTrading
    } else if time < afternoon_start {
        EffectiveMarketPhase::LunchBreak
    } else if time < afternoon_end {
        EffectiveMarketPhase::AfternoonTrading
    } else if time >= close_start && time < close_end {
        EffectiveMarketPhase::ClosingAuction
    } else {
        EffectiveMarketPhase::AfterClose
    }
}

/// Map one immutable news revision to the first A-share session in which it
/// could have influenced a decision. This is the only supported availability
/// rule for both live context and historical replay.
pub fn classify_news_session(
    rules: &RuleSet,
    input: &NewsSessionInput,
) -> Result<EffectiveNewsSession> {
    let observed_at = input
        .revision_time_utc
        .unwrap_or(input.first_seen_time_utc)
        .max(input.first_seen_time_utc);
    let (published_at, time_uncertain, clock_reason) = match input.publication_precision {
        PublicationPrecision::ExactTime => (
            input.publish_time_utc.unwrap_or(observed_at),
            input.publish_time_utc.is_none(),
            if input.publish_time_utc.is_some() {
                "精确发布时间"
            } else {
                "精确发布时间缺失，退回系统首次发现"
            },
        ),
        PublicationPrecision::DateOnly => {
            let date = china_time(input.publish_time_utc.unwrap_or(observed_at))?.date_naive();
            (
                at_china_time(
                    date,
                    NaiveTime::from_hms_opt(15, 0, 0).expect("valid boundary"),
                )?,
                true,
                "来源仅提供日期，保守按当日15:00处理",
            )
        }
        PublicationPrecision::Missing => (
            observed_at,
            true,
            "来源未提供发布时间，以系统首次发现/修订时间为最早可用时点",
        ),
    };
    let effective_at = published_at.max(observed_at);
    let local = china_time(effective_at)?;
    let local_date = local.date_naive();
    let local_time = local.time();
    let phase = phase(rules, local_date, local_time);
    let close = NaiveTime::parse_from_str(&rules.data.auction.close_call_auction.end, "%H:%M")
        .expect("validated rule time");
    let open = NaiveTime::parse_from_str(&rules.data.auction.open_call_auction.start, "%H:%M")
        .expect("validated rule time");
    let (target_trading_date, base_role, boundary_reason) = if !rules.is_trading_day(local_date) {
        (
            rules.next_trading_day(local_date),
            EffectiveSessionRole::NextTradingDay,
            "该时点为休市日，归入下一交易日",
        )
    } else if local_time >= close {
        (
            rules.next_trading_day(local_date),
            EffectiveSessionRole::NextTradingDay,
            "该时点不早于15:00收盘边界，归入下一交易日",
        )
    } else if local_time < open {
        (
            local_date,
            EffectiveSessionRole::SameDayPremarket,
            "该时点早于09:15集合竞价，归入当日盘前",
        )
    } else {
        (
            local_date,
            EffectiveSessionRole::Intraday,
            "该时点位于当日可交易决策窗口（含集合竞价与午休）",
        )
    };
    let role = if input.old_republication {
        EffectiveSessionRole::HistoricalOnly
    } else {
        base_role
    };
    let evidence_use = if input.old_republication {
        NewsEvidenceUse::HistoricalContext
    } else if input.stale || !input.verified || input.discovery_only {
        NewsEvidenceUse::VerificationLead
    } else {
        NewsEvidenceUse::DecisionEvidence
    };
    let can_increase_confidence = evidence_use == NewsEvidenceUse::DecisionEvidence;
    let quality_reason = match evidence_use {
        NewsEvidenceUse::DecisionEvidence => "来源与新鲜度允许作为决策证据",
        NewsEvidenceUse::VerificationLead => "stale、未核验或仅发现线索不得提高仓位/结论置信度",
        NewsEvidenceUse::HistoricalContext => "旧闻重发仅作历史背景，不得重复视为新催化",
    };
    Ok(EffectiveNewsSession {
        target_trading_date,
        role,
        phase,
        effective_at_utc: effective_at,
        effective_at_china: local.format("%Y-%m-%d %H:%M:%S %:z").to_string(),
        publication_precision: input.publication_precision,
        time_uncertain,
        evidence_use,
        can_increase_confidence,
        rationale: format!("{clock_reason}；{boundary_reason}；{quality_reason}"),
        rules_version: rules.data.version.clone(),
    })
}

/// Target A-share trading date for a live decision taken at `now_utc`.
pub fn target_trading_date_at(rules: &RuleSet, now_utc: i64) -> Result<NaiveDate> {
    classify_news_session(
        rules,
        &NewsSessionInput {
            event_time_utc: None,
            publish_time_utc: Some(now_utc),
            first_seen_time_utc: now_utc,
            revision_time_utc: Some(now_utc),
            publication_precision: PublicationPrecision::ExactTime,
            stale: false,
            verified: true,
            discovery_only: false,
            old_republication: false,
        },
    )
    .map(|session| session.target_trading_date)
}

/// PIT-safe event-backtest gate. Only first-seen/revision availability is
/// used; an earlier event clock can never make the item eligible.
pub fn eligible_for_target_session(
    rules: &RuleSet,
    input: &NewsSessionInput,
    target: NaiveDate,
) -> Result<bool> {
    let session = classify_news_session(rules, input)?;
    Ok(session.target_trading_date == target
        && session.role != EffectiveSessionRole::HistoricalOnly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn rules() -> RuleSet {
        RuleSet::from_json(crate::EMBEDDED_RULES_JSON).unwrap()
    }

    fn utc(value: &str) -> i64 {
        NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_utc()
            .timestamp()
    }

    fn china(value: &str) -> i64 {
        let local = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").unwrap();
        china_offset()
            .from_local_datetime(&local)
            .single()
            .unwrap()
            .timestamp()
    }

    fn exact(value: &str) -> NewsSessionInput {
        let timestamp = china(value);
        NewsSessionInput {
            event_time_utc: Some(timestamp - 86_400),
            publish_time_utc: Some(timestamp),
            first_seen_time_utc: timestamp,
            revision_time_utc: Some(timestamp),
            publication_precision: PublicationPrecision::ExactTime,
            stale: false,
            verified: true,
            discovery_only: false,
            old_republication: false,
        }
    }

    #[test]
    fn friday_close_boundary_is_exact_and_never_leaks_backward() {
        let rules = rules();
        let before = classify_news_session(&rules, &exact("2026-08-21 14:59:59")).unwrap();
        assert_eq!(before.target_trading_date.to_string(), "2026-08-21");
        assert_eq!(before.role, EffectiveSessionRole::Intraday);

        let at_close = classify_news_session(&rules, &exact("2026-08-21 15:00:00")).unwrap();
        assert_eq!(at_close.target_trading_date.to_string(), "2026-08-24");
        assert_eq!(at_close.role, EffectiveSessionRole::NextTradingDay);
    }

    #[test]
    fn holiday_weekend_auction_and_lunch_have_one_calendar_semantics() {
        let rules = rules();
        assert!(!rules.is_trading_day(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()));
        assert!(!rules.is_trading_day(NaiveDate::from_ymd_opt(2026, 2, 23).unwrap()));
        assert!(rules.is_trading_day(NaiveDate::from_ymd_opt(2026, 10, 8).unwrap()));
        let spring_festival = classify_news_session(&rules, &exact("2026-02-23 10:00:00")).unwrap();
        assert_eq!(spring_festival.phase, EffectiveMarketPhase::NonTradingDay);
        assert_eq!(
            spring_festival.target_trading_date.to_string(),
            "2026-02-24"
        );

        let auction = classify_news_session(&rules, &exact("2026-08-21 09:20:00")).unwrap();
        assert_eq!(auction.phase, EffectiveMarketPhase::OpeningAuction);
        let lunch = classify_news_session(&rules, &exact("2026-08-21 12:00:00")).unwrap();
        assert_eq!(lunch.phase, EffectiveMarketPhase::LunchBreak);
    }

    #[test]
    fn date_only_missing_and_overseas_time_are_conservative() {
        let rules = rules();
        let mut date_only = exact("2026-08-21 08:00:00");
        date_only.publication_precision = PublicationPrecision::DateOnly;
        let result = classify_news_session(&rules, &date_only).unwrap();
        assert!(result.time_uncertain);
        assert_eq!(result.target_trading_date.to_string(), "2026-08-24");

        let mut missing = exact("2026-08-21 10:00:00");
        missing.publish_time_utc = None;
        missing.publication_precision = PublicationPrecision::Missing;
        assert!(
            classify_news_session(&rules, &missing)
                .unwrap()
                .time_uncertain
        );

        // 20:00 UTC Friday is 04:00 Saturday in China and therefore Monday.
        let mut overseas = exact("2026-08-21 10:00:00");
        let timestamp = utc("2026-08-21 20:00:00");
        overseas.publish_time_utc = Some(timestamp);
        overseas.first_seen_time_utc = timestamp;
        overseas.revision_time_utc = Some(timestamp);
        let result = classify_news_session(&rules, &overseas).unwrap();
        assert_eq!(result.phase, EffectiveMarketPhase::NonTradingDay);
        assert_eq!(result.target_trading_date.to_string(), "2026-08-24");
    }

    #[test]
    fn revision_clock_and_evidence_quality_block_false_catalysts() {
        let rules = rules();
        let mut correction = exact("2026-08-21 10:00:00");
        correction.revision_time_utc = Some(china("2026-08-21 16:00:00"));
        correction.stale = true;
        let result = classify_news_session(&rules, &correction).unwrap();
        assert_eq!(result.target_trading_date.to_string(), "2026-08-24");
        assert_eq!(result.evidence_use, NewsEvidenceUse::VerificationLead);
        assert!(!result.can_increase_confidence);

        correction.old_republication = true;
        let old = classify_news_session(&rules, &correction).unwrap();
        assert_eq!(old.role, EffectiveSessionRole::HistoricalOnly);
        assert!(
            !eligible_for_target_session(&rules, &correction, old.target_trading_date).unwrap()
        );
    }

    #[test]
    fn publication_precision_is_conservative_for_date_only_sources() {
        assert_eq!(
            publication_precision_from_source(Some(1), Some("2026-08-21")),
            PublicationPrecision::DateOnly
        );
        assert_eq!(
            publication_precision_from_source(Some(1), Some("2026-08-21T10:00:00+08:00")),
            PublicationPrecision::ExactTime
        );
        assert_eq!(
            publication_precision_from_source(None, None),
            PublicationPrecision::Missing
        );
    }

    #[test]
    fn versioned_override_can_add_temporary_exchange_closure() {
        let mut data: serde_json::Value = serde_json::from_str(crate::EMBEDDED_RULES_JSON).unwrap();
        data["calendar"]["holidays"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("2026-08-21"));
        let rules = RuleSet::from_json(&serde_json::to_string(&data).unwrap()).unwrap();
        let result = classify_news_session(&rules, &exact("2026-08-21 10:00:00")).unwrap();
        assert_eq!(result.phase, EffectiveMarketPhase::NonTradingDay);
        assert_eq!(result.target_trading_date.to_string(), "2026-08-24");
    }
}
