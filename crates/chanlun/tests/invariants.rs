//! Property-based invariants over random kline sequences:
//!
//! - merged klines never overlap in the containment sense (adjacent merged
//!   segments are strictly separated on one side);
//! - raw kline counts and date ranges are preserved by the merge;
//! - fractals sit strictly inside the merged series with matching extremes;
//! - strokes alternate direction and chain end-to-start through the fractal
//!   list;
//! - 5-minute aggregation conserves volume and price envelopes.

use astock_chanlun::daily::{
    find_daily_fractals, find_daily_strokes, merge_daily_klines,
};
use astock_chanlun::minute::{construct_5min_klines, merge_klines, MinuteKline};
use proptest::prelude::*;

/// Random raw klines as (dates, highs, lows) with high >= low.
fn daily_klines() -> impl Strategy<Value = (Vec<String>, Vec<f64>, Vec<f64>)> {
    prop::collection::vec((0.0f64..1000.0, 0.01f64..50.0), 1..160).prop_map(|v| {
        let dates: Vec<String> = (0..v.len()).map(|i| format!("2026-01-{i:03}")).collect();
        let highs: Vec<f64> = v.iter().map(|(c, w)| c + w).collect();
        let lows: Vec<f64> = v.iter().map(|(c, w)| c - w).collect();
        (dates, highs, lows)
    })
}

/// Random minute bars: monotone increasing minute timestamps with random
/// gaps (session breaks), random prices and volumes.
fn minute_bars() -> impl Strategy<Value = (Vec<String>, Vec<f64>, Vec<f64>)> {
    // Steps stay small so formatted hours keep two digits (string order ==
    // chronological order) and gaps model session breaks.
    prop::collection::vec((1i64..10, 0.0f64..100.0, 0.0f64..1000.0), 0..240).prop_map(|v| {
        let mut minute = 9 * 60 + 30;
        let mut times = Vec::with_capacity(v.len());
        let mut prices = Vec::with_capacity(v.len());
        let mut volumes = Vec::with_capacity(v.len());
        for (step, price, vol) in v {
            times.push(format!("{:02}:{:02}", minute / 60, minute % 60));
            prices.push(price);
            volumes.push(vol);
            minute += step;
        }
        (times, prices, volumes)
    })
}

