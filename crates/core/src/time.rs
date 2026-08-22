//! China timezone helpers and minimal trading-time utilities.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, Utc};

/// China Standard Time, UTC+8 (no DST).
pub fn china_tz() -> FixedOffset {
    // +8 * 3600; east offset, always valid.
    FixedOffset::east_opt(8 * 3600).expect("+08:00 is a valid offset")
}

/// Current time in China Standard Time.
pub fn now_china() -> DateTime<FixedOffset> {
    utc_now().with_timezone(&china_tz())
}

/// Current UTC time. The workspace pins chrono without its `clock` feature,
/// so this goes through `SystemTime` instead of `Utc::now()`.
pub fn utc_now() -> DateTime<Utc> {
    std::time::SystemTime::now().into()
}

/// Whether `date` is a weekday. This is *not* a trading calendar — public
/// holidays are not modelled — hence the name.
pub fn is_weekday(date: NaiveDate) -> bool {
    date.weekday().num_days_from_monday() < 5
}

/// Loose "could be trading right now" check: weekday and within 09:00–15:30
/// China time. Deliberately coarse; use upstream emptiness as ground truth.
pub fn is_plausible_trading_time(now: DateTime<FixedOffset>) -> bool {
    use chrono::Timelike;
    if !is_weekday(now.date_naive()) {
        return false;
    }
    let minutes = now.hour() * 60 + now.minute();
    ((9 * 60)..=(15 * 60 + 30)).contains(&minutes)
}

/// Parse a naive datetime that may be a full `"YYYY-MM-DD HH:MM[:SS]"` or a
/// bare `"YYYY-MM-DD"` (interpreted as midnight). Accepts a trailing time
/// fragment after the date, mirroring the legacy `parts[0].split(" ")[-1]`
/// handling for minute rows.
pub fn parse_datetime_flexible(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt);
        }
    }
    // NaiveDateTime::parse_from_str rejects time-less formats ("premature end
    // of input"), so go through NaiveDate for the date-only case.
    parse_date(s).and_then(|d| d.and_hms_opt(0, 0, 0))
}

/// Parse a bare `"YYYY-MM-DD"` date.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn tz_is_plus_8() {
        let now = now_china();
        assert_eq!(now.offset().local_minus_utc(), 8 * 3600);
    }

    #[test]
    fn flexible_datetime_parse() {
        let d = parse_datetime_flexible("2025-08-21").unwrap();
        assert_eq!(d.date(), NaiveDate::from_ymd_opt(2025, 8, 21).unwrap());
        let t = parse_datetime_flexible("2025-08-21 09:31").unwrap();
        assert_eq!(t.time().hour(), 9);
        assert_eq!(t.time().minute(), 31);
        assert!(parse_datetime_flexible("nonsense").is_none());
    }
}
