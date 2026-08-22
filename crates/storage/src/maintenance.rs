//! Disk accounting, cleanup policies, and free-space probing.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// Size breakdown of the on-disk cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Total bytes of kline/fund-flow parquet files.
    pub kline_parquet_bytes: u64,
    /// Number of parquet files.
    pub kline_parquet_files: u64,
    /// Bytes of the SQLite metadata database file.
    pub sqlite_bytes: u64,
    /// Number of rows in `tool_cache`.
    pub tool_cache_rows: u64,
    /// Approximate bytes held by `tool_cache` (params + result JSON).
    pub tool_cache_bytes: u64,
    /// Approximate bytes held by chat messages (content + tool_calls).
    pub chat_bytes: u64,
}

impl CacheStats {
    /// Total on-disk footprint accounted here.
    pub fn total_bytes(&self) -> u64 {
        self.kline_parquet_bytes + self.sqlite_bytes + self.tool_cache_bytes + self.chat_bytes
    }
}

/// Cleanup policy: shrink cached data until the total is under the target.
#[derive(Debug, Clone, Copy)]
pub struct CleanupPolicy {
    /// Target for [`CacheStats::total_bytes`] after cleanup, in bytes.
    pub target_total_bytes: u64,
}

/// What a cleanup run did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanupReport {
    /// Expired `tool_cache` rows deleted.
    pub tool_cache_rows_deleted: u64,
    /// Parquet files evicted.
    pub parquet_files_deleted: u64,
    /// Bytes freed by parquet eviction (expired-row bytes are not counted).
    pub bytes_freed: u64,
}

/// Sum the sizes of the given files.
pub(crate) fn files_size(files: &[PathBuf]) -> u64 {
    files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// Pick parquet files to evict, least-recently-used first (file modification
/// time is used as the LRU proxy — reads do not bump it, which is acceptable
/// for a disk cache), until `bytes_to_free` have been selected.
pub(crate) fn select_evictions(files: &[PathBuf], bytes_to_free: u64) -> Vec<PathBuf> {
    let mut by_mtime: Vec<(std::time::SystemTime, PathBuf, u64)> = files
        .iter()
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((mtime, p.clone(), meta.len()))
        })
        .collect();
    by_mtime.sort_by_key(|(mtime, _, _)| *mtime);
    let mut picked = Vec::new();
    let mut freed = 0;
    for (_, path, size) in by_mtime {
        if freed >= bytes_to_free {
            break;
        }
        freed += size;
        picked.push(path);
    }
    picked
}

/// Free bytes available on the volume holding `path`.
///
/// Windows: calls `GetDiskFreeSpaceExW` from `kernel32` via a minimal
/// `extern "C"` block (no extra crates). Other platforms: `None` (unknown) —
/// callers must treat `None` as "no information", not "no space".
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    disk_free_bytes_impl(path)
}

#[cfg(windows)]
fn disk_free_bytes_impl(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;

    extern "C" {
        /// https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getdiskfreespaceexw
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_bytes_available: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free: u64 = 0;
    // SAFETY: `wide` is a valid null-terminated UTF-16 buffer that outlives
    // the call; `free` is a valid, aligned out-pointer; null pointers are
    // documented as accepted for the remaining outputs.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn disk_free_bytes_impl(_path: &Path) -> Option<u64> {
    // Unknown without platform APIs; documented fallback.
    None
}

/// Ensure cleanup can always run: recompute stats from scratch.
pub(crate) fn sqlite_file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}