proptest! {
    #[test]
    fn merged_klines_never_contain_neighbours((dates, highs, lows) in daily_klines()) {
        let merged = merge_daily_klines(&dates, &highs, &lows);
        prop_assert!(!merged.is_empty());
        // Raw counts and date span are preserved.
        let total: usize = merged.iter().map(|m| m.raw_count).sum();
        prop_assert_eq!(total, dates.len());
        prop_assert_eq!(merged[0].date_start.as_str(), dates[0].as_str());
        prop_assert_eq!(merged.last().unwrap().date_end.as_str(), dates.last().unwrap().as_str());
        prop_assert_eq!(merged[0].direction, 0);
        // Adjacent merged klines are strictly separated on one side, which
        // implies no containment between them.
        for w in merged.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let up = b.high > a.high && b.low > a.low;
            let down = b.high < a.high && b.low < a.low;
            prop_assert!(up || down, "adjacent merged klines overlap: {a:?} {b:?}");
            let contained = (b.high <= a.high && b.low >= a.low)
                || (b.high >= a.high && b.low <= a.low);
            prop_assert!(!contained);
            prop_assert_eq!(b.direction, if up { 1 } else { -1 });
        }
    }

    #[test]
    fn minute_merge_matches_daily_invariants((dates, highs, lows) in daily_klines()) {
        let klines: Vec<MinuteKline> = dates
            .iter()
            .zip(&highs)
            .zip(&lows)
            .map(|((t, &h), &l)| MinuteKline {
                time: t.clone(),
                open: l,
                close: h,
                high: h,
                low: l,
                volume: 1.0,
            })
            .collect();
        let merged = merge_klines(&klines);
        let total: usize = merged.iter().map(|m| m.raw_count).sum();
        prop_assert_eq!(total, klines.len());
        for w in merged.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let contained = (b.high <= a.high && b.low >= a.low)
                || (b.high >= a.high && b.low <= a.low);
            prop_assert!(!contained, "adjacent merged klines overlap: {a:?} {b:?}");
        }
    }

    #[test]
    fn strokes_alternate_and_chain((dates, highs, lows) in daily_klines()) {
        let merged = merge_daily_klines(&dates, &highs, &lows);
        let fractals = find_daily_fractals(&merged);
        // Fractals sit strictly inside the series and match the merged extreme.
        for (pos, f) in fractals.iter().enumerate() {
            prop_assert!(f.index >= 1 && f.index + 1 < merged.len());
            if pos > 0 {
                prop_assert!(f.index > fractals[pos - 1].index);
            }
            let m = &merged[f.index];
            match f.fractal_type.as_str() {
                "top" => prop_assert_eq!(f.price, m.high),
                "bottom" => prop_assert_eq!(f.price, m.low),
                other => prop_assert!(false, "unknown fractal type {other}"),
            }
            prop_assert_eq!(f.date.as_str(), m.date_end.as_str());
        }
        let strokes = find_daily_strokes(&fractals);
        for (i, s) in strokes.iter().enumerate() {
            // Strokes strictly alternate direction. The next stroke starts at
            // or after the previous stroke's end (the start may migrate
            // forward to a more extreme same-type fractal before the new
            // endpoint is found, per the legacy absorption rule).
            if i > 0 {
                let prev = &strokes[i - 1];
                prop_assert_ne!(s.direction.as_str(), prev.direction.as_str());
                prop_assert!(s.start_idx >= prev.end_idx);
            }
            prop_assert!(s.start_idx < s.end_idx);
            prop_assert!(s.end_idx < fractals.len());
            // Endpoints agree with the referenced fractals.
            let sf = &fractals[s.start_idx];
            let ef = &fractals[s.end_idx];
            prop_assert_eq!(s.start_price, sf.price);
            prop_assert_eq!(s.end_price, ef.price);
            prop_assert_eq!(s.start_date.as_str(), sf.date.as_str());
            prop_assert_eq!(s.end_date.as_str(), ef.date.as_str());
            // A down stroke starts at a top and ends at a bottom (and vice
            // versa), and the merged-index gap rule is respected.
            prop_assert_ne!(sf.fractal_type.as_str(), ef.fractal_type.as_str());
            prop_assert!(ef.index >= sf.index + 4);
            match s.direction.as_str() {
                "up" => {
                    prop_assert_eq!(sf.fractal_type.as_str(), "bottom");
                    prop_assert_eq!(ef.fractal_type.as_str(), "top");
                }
                "down" => {
                    prop_assert_eq!(sf.fractal_type.as_str(), "top");
                    prop_assert_eq!(ef.fractal_type.as_str(), "bottom");
                }
                other => prop_assert!(false, "unknown stroke direction {other}"),
            }
        }
    }

    #[test]
    fn aggregation_conserves_volume_and_envelope((times, prices, volumes) in minute_bars()) {
        let klines = construct_5min_klines(&times, &prices, &volumes);
        let total_in: f64 = volumes.iter().sum();
        let total_out: f64 = klines.iter().map(|k| k.volume).sum();
        prop_assert!((total_in - total_out).abs() <= 1e-6 * total_in.abs().max(1.0));
        if times.is_empty() {
            prop_assert!(klines.is_empty());
            return Ok(());
        }
        // Session breaks can make every bar its own group, so the kline
        // count is bounded by the bar count.
        prop_assert!(klines.len() <= times.len());
        let mut prev_time = String::new();
        for k in &klines {
            prop_assert!(k.high >= k.low);
            prop_assert!(k.high >= k.open && k.high >= k.close);
            prop_assert!(k.low <= k.open && k.low <= k.close);
            prop_assert!(times.contains(&k.time));
            prop_assert!(k.time.as_str() > prev_time.as_str());
            prev_time = k.time.clone();
        }
        prop_assert_eq!(klines.last().unwrap().time.as_str(), times.last().unwrap().as_str());
    }
}
