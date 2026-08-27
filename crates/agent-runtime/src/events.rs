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

    /// True once the task will not progress further on its own.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
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
    /// Legacy free-text plan summary, retained so existing adapters keep
    /// working while they migrate to the structured `PlanRevised` event.
    PlanUpdated {
        summary: String,
    },
    /// The structured user-visible plan changed. Carries the mutation that
    /// caused the change plus the resulting plan, so an adapter can either
    /// animate the delta or simply re-render.
    PlanRevised {
        mutation: crate::plan::PlanMutation,
        plan: crate::plan::Plan,
    },
    /// The Agent needs one materially necessary decision from the user.
    ClarificationRequested {
        id: String,
        question: String,
        /// Pre-rendered option rows, including `Let Agent choose` and
        /// `Other...` when offered, so the terminal and the desktop adapter
        /// display the same list.
        options: Vec<String>,
        recommended: Option<String>,
    },
    /// A clarification was answered, by any accepted spelling.
    ClarificationResolved {
        id: String,
        /// Canonical answer kind: option, delegated, other_requested, free_text.
        answer: String,
        /// Recorded reason when the Agent chose after the user delegated.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    ModelStarted {
        model: String,
        round: usize,
    },
    /// The provider returned a turn carrying neither visible text nor a tool call.
    ///
    /// Recorded rather than inferred from a failure, because the recovery is
    /// bounded and a reader needs to see how many replays a task consumed. An empty
    /// turn commits nothing, so `action` distinguishes a safe replay from the point
    /// where the runtime gave up.
    ModelTurnEmpty {
        round: usize,
        attempt: usize,
        /// `replay`, `replay_with_instruction` or `exhausted`.
        action: String,
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
            Self::PlanRevised { .. } => "plan_revised",
            Self::ClarificationRequested { .. } => "clarification_requested",
            Self::ClarificationResolved { .. } => "clarification_resolved",
            Self::ModelStarted { .. } => "model_started",
            Self::ModelTurnEmpty { .. } => "model_turn_empty",
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
