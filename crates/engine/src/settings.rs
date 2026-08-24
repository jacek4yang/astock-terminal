//! Non-secret Agent settings persisted in the research database.

use astock_storage::Storage;
use serde::{Deserialize, Serialize};

use super::{invalid, storage, ServiceError};

const AGENT_MODEL_ROUTING_SETTING: &str = "agent.model_routing.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct AgentModelRouting {
    pub coordinator_model: String,
    pub fast_model: String,
    pub deep_model: String,
    pub verifier_model: String,
    pub multi_agent_enabled: bool,
    pub max_parallel_agents: u8,
}

impl Default for AgentModelRouting {
    fn default() -> Self {
        Self {
            coordinator_model: "auto".into(),
            fast_model: "auto".into(),
            deep_model: "auto".into(),
            verifier_model: "auto".into(),
            multi_agent_enabled: true,
            max_parallel_agents: 3,
        }
    }
}

impl AgentModelRouting {
    fn normalize(mut self) -> Result<Self, ServiceError> {
        for model in [
            &mut self.coordinator_model,
            &mut self.fast_model,
            &mut self.deep_model,
            &mut self.verifier_model,
        ] {
            *model = model.trim().to_string();
            if model.is_empty() {
                *model = "auto".into();
            }
            if model.len() > 128 || model.chars().any(char::is_control) {
                return Err(invalid("模型 ID 过长或包含控制字符"));
            }
        }
        if !(1..=4).contains(&self.max_parallel_agents) {
            return Err(invalid("并行复核数量必须在 1 到 4 之间"));
        }
        Ok(self)
    }
}

pub(super) async fn get(storage_ref: &Storage) -> AgentModelRouting {
    storage_ref
        .settings_get(AGENT_MODEL_ROUTING_SETTING)
        .await
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<AgentModelRouting>(&value).ok())
        .and_then(|value| value.normalize().ok())
        .unwrap_or_default()
}

pub(super) async fn set(
    storage_ref: &Storage,
    settings: AgentModelRouting,
) -> Result<AgentModelRouting, ServiceError> {
    let settings = settings.normalize()?;
    let encoded = serde_json::to_string(&settings).map_err(invalid)?;
    storage_ref
        .settings_set(AGENT_MODEL_ROUTING_SETTING, &encoded)
        .await
        .map_err(storage)?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routing_is_normalized_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let storage_ref =
            Storage::open(astock_storage::StorageConfig::with_base_dir(dir.path())).unwrap();
        let saved = set(
            &storage_ref,
            AgentModelRouting {
                deep_model: " MiniMax-M3 ".into(),
                max_parallel_agents: 4,
                ..AgentModelRouting::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(saved.deep_model, "MiniMax-M3");
        assert_eq!(get(&storage_ref).await, saved);
    }

    #[test]
    fn routing_rejects_control_characters_and_unbounded_parallelism() {
        assert!(AgentModelRouting {
            deep_model: "bad\nmodel".into(),
            ..AgentModelRouting::default()
        }
        .normalize()
        .is_err());
        assert!(AgentModelRouting {
            max_parallel_agents: 5,
            ..AgentModelRouting::default()
        }
        .normalize()
        .is_err());
    }
}
