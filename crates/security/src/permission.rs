use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Capability domain of an Agent tool. The classification is about side
/// effects, not implementation detail: a calculation may still read cached
/// inputs but cannot mutate external systems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionDomain {
    ReadOnlyNetwork,
    ReadOnlyLocal,
    Compute,
    WriteExternal,
}

impl std::fmt::Display for ToolPermissionDomain {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::ReadOnlyNetwork => "read_only_network",
            Self::ReadOnlyLocal => "read_only_local",
            Self::Compute => "compute",
            Self::WriteExternal => "write_external",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationOrigin {
    UserRequest,
    ModelPlan,
    ExternalContent,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolAuthorizationError {
    #[error("工具未在用户锁定的本轮权限清单中")]
    NotEnabled,
    #[error("外部网页、PDF 或新闻内容无权读取本地数据")]
    ExternalCannotReadLocal,
    #[error("外部内容和模型计划都不能直接授权外部写操作")]
    ExplicitUserConfirmationRequired,
}

/// Enforce the immutable user allowlist and prevent untrusted external text
/// from granting itself local-read or external-write capabilities.
pub fn authorize_tool(
    domain: ToolPermissionDomain,
    origin: InvocationOrigin,
    enabled_by_user: bool,
) -> Result<(), ToolAuthorizationError> {
    if !enabled_by_user {
        return Err(ToolAuthorizationError::NotEnabled);
    }
    match (origin, domain) {
        (_, ToolPermissionDomain::WriteExternal) if origin != InvocationOrigin::UserRequest => {
            Err(ToolAuthorizationError::ExplicitUserConfirmationRequired)
        }
        (InvocationOrigin::ExternalContent, ToolPermissionDomain::ReadOnlyLocal) => {
            Err(ToolAuthorizationError::ExternalCannotReadLocal)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_content_cannot_expand_local_or_write_permissions() {
        assert!(authorize_tool(
            ToolPermissionDomain::ReadOnlyLocal,
            InvocationOrigin::ExternalContent,
            true
        )
        .is_err());
        assert!(authorize_tool(
            ToolPermissionDomain::WriteExternal,
            InvocationOrigin::ModelPlan,
            true
        )
        .is_err());
        assert!(authorize_tool(
            ToolPermissionDomain::Compute,
            InvocationOrigin::ExternalContent,
            true
        )
        .is_ok());
    }

    #[test]
    fn disabled_tool_is_always_denied() {
        assert_eq!(
            authorize_tool(
                ToolPermissionDomain::ReadOnlyNetwork,
                InvocationOrigin::UserRequest,
                false
            ),
            Err(ToolAuthorizationError::NotEnabled)
        );
    }
}
