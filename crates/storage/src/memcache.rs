//! In-memory LRU cache for hot values.

use std::num::NonZeroUsize;

use lru::LruCache;
use parking_lot::Mutex;

/// A tiny thread-safe LRU cache wrapper (lru + parking_lot).
///
/// Used for hot tool-cache results and recently-read kline bar vectors.
pub struct MemCache<V> {
    inner: Mutex<LruCache<String, V>>,
}

impl<V: Clone> MemCache<V> {
    /// Create a cache holding at most `capacity` entries (clamped to >= 1).
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        MemCache {
            inner: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Look up `key`, promoting the entry to most-recently-used.
    pub fn get(&self, key: &str) -> Option<V> {
        self.inner.lock().get(key).cloned()
    }

    /// Insert or replace `key`, evicting the LRU entry when full.
    pub fn put(&self, key: String, value: V) {
        self.inner.lock().put(key, value);
    }

    /// Drop a single entry.
    pub fn invalidate(&self, key: &str) {
        self.inner.lock().pop(key);
    }

    /// Number of entries currently held.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop all entries.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used() {
        let cache: MemCache<i32> = MemCache::new(2);
        cache.put("a".into(), 1);
        cache.put("b".into(), 2);
        assert_eq!(cache.get("a"), Some(1)); // promote "a"
        cache.put("c".into(), 3); // evicts "b"
        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(1));
        assert_eq!(cache.get("c"), Some(3));
    }
}
