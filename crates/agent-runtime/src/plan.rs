//! User-visible dynamic research plan.
//!
//! The plan is an execution artifact, not private chain-of-thought. It exists
//! so the user can see what the Agent is doing and why the shape of the work
//! changed. Both the terminal and the desktop adapter consume the same plan
//! state, so neither can invent its own progress model.
//!
//! The Agent may add, remove, reorder, split, retry, block or degrade steps as
//! evidence arrives. Those transitions are recorded as [`PlanMutation`] values
//! rather than applied silently, which is what makes the plan explainable and
//! what allows enough state to be persisted for recovery after an interruption.

use serde::{Deserialize, Serialize};

/// Lifecycle of a single plan step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    /// Not started.
    Pending,
    /// Currently executing.
    Active,
    /// Finished successfully.
    Done,
    /// Deliberately dropped because it became irrelevant.
    Removed,
    /// Cannot proceed; carries a reason.
    Blocked,
    /// Completed with reduced coverage, for example a partially unavailable
    /// source. Never silently upgraded to `Done`, because degraded coverage
    /// must stay visible in the final report.
    Degraded,
}

impl PlanStepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Done => "done",
            Self::Removed => "removed",
            Self::Blocked => "blocked",
            Self::Degraded => "degraded",
        }
    }

    /// Marker used in the compact terminal rendering.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Pending => "○",
            Self::Active => "◐",
            Self::Done => "✓",
            Self::Removed => "─",
            Self::Blocked => "✗",
            Self::Degraded => "!",
        }
    }

    /// True once the step will not run again.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Removed | Self::Degraded)
    }
}

/// A single visible unit of research work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub title: String,
    pub status: PlanStepStatus,
    /// Why the step is blocked or degraded. Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// How many times the step has been retried.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl PlanStep {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status: PlanStepStatus::Pending,
            note: None,
            attempts: 0,
        }
    }
}

/// An explicit, explainable change to the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanMutation {
    Add {
        step: PlanStep,
        /// Why the Agent decided more work was needed.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Start {
        id: String,
    },
    Complete {
        id: String,
    },
    /// Finished, but with reduced coverage that must remain visible.
    Degrade {
        id: String,
        reason: String,
    },
    Block {
        id: String,
        reason: String,
    },
    /// Drop a step that became irrelevant.
    Remove {
        id: String,
        reason: String,
    },
    /// Run a step again after a recoverable failure.
    Retry {
        id: String,
        reason: String,
    },
    /// Replace one step with finer-grained children.
    Split {
        id: String,
        into: Vec<PlanStep>,
        reason: String,
    },
    /// Reorder the remaining steps.
    Reorder {
        order: Vec<String>,
    },
}

/// The current plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<PlanStep>,
    /// Applied mutations, oldest first. Enough to explain how the plan reached
    /// its current shape and to rebuild it after an interruption.
    #[serde(default)]
    pub history: Vec<PlanMutation>,
}

