///| A pending tool call. `call_id` is globally unique for one task history.
pub(all) struct PendingTool {
  call_id : String
  tool : String
} derive(Debug, Eq, ToJson, FromJson)

///| Pure, replayable logical Agent state. It contains no sockets, handles,
/// clocks, random generators, clients or database references.
pub(all) struct AgentState {
  task_id : String
  phase : @protocol.AgentPhase
  spec : @protocol.TaskSpec?
  accepted_seq : Int
  model_rounds : Int
  retry_count : Int
  max_model_rounds : Int
  max_retries : Int
  missing_fields : Array[String]
  clarification : @protocol.ClarificationRequest?
  clarification_answers : Array[@protocol.ClarificationAnswer]
  pending_tools : Array[PendingTool]
  completed_call_ids : Array[String]
  evidence_ids : Array[String]
  blocking_verification_errors : Array[String]
  terminal_reason : String?
} derive(Debug, Eq, ToJson, FromJson)

///| Construct the only valid idle state.
pub fn initial(task_id : String) -> AgentState {
  AgentState::{
    task_id,
    phase: Idle,
    spec: None,
    accepted_seq: 0,
    model_rounds: 0,
    retry_count: 0,
    max_model_rounds: 24,
    max_retries: 3,
    missing_fields: [],
    clarification: None,
    clarification_answers: [],
    pending_tools: [],
    completed_call_ids: [],
    evidence_ids: [],
    blocking_verification_errors: [],
    terminal_reason: None,
  }
}

///| States that ordinary events can never leave.
pub fn AgentState::is_terminal(self : AgentState) -> Bool {
  match self.phase {
    Completed | Cancelled | HardFailed => true
    _ => false
  }
}

///| Stable wire token for diagnostics and event-store checkpoints.
pub fn AgentState::phase_name(self : AgentState) -> String {
  match self.phase {
    Idle => "idle"
    Preparing => "preparing"
    WaitingForUser => "waiting_for_user"
    Reasoning => "reasoning"
    AwaitingTools => "awaiting_tools"
    Reviewing => "reviewing"
    Synthesizing => "synthesizing"
    Verifying => "verifying"
    Suspended => "suspended"
    Completed => "completed"
    VerificationFailed => "verification_failed"
    Cancelled => "cancelled"
    HardFailed => "hard_failed"
  }
}

///| Fields required before autonomous research may begin. Business fields
/// never receive silent defaults.
pub fn missing_task_fields(spec : @protocol.TaskSpec) -> Array[String] {
  let missing : Array[String] = []
  if spec.objective.trim().is_empty() { missing.push("objective") }
  if spec.security_universe.is_empty() { missing.push("security_universe") }
  if spec.as_of.trim().is_empty() { missing.push("as_of") }
  if spec.research_start.trim().is_empty() { missing.push("research_start") }
  if spec.research_end.trim().is_empty() { missing.push("research_end") }
  if spec.investment_horizon.trim().is_empty() {
    missing.push("investment_horizon")
  }
  if spec.comparison_benchmark.trim().is_empty() {
    missing.push("comparison_benchmark")
  }
  if spec.output_type.trim().is_empty() { missing.push("output_type") }
  if spec.evidence_requirement.trim().is_empty() {
    missing.push("evidence_requirement")
  }
  missing
}

///| Check a membership relation without exposing mutable state.
fn contains(values : Array[String], needle : String) -> Bool {
  for value in values {
    if value == needle { return true }
  }
  false
}

///| Remove exactly one pending call, returning a fresh array.
fn remove_pending(
  values : Array[PendingTool],
  call_id : String,
) -> Array[PendingTool] {
  let out : Array[PendingTool] = []
  for item in values {
    if item.call_id != call_id { out.push(item) }
  }
  out
}

///| Find a matching pending call.
pub fn AgentState::has_pending(self : AgentState, call_id : String) -> Bool {
  for item in self.pending_tools {
    if item.call_id == call_id { return true }
  }
  false
}

///| Copy with controlled replacement. New arrays passed to this function are
/// never mutated after the state is returned.
fn evolve(
  state : AgentState,
  phase? : @protocol.AgentPhase = state.phase,
  spec? : @protocol.TaskSpec? = state.spec,
  accepted_seq? : Int = state.accepted_seq,
  model_rounds? : Int = state.model_rounds,
  retry_count? : Int = state.retry_count,
  missing_fields? : Array[String] = state.missing_fields,
  clarification? : @protocol.ClarificationRequest? = state.clarification,
  clarification_answers? : Array[@protocol.ClarificationAnswer] = state.clarification_answers,
  pending_tools? : Array[PendingTool] = state.pending_tools,
  completed_call_ids? : Array[String] = state.completed_call_ids,
  evidence_ids? : Array[String] = state.evidence_ids,
  blocking_verification_errors? : Array[String] = state.blocking_verification_errors,
  terminal_reason? : String? = state.terminal_reason,
) -> AgentState {
  AgentState::{
    task_id: state.task_id,
    phase,
    spec,
    accepted_seq,
    model_rounds,
    retry_count,
    max_model_rounds: state.max_model_rounds,
    max_retries: state.max_retries,
    missing_fields,
    clarification,
    clarification_answers,
    pending_tools,
    completed_call_ids,
    evidence_ids,
    blocking_verification_errors,
    terminal_reason,
  }
}

