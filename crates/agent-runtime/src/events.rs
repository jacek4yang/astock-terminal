use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Preparing,
    Planning,
    Reasoning,
    AwaitingTools,
    Reviewing,
    Verifying,
    Suspended,
    Completed,
    Cancelled,
    Failed,
}

impl AgentPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Planning => "planning",
            Self::Reasoning => "reasoning",
            Self::AwaitingTools => "awaiting_tools",
            Self::Reviewing => "reviewing",
            Self::Verifying => "verifying",
            Self::Suspended => "suspended",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationFinding {
    pub code: String,
    pub message: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted {
        task_id: String,
        objective: String,
    },
    UserMessageAccepted,
    PlanningStarted,
    PlanUpdated {
        summary: String,
    },
    ModelStarted {
        model: String,
        round: usize,
    },
    TextDelta {
        text: String,
    },
    ToolScheduled {
        call_id: String,
        tool: String,
    },
    ToolStarted {
        call_id: String,
        tool: String,
    },
    ToolCompleted {
        call_id: String,
        tool: String,
        evidence_ids: Vec<String>,
    },
    ToolFailed {
        call_id: String,
        tool: String,
        message: String,
        retryable: bool,
    },
    EvidenceAdded {
        call_id: String,
        evidence_ids: Vec<String>,
    },
    VerificationStarted,
    VerificationFinding {
        finding: VerificationFinding,
    },
    Suspended {
        reason: String,
    },
    Cancelled,
    Completed {
        report: String,
        evidence_ids: Vec<String>,
    },
    Failed {
        message: String,
    },
}

impl AgentEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session_started",
            Self::UserMessageAccepted => "user_message_accepted",
            Self::PlanningStarted => "planning_started",
            Self::PlanUpdated { .. } => "plan_updated",
            Self::ModelStarted { .. } => "model_started",
            Self::TextDelta { .. } => "text_delta",
            Self::ToolScheduled { .. } => "tool_scheduled",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolCompleted { .. } => "tool_completed",
            Self::ToolFailed { .. } => "tool_failed",
            Self::EvidenceAdded { .. } => "evidence_added",
            Self::VerificationStarted => "verification_started",
            Self::VerificationFinding { .. } => "verification_finding",
            Self::Suspended { .. } => "suspended",
            Self::Cancelled => "cancelled",
            Self::Completed { .. } => "completed",
            Self::Failed { .. } => "failed",
        }
    }
}