impl Plan {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build an initial plan from ordered titles.
    pub fn from_titles<I, T>(titles: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<String>,
    {
        let mut plan = Self::new();
        for (index, title) in titles.into_iter().enumerate() {
            plan.steps
                .push(PlanStep::new(format!("step-{}", index + 1), title.into()));
        }
        plan
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.steps.iter().position(|step| step.id == id)
    }

    /// Apply a mutation, recording it for explainability.
    ///
    /// Returns an error rather than panicking or silently ignoring an unknown
    /// step, because a plan that drifts from the work actually performed is
    /// worse than no plan.
    pub fn apply(&mut self, mutation: PlanMutation) -> Result<(), String> {
        match &mutation {
            PlanMutation::Add { step, .. } => {
                if self.position(&step.id).is_some() {
                    return Err(format!("duplicate plan step id `{}`", step.id));
                }
                self.steps.push(step.clone());
            }
            PlanMutation::Start { id } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Active;
            }
            PlanMutation::Complete { id } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Done;
                self.steps[index].note = None;
            }
            PlanMutation::Degrade { id, reason } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Degraded;
                self.steps[index].note = Some(reason.clone());
            }
            PlanMutation::Block { id, reason } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Blocked;
                self.steps[index].note = Some(reason.clone());
            }
            PlanMutation::Remove { id, reason } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Removed;
                self.steps[index].note = Some(reason.clone());
            }
            PlanMutation::Retry { id, reason } => {
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Active;
                self.steps[index].attempts += 1;
                self.steps[index].note = Some(reason.clone());
            }
            PlanMutation::Split { id, into, reason } => {
                if into.is_empty() {
                    return Err("a split must produce at least one step".into());
                }
                let index = self.require(id)?;
                self.steps[index].status = PlanStepStatus::Removed;
                self.steps[index].note = Some(reason.clone());
                for (offset, child) in into.iter().enumerate() {
                    if self.position(&child.id).is_some() {
                        return Err(format!("duplicate plan step id `{}`", child.id));
                    }
                    self.steps.insert(index + 1 + offset, child.clone());
                }
            }
            PlanMutation::Reorder { order } => {
                // Every named step must exist, and naming a subset is allowed:
                // unnamed steps keep their relative order at the end.
                for id in order {
                    self.require(id)?;
                }
                let mut reordered: Vec<PlanStep> = Vec::with_capacity(self.steps.len());
                for id in order {
                    if let Some(index) = self.position(id) {
                        reordered.push(self.steps[index].clone());
                    }
                }
                for step in &self.steps {
                    if !order.contains(&step.id) {
                        reordered.push(step.clone());
                    }
                }
                self.steps = reordered;
            }
        }
        self.history.push(mutation);
        Ok(())
    }

    fn require(&self, id: &str) -> Result<usize, String> {
        self.position(id)
            .ok_or_else(|| format!("unknown plan step `{id}`"))
    }

    /// Steps a user should see. Removed steps are hidden from the live view but
    /// remain in `history` so the change stays auditable.
    pub fn visible(&self) -> impl Iterator<Item = &PlanStep> {
        self.steps
            .iter()
            .filter(|step| step.status != PlanStepStatus::Removed)
    }

    pub fn is_empty(&self) -> bool {
        self.visible().next().is_none()
    }

    /// True when no visible step can still make progress.
    pub fn is_settled(&self) -> bool {
        self.visible()
            .all(|step| step.status.is_terminal() || step.status == PlanStepStatus::Blocked)
    }

    /// Compact terminal rendering.
    pub fn render_plain(&self) -> String {
        if self.is_empty() {
            return "no active research plan".to_string();
        }
        let mut lines = vec!["Plan".to_string()];
        for step in self.visible() {
            let mut line = format!("{} {}", step.status.marker(), step.title);
            if step.attempts > 0 {
                line.push_str(&format!(" (attempt {})", step.attempts + 1));
            }
            lines.push(line);
            if let Some(note) = &step.note {
                lines.push(format!("    {note}"));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Plan {
        Plan::from_titles([
            "Resolve 紫金矿业 / 601899",
            "Obtain current quote and valuation",
            "Review latest financial disclosures",
        ])
    }

    #[test]
    fn a_plan_progresses_through_visible_states() {
        let mut plan = sample();
        plan.apply(PlanMutation::Start {
            id: "step-1".into(),
        })
        .unwrap();
        assert_eq!(plan.steps[0].status, PlanStepStatus::Active);
        plan.apply(PlanMutation::Complete {
            id: "step-1".into(),
        })
        .unwrap();
        assert_eq!(plan.steps[0].status, PlanStepStatus::Done);
        assert!(plan.render_plain().contains("✓ Resolve 紫金矿业 / 601899"));
    }

    #[test]
    fn the_agent_can_add_work_when_evidence_demands_it() {
        let mut plan = sample();
        plan.apply(PlanMutation::Add {
            step: PlanStep::new("copper", "Quantify copper-price sensitivity"),
            reason: Some("copper price exposure is material".into()),
        })
        .unwrap();
        assert_eq!(plan.visible().count(), 4);
        assert!(plan
            .render_plain()
            .contains("○ Quantify copper-price sensitivity"));
    }

    #[test]
    fn degraded_coverage_is_never_reported_as_success() {
        let mut plan = sample();
        plan.apply(PlanMutation::Degrade {
            id: "step-3".into(),
            reason: "two news sources unavailable, event coverage degraded".into(),
        })
        .unwrap();
        let step = &plan.steps[2];
        assert_eq!(step.status, PlanStepStatus::Degraded);
        assert_ne!(step.status, PlanStepStatus::Done);
        assert!(plan.render_plain().contains("event coverage degraded"));
    }

    #[test]
    fn blocked_steps_keep_their_reason_and_do_not_settle_as_done() {
        let mut plan = sample();
        plan.apply(PlanMutation::Block {
            id: "step-2".into(),
            reason: "quote provider returned 429".into(),
        })
        .unwrap();
        assert_eq!(plan.steps[1].status, PlanStepStatus::Blocked);
        assert!(!plan.steps[1].status.is_terminal());
    }

    #[test]
    fn retry_increments_attempts_rather_than_hiding_the_failure() {
        let mut plan = sample();
        plan.apply(PlanMutation::Retry {
            id: "step-2".into(),
            reason: "transient timeout".into(),
        })
        .unwrap();
        assert_eq!(plan.steps[1].attempts, 1);
        assert!(plan.render_plain().contains("(attempt 2)"));
    }

    #[test]
    fn split_replaces_a_step_with_children_in_place() {
        let mut plan = sample();
        plan.apply(PlanMutation::Split {
            id: "step-3".into(),
            into: vec![
                PlanStep::new("step-3a", "Read the latest annual report"),
                PlanStep::new("step-3b", "Read the latest quarterly report"),
            ],
            reason: "the disclosure review is two distinct documents".into(),
        })
        .unwrap();
        let titles: Vec<&str> = plan.visible().map(|step| step.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Resolve 紫金矿业 / 601899",
                "Obtain current quote and valuation",
                "Read the latest annual report",
                "Read the latest quarterly report",
            ]
        );
    }

    #[test]
    fn removed_steps_disappear_from_view_but_stay_in_history() {
        let mut plan = sample();
        plan.apply(PlanMutation::Remove {
            id: "step-2".into(),
            reason: "valuation already answered by the previous turn".into(),
        })
        .unwrap();
        assert_eq!(plan.visible().count(), 2);
        assert!(matches!(
            plan.history.last(),
            Some(PlanMutation::Remove { .. })
        ));
    }

    #[test]
    fn reorder_moves_named_steps_first_and_keeps_the_rest_stable() {
        let mut plan = sample();
        plan.apply(PlanMutation::Reorder {
            order: vec!["step-3".into(), "step-1".into()],
        })
        .unwrap();
        let ids: Vec<&str> = plan.steps.iter().map(|step| step.id.as_str()).collect();
        assert_eq!(ids, vec!["step-3", "step-1", "step-2"]);
    }

    #[test]
    fn mutating_an_unknown_step_is_an_error_not_a_silent_no_op() {
        let mut plan = sample();
        assert!(plan
            .apply(PlanMutation::Start { id: "nope".into() })
            .is_err());
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let mut plan = sample();
        assert!(plan
            .apply(PlanMutation::Add {
                step: PlanStep::new("step-1", "duplicate"),
                reason: None,
            })
            .is_err());
    }

    #[test]
    fn a_plan_survives_a_serialization_round_trip_for_recovery() {
        let mut plan = sample();
        plan.apply(PlanMutation::Start {
            id: "step-1".into(),
        })
        .unwrap();
        plan.apply(PlanMutation::Add {
            step: PlanStep::new("copper", "Quantify copper sensitivity"),
            reason: Some("material exposure".into()),
        })
        .unwrap();
        let encoded = serde_json::to_string(&plan).expect("plan serializes");
        let restored: Plan = serde_json::from_str(&encoded).expect("plan deserializes");
        assert_eq!(restored, plan);
        assert_eq!(restored.history.len(), 2);
    }
}