///| Inputs accepted by the pure Agent kernel.
pub(all) enum AgentEvent {
  Start(@protocol.TaskSpec)
  Clarified(@protocol.TaskSpec)
  ClarificationAnswered(
    @protocol.TaskSpec,
    Array[@protocol.ClarificationAnswer],
  )
  ClarificationGenerated(@protocol.ClarificationRequest)
  Prepared
  ModelRoundStarted
  ToolRequested(call_id~ : String, tool~ : String)
  ToolSucceeded(call_id~ : String, evidence_ids~ : Array[String])
  ToolFailed(call_id~ : String, retryable~ : Bool)
  ReviewReady
  SynthesisReady
  VerificationPassed
  VerificationRejected(Array[String])
  QuotaSuspended
  Resume
  Cancel
  HardFailure(String)
} derive(Debug, Eq)

///| Effect descriptions interpreted by the worker shell only after the input
/// event and checkpoint have been durably committed.
pub(all) enum AgentEffect {
  PersistCheckpoint
  AskForClarification(Array[String])
  PrepareResearch
  RequestModel
  ExecuteTool(call_id~ : String, tool~ : String)
  ScheduleRetry(call_id~ : String, attempt~ : Int)
  ReviewEvidence
  SynthesizeReport
  VerifyReport
  PublishReport
  EmitTerminal
} derive(Debug, Eq)

pub(all) struct Transition {
  state : AgentState
  effects : Array[AgentEffect]
  accepted : Bool
  rejection : String?
} derive(Debug, Eq)

fn accepted(state : AgentState, effects : Array[AgentEffect]) -> Transition {
  Transition::{ state, effects, accepted: true, rejection: None }
}

fn rejected(state : AgentState, reason : String) -> Transition {
  Transition::{ state, effects: [], accepted: false, rejection: Some(reason) }
}

