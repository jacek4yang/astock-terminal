use astock_storage::{Storage, StorageConfig};
use rusqlite::{backup::Backup, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REDIRECT_FILE: &str = "active-data-root.json";
const REDIRECT_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "migration-manifest.json";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRootOrigin {
    ExplicitEnvironment,
    MigratedRedirect,
    LegacyCustom,
    LegacyAdopted,
    ProtonDefault,
    TemporaryFallback,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataRootDecision {
    pub path: PathBuf,
    pub origin: DataRootOrigin,
    pub legacy_path: Option<PathBuf>,
    pub copied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveDataRoot {
    version: u32,
    data_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migration_destination: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationFile {
    pub relative_path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub schema_version: u32,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub created_at: String,
    pub sqlite_integrity: String,
    pub total_bytes: u64,
    pub files: Vec<MigrationFile>,
    pub source_retained: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationOutcome {
    pub data_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub files_copied: usize,
    pub bytes_copied: u64,
    pub sqlite_integrity: String,
    pub source_retained: bool,
    pub restart_required: bool,
    pub compatibility_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RollbackOutcome {
    pub data_dir: PathBuf,
    pub migrated_copy: PathBuf,
    pub source_sqlite_integrity: String,
    pub source_retained: bool,
    pub migrated_copy_retained: bool,
    pub restart_required: bool,
}

pub async fn resolve_and_open() -> Result<(Storage, DataRootDecision), String> {
    if let Some(explicit) = non_empty_env("ASTOCK_DATA_DIR") {
        return open_decision(explicit, DataRootOrigin::ExplicitEnvironment, None);
    }

    let legacy = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("astock-terminal"));
    let proton_default = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("com.astock.terminal"));

    if let Some(default_root) = proton_default.as_ref() {
        let redirect = default_root.join(REDIRECT_FILE);
        if redirect.is_file() {
            let configured = read_active_redirect(&redirect)?;
            if !has_store(&configured) {
                return Err(format!(
                    "configured migrated data root is unavailable or incomplete: {}",
                    configured.display()
                ));
            }
            return open_decision(configured, DataRootOrigin::MigratedRedirect, legacy);
        }
    }

    if let Some(legacy_path) = legacy.as_ref().filter(|path| has_store(path)) {
        let legacy_storage = Storage::open(StorageConfig::with_base_dir(legacy_path))
            .map_err(|error| format!("open legacy store {}: {error}", legacy_path.display()))?;
        let configured = legacy_storage
            .settings_get("data_dir")
            .await
            .map_err(|error| format!("read legacy data_dir: {error}"))?;
        if let Some(custom) = configured
            .map(PathBuf::from)
            .filter(|path| path != legacy_path && has_store(path))
        {
            legacy_storage.shutdown();
            return open_decision(
                custom,
                DataRootOrigin::LegacyCustom,
                Some(legacy_path.clone()),
            );
        }
        return Ok((
            legacy_storage,
            DataRootDecision {
                path: legacy_path.clone(),
                origin: DataRootOrigin::LegacyAdopted,
                legacy_path: Some(legacy_path.clone()),
                copied: false,
            },
        ));
    }

    if let Some(local) = proton_default {
        return open_decision(local, DataRootOrigin::ProtonDefault, legacy);
    }

    open_decision(
        std::env::temp_dir().join("com.astock.terminal"),
        DataRootOrigin::TemporaryFallback,
        legacy,
    )
}

pub async fn migrate(storage: &Storage, requested: &str) -> Result<MigrationOutcome, String> {
    let bootstrap_root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "LOCALAPPDATA is unavailable; cannot persist migrated data root".to_string()
        })?
        .join("com.astock.terminal");
    migrate_with_bootstrap(storage, requested, &bootstrap_root).await
}

pub async fn rollback(storage: &Storage) -> Result<RollbackOutcome, String> {
    let bootstrap_root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "LOCALAPPDATA is unavailable; cannot restore the prior data root".to_string()
        })?
        .join("com.astock.terminal");
    rollback_with_bootstrap(storage, &bootstrap_root).await
}

