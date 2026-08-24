use astock_storage::{Storage, StorageConfig};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DataRootOrigin {
    ExplicitEnvironment,
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

pub async fn resolve_and_open() -> Result<(Storage, DataRootDecision), String> {
    if let Some(explicit) = non_empty_env("ASTOCK_DATA_DIR") {
        return open_decision(explicit, DataRootOrigin::ExplicitEnvironment, None);
    }

    let legacy = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("astock-terminal"));
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

    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return open_decision(
            PathBuf::from(local).join("com.astock.terminal"),
            DataRootOrigin::ProtonDefault,
            legacy,
        );
    }

    open_decision(
        std::env::temp_dir().join("com.astock.terminal"),
        DataRootOrigin::TemporaryFallback,
        legacy,
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_detection_requires_database_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_store(dir.path()));
        std::fs::write(dir.path().join("meta.db"), []).unwrap();
        assert!(has_store(dir.path()));
    }
}
