//! Storage configuration.

use std::path::PathBuf;

/// Configuration for [`crate::Storage`].
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Root directory for all persisted data. Defaults to
    /// `%APPDATA%/astock-terminal` on Windows and `~/.astock-terminal`
    /// elsewhere. Missing directories are created on open; storage never
    /// panics on absent paths.
    pub base_dir: PathBuf,
    /// Disk budget for cached data (kline parquet + tool cache), in MiB.
    /// Used by [`crate::Storage::cleanup`] when no explicit target is given.
    pub cache_budget_mb: u64,
    /// Capacity (entry count) of the in-memory LRU caches.
    pub mem_cache_entries: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            base_dir: default_base_dir(),
            cache_budget_mb: 512,
            mem_cache_entries: 256,
        }
    }
}

impl StorageConfig {
    /// Config rooted at a specific directory (tests, portable installs).
    pub fn with_base_dir(base_dir: impl Into<PathBuf>) -> Self {
        StorageConfig {
            base_dir: base_dir.into(),
            ..Default::default()
        }
    }

    /// Path of the SQLite metadata database.
    pub(crate) fn meta_db_path(&self) -> PathBuf {
        self.base_dir.join("meta.db")
    }

    /// Root of the Parquet time-series cache.
    pub(crate) fn timeseries_dir(&self) -> PathBuf {
        self.base_dir.join("timeseries")
    }
}

fn default_base_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("astock-terminal");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".astock-terminal");
        }
    }
    // Last-resort fallback: a directory under the temp dir. Never panics.
    std::env::temp_dir().join("astock-terminal")
}