async fn rollback_with_bootstrap(
    storage: &Storage,
    bootstrap_root: &Path,
) -> Result<RollbackOutcome, String> {
    let redirect_path = bootstrap_root.join(REDIRECT_FILE);
    if !redirect_path.is_file() {
        return Err("no migrated data-root activation is available to roll back".into());
    }
    let redirect = read_active_redirect_record(&redirect_path)?;
    let active_root = friendly_canonicalize(&redirect.data_root)
        .map_err(|error| format!("resolve active data root: {error}"))?;
    let migrated_copy = friendly_canonicalize(
        redirect
            .migration_destination
            .as_deref()
            .unwrap_or(&active_root),
    )
    .map_err(|error| format!("resolve migrated data root: {error}"))?;
    let manifest_path = migrated_copy.join(MANIFEST_FILE);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        format!(
            "read migration manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: MigrationManifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        format!(
            "parse migration manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest_destination = friendly_canonicalize(&manifest.destination)
        .map_err(|error| format!("resolve manifest destination: {error}"))?;
    if manifest.schema_version != 1 || manifest_destination != migrated_copy {
        return Err("migration manifest does not match the active migrated data root".into());
    }
    let source = friendly_canonicalize(&manifest.source)
        .map_err(|error| format!("resolve retained source data root: {error}"))?;
    let current = friendly_canonicalize(storage.base_dir())
        .map_err(|error| format!("resolve current Engine data root: {error}"))?;
    if current != source && current != migrated_copy {
        return Err("current Engine data root is unrelated to the active migration".into());
    }
    if source == migrated_copy || !has_store(&source) {
        return Err(
            "retained source data root is missing or invalid; rollback was not activated".into(),
        );
    }

    let source_database = source.join("meta.db");
    let source_for_check = source_database.clone();
    let integrity = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let connection = Connection::open_with_flags(
            &source_for_check,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|error| format!("open retained source database read-only: {error}"))?;
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| format!("verify retained source database: {error}"))?;
        if integrity != "ok" {
            return Err(format!(
                "retained source database integrity_check returned {integrity}"
            ));
        }
        Ok(integrity)
    })
    .await
    .map_err(|error| format!("rollback verification worker failed: {error}"))??;

    // The pointer is the only switched state. Neither the original nor the
    // migrated copy is deleted, so an interrupted rollback remains recoverable.
    write_active_redirect(bootstrap_root, &source, Some(&migrated_copy))?;
    Ok(RollbackOutcome {
        data_dir: source,
        migrated_copy,
        source_sqlite_integrity: integrity,
        source_retained: true,
        migrated_copy_retained: true,
        restart_required: true,
    })
}

async fn migrate_with_bootstrap(
    storage: &Storage,
    requested: &str,
    bootstrap_root: &Path,
) -> Result<MigrationOutcome, String> {
    let source = friendly_canonicalize(storage.base_dir())
        .map_err(|error| format!("resolve current data root: {error}"))?;
    let destination = prepare_destination(&source, requested)?;
    let parent = destination
        .parent()
        .ok_or_else(|| "destination must have a parent directory".to_string())?
        .to_path_buf();
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "destination directory name is invalid".to_string())?;
    let stage = parent.join(format!(
        ".{name}.astock-migration-{}-{}",
        std::process::id(),
        unix_seconds()
    ));
    if stage.exists() {
        return Err(format!(
            "migration staging path already exists: {}",
            stage.display()
        ));
    }
    fs::create_dir(&stage)
        .map_err(|error| format!("create migration staging directory: {error}"))?;
    let mut stage_guard = StageGuard::new(stage.clone());

    let source_for_copy = source.clone();
    let stage_for_copy = stage.clone();
    let mut files = tokio::task::spawn_blocking(move || {
        copy_non_database_snapshot(&source_for_copy, &stage_for_copy)
    })
    .await
    .map_err(|error| format!("migration copy worker failed: {error}"))??;

    let database_path = stage.join("meta.db");
    let database_for_backup = database_path.clone();
    let integrity = storage
        .run(move |source_connection| {
            let mut destination_connection = Connection::open(&database_for_backup)?;
            {
                let backup = Backup::new(source_connection, &mut destination_connection)?;
                backup.run_to_completion(128, Duration::from_millis(20), None)?;
            }
            let integrity: String =
                destination_connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(astock_storage::Error::Invalid(format!(
                    "SQLite backup integrity_check returned {integrity}"
                )));
            }
            destination_connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            Ok(integrity)
        })
        .await
        .map_err(|error| format!("create verified SQLite backup: {error}"))?;
    files.push(file_manifest_entry(&database_path, Path::new("meta.db"))?);
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));

    let total_bytes = files.iter().map(|entry| entry.bytes).sum();
    let manifest = MigrationManifest {
        schema_version: 1,
        source: source.clone(),
        destination: destination.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
        sqlite_integrity: integrity.clone(),
        total_bytes,
        files,
        source_retained: true,
    };
    write_json_synced(&stage.join(MANIFEST_FILE), &manifest)?;
    verify_manifest(&stage, &manifest)?;

    fs::rename(&stage, &destination)
        .map_err(|error| format!("atomically finalize migrated data directory: {error}"))?;
    stage_guard.disarm();

    write_active_redirect(bootstrap_root, &destination, Some(&destination))?;
    let compatibility_warning = storage
        .settings_set("data_dir", &destination.to_string_lossy())
        .await
        .err()
        .map(|error| format!("legacy data_dir compatibility pointer was not written: {error}"));

    Ok(MigrationOutcome {
        data_dir: destination.clone(),
        manifest_path: destination.join(MANIFEST_FILE),
        files_copied: manifest.files.len(),
        bytes_copied: total_bytes,
        sqlite_integrity: integrity,
        source_retained: true,
        restart_required: true,
        compatibility_warning,
    })
}