///| Pure reducer. The caller must persist the input event before executing
/// returned effects. Duplicate/stale sequences and terminal-state events are
/// rejected without mutation.
pub fn reduce(
  state : AgentState,
  seq : Int,
  event : AgentEvent,
) -> Transition {
  if seq <= state.accepted_seq {
    return rejected(state, "duplicate_or_stale_sequence")
  }
  if state.is_terminal() {
    return rejected(state, "terminal_state_absorbs_event")
  }

  match event {
    Start(spec) | Clarified(spec) => {
      let missing = missing_task_fields(spec)
      if !missing.is_empty() {
        accepted(
          evolve(
            state,
            phase=WaitingForUser,
            spec=Some(spec),
            accepted_seq=seq,
            missing_fields=missing,
            clarification=None,
          ),
          [PersistCheckpoint, AskForClarification(missing)],
        )
      } else {
        accepted(
          evolve(
            state,
            phase=Preparing,
            spec=Some(spec),
            accepted_seq=seq,
            missing_fields=[],
            clarification=None,
          ),
          [PersistCheckpoint, PrepareResearch],
        )
      }
    }
    ClarificationAnswered(spec, answers) => {
      let missing = missing_task_fields(spec)
      if state.phase != WaitingForUser {
        rejected(state, "clarification_answer_requires_waiting")
      } else if !missing.is_empty() {
        rejected(state, "clarification_answer_did_not_complete_task_spec")
      } else {
        accepted(
          evolve(
            state,
            phase=Preparing,
            spec=Some(spec),
            accepted_seq=seq,
            missing_fields=[],
            clarification=None,
            clarification_answers=answers,
          ),
          [PersistCheckpoint, PrepareResearch],
        )
      }
    }
    ClarificationGenerated(request) =>
      if state.phase == WaitingForUser && !request.questions.is_empty() {
        accepted(
          evolve(
            state,
            accepted_seq=seq,
            clarification=Some(request),
          ),
          [PersistCheckpoint],
        )
      } else {
        rejected(state, "clarification_generation_requires_waiting")
      }
    Prepared =>
      if state.phase == Preparing {
        accepted(
          evolve(state, phase=Reasoning, accepted_seq=seq),
          [PersistCheckpoint, RequestModel],
        )
      } else {
        rejected(state, "prepared_requires_preparing")
      }
    ModelRoundStarted =>
      if state.phase == Reasoning {
        let rounds = state.model_rounds + 1
        if rounds > state.max_model_rounds {
          accepted(
            evolve(
              state,
              phase=HardFailed,
              accepted_seq=seq,
              model_rounds=rounds,
              terminal_reason=Some("model_round_limit"),
            ),
            [PersistCheckpoint, EmitTerminal],
          )
        } else {
          accepted(
            evolve(state, accepted_seq=seq, model_rounds=rounds),
            [PersistCheckpoint],
          )
        }
      } else {
        rejected(state, "model_round_requires_reasoning")
      }
    ToolRequested(call_id~, tool~) =>
      if state.phase == Reasoning &&
        !contains(state.completed_call_ids, call_id) &&
        !state.has_pending(call_id) {
        let pending = [..state.pending_tools, PendingTool::{ call_id, tool }]
        accepted(
          evolve(
            state,
            phase=AwaitingTools,
            accepted_seq=seq,
            pending_tools=pending,
          ),
          [PersistCheckpoint, ExecuteTool(call_id~, tool~)],
        )
      } else {
        rejected(state, "tool_request_not_unique_or_wrong_phase")
      }
    ToolSucceeded(call_id~, evidence_ids~) =>
      if state.has_pending(call_id) {
        let pending = remove_pending(state.pending_tools, call_id)
        let completed = [..state.completed_call_ids, call_id]
        let evidence = [..state.evidence_ids, ..evidence_ids]
        let next_phase : @protocol.AgentPhase = if pending.is_empty() {
          Reviewing
        } else {
          AwaitingTools
        }
        let effects = if pending.is_empty() {
          [PersistCheckpoint, ReviewEvidence]
        } else {
          [PersistCheckpoint]
        }
        accepted(
          evolve(
            state,
            phase=next_phase,
            accepted_seq=seq,
            pending_tools=pending,
            completed_call_ids=completed,
            evidence_ids=evidence,
            retry_count=0,
          ),
          effects,
        )
      } else {
        rejected(state, "tool_result_without_pending_call")
      }
    ToolFailed(call_id~, retryable~) =>
      if !state.has_pending(call_id) {
        rejected(state, "tool_failure_without_pending_call")
      } else if retryable && state.retry_count < state.max_retries {
        let attempt = state.retry_count + 1
        accepted(
          evolve(state, accepted_seq=seq, retry_count=attempt),
          [PersistCheckpoint, ScheduleRetry(call_id~, attempt~)],
        )
      } else {
        accepted(
          evolve(
            state,
            phase=HardFailed,
            accepted_seq=seq,
            terminal_reason=Some("tool_failed"),
          ),
          [PersistCheckpoint, EmitTerminal],
        )
      }
    ReviewReady =>
      if state.phase == Reviewing {
        accepted(
          evolve(state, phase=Synthesizing, accepted_seq=seq),
          [PersistCheckpoint, SynthesizeReport],
        )
      } else {
        rejected(state, "review_ready_requires_reviewing")
      }
    SynthesisReady =>
      if state.phase == Synthesizing {
        accepted(
          evolve(state, phase=Verifying, accepted_seq=seq),
          [PersistCheckpoint, VerifyReport],
        )
      } else {
        rejected(state, "synthesis_ready_requires_synthesizing")
      }
    VerificationRejected(errors) =>
      if state.phase == Verifying {
        accepted(
          evolve(
            state,
            phase=VerificationFailed,
            accepted_seq=seq,
            blocking_verification_errors=errors,
          ),
          [PersistCheckpoint],
        )
      } else {
        rejected(state, "verification_requires_verifying")
      }
    VerificationPassed =>
      if state.phase == Verifying &&
        state.blocking_verification_errors.is_empty() {
        accepted(
          evolve(state, phase=Completed, accepted_seq=seq),
          [PersistCheckpoint, PublishReport, EmitTerminal],
        )
      } else {
        rejected(state, "publication_blocked")
      }
    QuotaSuspended => accepted(
      evolve(state, phase=Suspended, accepted_seq=seq),
      [PersistCheckpoint],
    )
    Resume =>
      if state.phase == Suspended {
        accepted(
          evolve(state, phase=Reasoning, accepted_seq=seq),
          [PersistCheckpoint, RequestModel],
        )
      } else {
        rejected(state, "resume_requires_suspended")
      }
    Cancel => accepted(
      evolve(
        state,
        phase=Cancelled,
        accepted_seq=seq,
        pending_tools=[],
        terminal_reason=Some("cancelled_by_user"),
      ),
      [PersistCheckpoint, EmitTerminal],
    )
    HardFailure(reason) => accepted(
      evolve(
        state,
        phase=HardFailed,
        accepted_seq=seq,
        pending_tools=[],
        terminal_reason=Some(reason),
      ),
      [PersistCheckpoint, EmitTerminal],
    )
  }
}
