//! Bounded TTL cache backed by `dashmap`.
//!
//! Fixes the legacy unbounded-dict bug: entries expire on read against their
//! TTL and the map is size-capped with oldest-first eviction.

use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use std::time::{Duration, Instant};

/// Standard TTLs, ported from the legacy cache call sites.
pub mod ttl {
    use std::time::Duration;

    /// Realtime data (quote, minute, realtime fund flow): 2s.
    pub const REALTIME: Duration = Duration::from_secs(2);
    /// Kline data: 15s.
    pub const KLINE: Duration = Duration::from_secs(15);
    /// Validated OHLCV base bars used by broad scans and later detailed
    /// analysis. Daily history is reusable for one minute; realtime quote
    /// freshness is handled by the separate quote path.
    pub const KLINE_BASE: Duration = Duration::from_secs(60);
    /// Search results and daily fund flow: 15s.
    pub const SEARCH: Duration = Duration::from_secs(15);
    /// Full A-share list: 60s.
    pub const ALL_A: Duration = Duration::from_secs(60);
    /// Market breadth: 120s.
    pub const BREADTH: Duration = Duration::from_secs(120);

    /// Longest TTL in use; anything older is unconditionally evictable.
    pub const MAX: Duration = BREADTH;
}

struct Entry {
    inserted: Instant,
    value: serde_json::Value,
}

/// A concurrent key-value cache with per-read TTL checks and a hard size cap.
pub struct TtlCache {
    map: DashMap<String, Entry>,
    cap: usize,
}

impl Default for TtlCache {
    fn default() -> Self {
        Self::new(2048)
    }
}

impl TtlCache {
    /// Create a cache holding at most `cap` entries.
    pub fn new(cap: usize) -> Self {
        TtlCache {
            map: DashMap::new(),
            cap: cap.max(1),
        }
    }

    /// Fetch and deserialize an entry if present and younger than `ttl`.
    pub fn get<T: DeserializeOwned>(&self, key: &str, ttl: Duration) -> Option<T> {
        let entry = self.map.get(key)?;
        if entry.inserted.elapsed() > ttl {
            drop(entry);
            self.map.remove(key);
            return None;
        }
        serde_json::from_value(entry.value.clone()).ok()
    }

    /// Insert an entry, evicting first if the cache is full.
    pub fn set<T: Serialize>(&self, key: &str, value: &T) {
        let Ok(value) = serde_json::to_value(value) else {
            return;
        };
        if self.map.len() >= self.cap && !self.map.contains_key(key) {
            self.evict();
        }
        self.map.insert(
            key.to_string(),
            Entry {
                inserted: Instant::now(),
                value,
            },
        );
    }

    /// Drop everything older than the max TTL, then the oldest ~10% if the
    /// cache is still at capacity.
    fn evict(&self) {
        self.map.retain(|_, e| e.inserted.elapsed() <= ttl::MAX);
        if self.map.len() >= self.cap {
            let mut keys_by_age: Vec<(String, Instant)> = self
                .map
                .iter()
                .map(|r| (r.key().clone(), r.value().inserted))
                .collect();
            keys_by_age.sort_by_key(|(_, t)| *t);
            let victims = (self.cap / 10).max(1);
            for (key, _) in keys_by_age.into_iter().take(victims) {
                self.map.remove(&key);
            }
        }
    }

    /// Number of live (not necessarily unexpired) entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_after_ttl() {
        let cache = TtlCache::new(16);
        cache.set("k", &42_i32);
        assert_eq!(cache.get::<i32>("k", Duration::from_secs(60)), Some(42));
        assert_eq!(cache.get::<i32>("k", Duration::from_millis(0)), None);
        // Expired entry is removed on read.
        assert!(cache.is_empty());
    }

    #[test]
    fn evicts_when_full() {
        let cache = TtlCache::new(4);
        for i in 0..4 {
            cache.set(&format!("k{i}"), &i);
        }
        cache.set("overflow", &99);
        assert!(cache.len() <= 4);
        assert_eq!(
            cache.get::<i32>("overflow", Duration::from_secs(60)),
            Some(99)
        );
    }

    #[test]
    fn overwrites_existing_key_without_eviction() {
        let cache = TtlCache::new(1);
        cache.set("a", &1_i32);
        cache.set("a", &2_i32);
        assert_eq!(cache.get::<i32>("a", Duration::from_secs(60)), Some(2));
        assert_eq!(cache.len(), 1);
    }
}