fn prepare_destination(source: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("destination data directory must not be empty".into());
    }
    let raw = PathBuf::from(requested);
    if !raw.is_absolute() || raw.file_name().is_none() {
        return Err("destination data directory must be an absolute non-root path".into());
    }
    if raw.exists() {
        return Err("destination must not already exist; choose a new empty path".into());
    }
    if raw.starts_with(source) || source.starts_with(&raw) {
        return Err("source and destination data directories must not overlap".into());
    }
    let parent = raw
        .parent()
        .ok_or_else(|| "destination must have a parent directory".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create destination parent {}: {error}", parent.display()))?;
    let parent = friendly_canonicalize(parent)
        .map_err(|error| format!("resolve destination parent: {error}"))?;
    let destination = parent.join(raw.file_name().expect("validated file name"));
    if destination.starts_with(source) || source.starts_with(&destination) {
        return Err("source and destination data directories must not overlap".into());
    }
    Ok(destination)
}

fn copy_non_database_snapshot(source: &Path, stage: &Path) -> Result<Vec<MigrationFile>, String> {
    let mut source_files = Vec::new();
    collect_regular_files(source, source, &mut source_files)?;
    let estimated = source_files
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|meta| meta.len()))
        .sum::<u64>()
        .saturating_add(
            fs::metadata(source.join("meta.db"))
                .map(|meta| meta.len())
                .unwrap_or(0),
        );
    let reserve = (estimated / 10).max(64 * 1024 * 1024);
    if astock_storage::disk_free_bytes(stage)
        .is_some_and(|free| free < estimated.saturating_add(reserve))
    {
        return Err(format!(
            "destination volume has insufficient free space; need at least {} bytes including reserve",
            estimated.saturating_add(reserve)
        ));
    }

    let mut manifest = Vec::with_capacity(source_files.len() + 1);
    for source_file in source_files {
        let relative = source_file
            .strip_prefix(source)
            .map_err(|error| format!("derive migration relative path: {error}"))?;
        let destination_file = stage.join(relative);
        if let Some(parent) = destination_file.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("create migration directory {}: {error}", parent.display())
            })?;
        }
        fs::copy(&source_file, &destination_file).map_err(|error| {
            format!(
                "copy migration file {} to {}: {error}",
                source_file.display(),
                destination_file.display()
            )
        })?;
        let source_hash = sha256_file(&source_file)?;
        let destination_hash = sha256_file(&destination_file)?;
        if source_hash != destination_hash {
            return Err(format!(
                "hash mismatch while copying {}",
                relative.display()
            ));
        }
        manifest.push(MigrationFile {
            relative_path: portable_relative_path(relative)?,
            bytes: fs::metadata(&destination_file)
                .map_err(|error| format!("read copied file metadata: {error}"))?
                .len(),
            sha256: destination_hash,
        });
    }
    Ok(manifest)
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("read data directory {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("read data directory entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("read file type {}: {error}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_symlink() {
            return Err(format!(
                "data migration refuses symbolic links or junctions: {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            collect_regular_files(root, &path, output)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("derive source relative path: {error}"))?;
            if !is_migration_control_file(relative) {
                output.push(path);
            }
        }
    }
    output.sort();
    Ok(())
}

fn is_migration_control_file(relative: &Path) -> bool {
    matches!(
        relative.to_string_lossy().replace('\\', "/").as_str(),
        "meta.db" | "meta.db-wal" | "meta.db-shm" | REDIRECT_FILE | MANIFEST_FILE
    ) || relative
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.starts_with("active-data-root.json.tmp-"))
}

