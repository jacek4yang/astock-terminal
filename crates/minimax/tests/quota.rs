//! Quota fixture parsing tests (no network).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use astock_minimax::{QuotaStatus, THROTTLE_PERCENT};

fn parse(body: &str) -> QuotaStatus {
    astock_minimax::quota::parse_remains(body.as_bytes()).unwrap()
}

const FULL_FIXTURE: &str = r#"{
    "model_remains": [
        {
            "start_time": 1755763200000,
            "end_time": 1755781200000,
            "remains_time": 3600000,
            "current_interval_total_count": 1500,
            "current_interval_usage_count": 300,
            "model_name": "MiniMax-M2.5",
            "current_weekly_total_count": 15000,
            "current_weekly_usage_count": 1500,
            "weekly_start_time": 1755504000000,
            "weekly_end_time": 1756108800000,
            "weekly_remains_time": 345600000,
            "current_interval_status": 1,
            "current_interval_remaining_percent": 80,
            "current_weekly_status": 1,
            "current_weekly_remaining_percent": 90
        },
        {
            "start_time": 1755763200000,
            "end_time": 1755781200000,
            "remains_time": 0,
            "current_interval_total_count": 100,
            "current_interval_usage_count": 100,
            "model_name": "MiniMax-M2",
            "current_weekly_total_count": 1000,
            "current_weekly_usage_count": 999,
            "weekly_start_time": 1755504000000,
            "weekly_end_time": 1756108800000,
            "weekly_remains_time": 345600000,
            "current_interval_status": 2,
            "current_interval_remaining_percent": 0,
            "current_weekly_status": 1,
            "current_weekly_remaining_percent": 0.1
        }
    ],
    "base_resp": {"status_code": 0, "status_msg": "success"}
}"#;

#[test]
fn parses_full_fixture() {
    let quota = parse(FULL_FIXTURE);
    assert_eq!(quota.models.len(), 2);

    let m25 = quota.model("MiniMax-M2.5").unwrap();
    assert_eq!(m25.current_interval_total_count, Some(1500));
    assert_eq!(m25.current_interval_usage_count, Some(300));
    assert_eq!(m25.current_interval_remaining_percent, Some(80.0));
    assert_eq!(m25.current_weekly_remaining_percent, Some(90.0));
    assert_eq!(m25.remains_time, Some(3_600_000));
    assert_eq!(
        m25.interval_reset_at(),
        Some(UNIX_EPOCH + Duration::from_millis(1_755_781_200_000))
    );

    assert!(!quota.throttled("MiniMax-M2.5"));
    assert!(!quota.exhausted("MiniMax-M2.5"));
    assert_eq!(quota.pacing("MiniMax-M2.5").min_interval, Duration::ZERO);
}

#[test]
fn exhausted_window_detected() {
    let quota = parse(FULL_FIXTURE);
    let m2 = quota.model("MiniMax-M2").unwrap();
    assert!(m2.interval_exhausted());
    assert!(quota.exhausted("MiniMax-M2"));
    assert!(quota.throttled("MiniMax-M2"));
    assert_eq!(
        quota.window_reset_at("MiniMax-M2"),
        Some(UNIX_EPOCH + Duration::from_millis(1_755_781_200_000))
    );
    assert_eq!(
        quota.pacing("MiniMax-M2").min_interval,
        Duration::from_secs(60)
    );
}

#[test]
fn pacing_ramps_up_as_quota_shrinks() {
    let with_percent = |p: u64| {
        let body = format!(
            r#"{{"model_remains":[{{"model_name":"M","current_interval_total_count":100,"current_interval_usage_count":{},"current_interval_remaining_percent":{}}}],"base_resp":{{"status_code":0,"status_msg":"success"}}}}"#,
            100 - p,
            p
        );
        parse(&body)
    };
    assert_eq!(with_percent(90).pacing("M").min_interval, Duration::ZERO);
    assert_eq!(
        with_percent(15).pacing("M").min_interval,
        Duration::from_secs(2)
    );
    let low = with_percent(THROTTLE_PERCENT as u64);
    assert!(low.throttled("M"));
    assert_eq!(low.pacing("M").min_interval, Duration::from_secs(10));
    // Unknown model: no opinion, no blocking.
    let q = with_percent(90);
    assert!(!q.throttled("nope"));
    assert!(!q.exhausted("nope"));
    assert_eq!(q.pacing("nope").min_interval, Duration::ZERO);
}

#[test]
fn tolerates_string_numbers_and_missing_fields() {
    let quota = parse(
        r#"{"model_remains":[{"model_name":"M","current_interval_remaining_percent":"42.5","current_interval_total_count":"10"}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
    );
    let m = quota.model("M").unwrap();
    assert_eq!(m.current_interval_remaining_percent, Some(42.5));
    assert_eq!(m.current_interval_total_count, Some(10));
    assert_eq!(m.current_interval_usage_count, None);
    assert!(!quota.exhausted("M"));
}

#[test]
fn falls_back_to_counters_when_percent_missing() {
    let quota = parse(
        r#"{"model_remains":[{"model_name":"M","current_interval_total_count":10,"current_interval_usage_count":10}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
    );
    assert!(quota.exhausted("M"));
}

#[test]
fn keeps_unknown_fields_for_forward_compat() {
    let quota = parse(
        r#"{"model_remains":[{"model_name":"M","brand_new_field":{"nested":true}}],"base_resp":{"status_code":0,"status_msg":"success"}}"#,
    );
    let m = quota.model("M").unwrap();
    assert!(m.extra.contains_key("brand_new_field"));
}

#[test]
fn fetched_at_is_recent() {
    let quota = parse(FULL_FIXTURE);
    let age = SystemTime::now()
        .duration_since(quota.fetched_at)
        .unwrap_or_default();
    assert!(age < Duration::from_secs(5));
}