fn portable_relative_path(path: &Path) -> Result<String, String> {
    let parts = path
        .components()
        .map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            format!(
                "non-Unicode path cannot be represented in manifest: {}",
                path.display()
            )
        })?;
    Ok(parts.join("/"))
}

fn friendly_canonicalize(path: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    #[cfg(windows)]
    {
        let value = canonical.to_string_lossy();
        if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = value.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(canonical)
}

fn file_manifest_entry(path: &Path, relative: &Path) -> Result<MigrationFile, String> {
    Ok(MigrationFile {
        relative_path: portable_relative_path(relative)?,
        bytes: fs::metadata(path)
            .map_err(|error| format!("read migration file metadata {}: {error}", path.display()))?
            .len(),
        sha256: sha256_file(path)?,
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let file = File::open(path)
        .map_err(|error| format!("open {} for hashing: {error}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("read {} for hashing: {error}", path.display()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_manifest(root: &Path, manifest: &MigrationManifest) -> Result<(), String> {
    for entry in &manifest.files {
        let relative = PathBuf::from(
            entry
                .relative_path
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        let path = root.join(relative);
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("verify migrated file {}: {error}", path.display()))?;
        if metadata.len() != entry.bytes || sha256_file(&path)? != entry.sha256 {
            return Err(format!(
                "migration manifest verification failed for {}",
                entry.relative_path
            ));
        }
    }
    Ok(())
}

fn write_active_redirect(
    root: &Path,
    data_root: &Path,
    migration_destination: Option<&Path>,
) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("create Proton bootstrap directory: {error}"))?;
    let final_path = root.join(REDIRECT_FILE);
    let temporary = root.join(format!("{REDIRECT_FILE}.tmp-{}", std::process::id()));
    write_json_synced(
        &temporary,
        &ActiveDataRoot {
            version: REDIRECT_VERSION,
            data_root: data_root.to_path_buf(),
            migration_destination: migration_destination.map(Path::to_path_buf),
        },
    )?;
    fs::rename(&temporary, &final_path)
        .map_err(|error| format!("atomically activate migrated data root: {error}"))?;
    Ok(())
}

fn read_active_redirect(path: &Path) -> Result<PathBuf, String> {
    Ok(read_active_redirect_record(path)?.data_root)
}

fn read_active_redirect_record(path: &Path) -> Result<ActiveDataRoot, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "read migrated data root pointer {}: {error}",
            path.display()
        )
    })?;
    let redirect: ActiveDataRoot = serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "parse migrated data root pointer {}: {error}",
            path.display()
        )
    })?;
    if redirect.version != REDIRECT_VERSION
        || !redirect.data_root.is_absolute()
        || redirect
            .migration_destination
            .as_ref()
            .is_some_and(|destination| !destination.is_absolute())
    {
        return Err(format!(
            "migrated data root pointer is invalid: {}",
            path.display()
        ));
    }
    Ok(redirect)
}

fn write_json_synced(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut file = File::create(path)
        .map_err(|error| format!("create migration metadata {}: {error}", path.display()))?;
    serde_json::to_writer_pretty(&mut file, value)
        .map_err(|error| format!("serialize migration metadata {}: {error}", path.display()))?;
    file.write_all(b"\n")
        .map_err(|error| format!("finish migration metadata {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("flush migration metadata {}: {error}", path.display()))?;
    Ok(())
}

fn open_decision(
    path: PathBuf,
    origin: DataRootOrigin,
    legacy_path: Option<PathBuf>,
) -> Result<(Storage, DataRootDecision), String> {
    let storage = Storage::open(StorageConfig::with_base_dir(&path))
        .map_err(|error| format!("open store {}: {error}", path.display()))?;
    Ok((
        storage,
        DataRootDecision {
            path,
            origin,
            legacy_path,
            copied: false,
        },
    ))
}

fn non_empty_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn has_store(path: &Path) -> bool {
    path.join("meta.db").is_file()
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct StageGuard {
    path: PathBuf,
    armed: bool,
}

impl StageGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StageGuard {
    fn drop(&mut self) {
        if self.armed
            && self
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.contains(".astock-migration-"))
        {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_detection_requires_database_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_store(dir.path()));
        fs::write(dir.path().join("meta.db"), []).unwrap();
        assert!(has_store(dir.path()));
    }

    #[test]
    fn copy_snapshot_hashes_files_and_excludes_database_control_files() {
        let source = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("timeseries/600519/day")).unwrap();
        fs::write(source.path().join("meta.db"), b"sqlite").unwrap();
        fs::write(source.path().join("meta.db-wal"), b"wal").unwrap();
        fs::write(
            source.path().join("timeseries/600519/day/qfq.parquet"),
            b"parquet-snapshot",
        )
        .unwrap();

        let files = copy_non_database_snapshot(source.path(), stage.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, "timeseries/600519/day/qfq.parquet");
        assert_eq!(files[0].sha256.len(), 64);
        assert!(!stage.path().join("meta.db").exists());
        assert_eq!(
            fs::read(stage.path().join("timeseries/600519/day/qfq.parquet")).unwrap(),
            b"parquet-snapshot"
        );
    }

    #[test]
    fn manifest_verification_detects_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evidence.bin");
        fs::write(&path, b"verified").unwrap();
        let manifest = MigrationManifest {
            schema_version: 1,
            source: PathBuf::from("source"),
            destination: dir.path().to_path_buf(),
            created_at: "test".into(),
            sqlite_integrity: "ok".into(),
            total_bytes: 8,
            files: vec![file_manifest_entry(&path, Path::new("evidence.bin")).unwrap()],
            source_retained: true,
        };
        verify_manifest(dir.path(), &manifest).unwrap();
        fs::write(&path, b"changed!").unwrap();
        assert!(verify_manifest(dir.path(), &manifest).is_err());
    }

    #[test]
    fn active_pointer_is_atomically_replaceable() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        write_active_redirect(root.path(), &first, None).unwrap();
        write_active_redirect(root.path(), &second, None).unwrap();
        assert_eq!(
            read_active_redirect(&root.path().join(REDIRECT_FILE)).unwrap(),
            second
        );
    }

    #[tokio::test]
    async fn full_migration_backs_up_sqlite_activates_pointer_and_keeps_source() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        let bootstrap = root.path().join("bootstrap");
        let storage = Storage::open(StorageConfig::with_base_dir(&source)).unwrap();
        storage.settings_set("proof", "retained").await.unwrap();
        let parquet = source.join("timeseries/600519/day/qfq.parquet");
        fs::create_dir_all(parquet.parent().unwrap()).unwrap();
        fs::write(&parquet, b"immutable parquet fixture").unwrap();

        let outcome = migrate_with_bootstrap(&storage, destination.to_str().unwrap(), &bootstrap)
            .await
            .unwrap();

        assert_eq!(outcome.sqlite_integrity, "ok");
        assert!(outcome.source_retained);
        assert!(source.join("meta.db").is_file());
        assert!(parquet.is_file());
        assert!(destination.join("meta.db").is_file());
        assert!(destination.join(MANIFEST_FILE).is_file());
        assert_eq!(
            read_active_redirect(&bootstrap.join(REDIRECT_FILE)).unwrap(),
            friendly_canonicalize(&destination).unwrap()
        );
        let migrated = Storage::open(StorageConfig::with_base_dir(&destination)).unwrap();
        assert_eq!(
            migrated.settings_get("proof").await.unwrap().as_deref(),
            Some("retained")
        );
        assert_eq!(
            storage.settings_get("data_dir").await.unwrap().as_deref(),
            destination.to_str()
        );
    }

    #[tokio::test]
    async fn rollback_atomically_reactivates_verified_source_and_keeps_both_copies() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        let bootstrap = root.path().join("bootstrap");
        let storage = Storage::open(StorageConfig::with_base_dir(&source)).unwrap();
        storage
            .settings_set("rollback-proof", "source")
            .await
            .unwrap();
        migrate_with_bootstrap(&storage, destination.to_str().unwrap(), &bootstrap)
            .await
            .unwrap();

        let outcome = rollback_with_bootstrap(&storage, &bootstrap).await.unwrap();
        assert_eq!(outcome.source_sqlite_integrity, "ok");
        assert!(outcome.source_retained);
        assert!(outcome.migrated_copy_retained);
        assert!(outcome.restart_required);
        assert_eq!(
            read_active_redirect(&bootstrap.join(REDIRECT_FILE)).unwrap(),
            friendly_canonicalize(&source).unwrap()
        );
        assert!(source.join("meta.db").is_file());
        assert!(destination.join("meta.db").is_file());
        assert!(destination.join(MANIFEST_FILE).is_file());

        let repeated = rollback_with_bootstrap(&storage, &bootstrap).await.unwrap();
        assert_eq!(repeated.data_dir, friendly_canonicalize(&source).unwrap());
        assert_eq!(
            repeated.migrated_copy,
            friendly_canonicalize(&destination).unwrap()
        );
        assert!(source.join("meta.db").is_file());
        assert!(destination.join("meta.db").is_file());
    }
}
