///|
fn complete_spec() -> @protocol.TaskSpec {
  @protocol.TaskSpec::{
    objective: "Evaluate earnings durability",
    security_universe: ["600519"],
    as_of: "2026-08-24T12:00:00+08:00",
    research_start: "2024-01-01",
    research_end: "2026-08-24",
    investment_horizon: "12 months",
    comparison_benchmark: "000300",
    output_type: "research_report",
    evidence_requirement: "strict",
  }
}

///|
test "missing business information pauses before research" {
  let incomplete = @protocol.TaskSpec::{
    objective: "",
    security_universe: [],
    as_of: "",
    research_start: "",
    research_end: "",
    investment_horizon: "",
    comparison_benchmark: "",
    output_type: "",
    evidence_requirement: "",
  }
  let transition = reduce(initial("task-1"), 1, Start(incomplete))
  assert_eq(transition.state.phase, WaitingForUser)
  assert_true(transition.state.missing_fields.length() == 9)
}

///|
test "model clarification is accepted only while waiting for the user" {
  let incomplete = @protocol.TaskSpec::{
    objective: "",
    security_universe: ["600519"],
    as_of: "",
    research_start: "",
    research_end: "",
    investment_horizon: "",
    comparison_benchmark: "",
    output_type: "",
    evidence_requirement: "",
  }
  let waiting = reduce(initial("task-clarify"), 1, Start(incomplete)).state
  let question = @protocol.ClarificationQuestion::{
    id: "scope",
    header: Some("研究范围"),
    question: "请确认希望研究的时间范围和输出形式。",
    kind: "single_choice",
    options: [
      @protocol.ClarificationOption::{
        id: "medium_term",
        label: "中期研究",
        description: Some("覆盖一年历史并给出研究报告"),
        recommended: true,
      },
    ],
    allow_other: true,
    target_fields: ["research_start", "research_end", "output_type"],
  }
  let request = @protocol.ClarificationRequest::{
    title: "补充研究边界",
    description: Some("问题由模型针对缺失信息动态生成"),
    questions: [question],
  }
  let generated = reduce(waiting, 2, ClarificationGenerated(request))
  assert_true(generated.accepted)
  assert_true(generated.state.clarification is Some(_))

  let rejected = reduce(
    initial("task-not-waiting"),
    1,
    ClarificationGenerated(request),
  )
  assert_false(rejected.accepted)
}

///|
test "complete clarification answer resumes autonomous preparation" {
  let incomplete = @protocol.TaskSpec::{
    objective: "研究贵州茅台盈利韧性",
    security_universe: ["600519"],
    as_of: "",
    research_start: "",
    research_end: "",
    investment_horizon: "",
    comparison_benchmark: "",
    output_type: "",
    evidence_requirement: "",
  }
  let waiting = reduce(initial("task-answer"), 1, Start(incomplete)).state
  let answers = [
    @protocol.ClarificationAnswer::{
      question_id: "scope",
      option_ids: ["agent_best"],
      answer: None,
      decision_mode: "agent_best_with_evidence",
    },
  ]
  let resumed = reduce(
    waiting,
    2,
    ClarificationAnswered(complete_spec(), answers),
  )
  assert_true(resumed.accepted)
  assert_eq(resumed.state.phase, Preparing)
  assert_eq(resumed.state.clarification_answers, answers)
  assert_true(resumed.state.missing_fields.is_empty())
}

///|
test "duplicate sequence cannot mutate state" {
  let first = reduce(initial("task-1"), 1, Start(complete_spec()))
  let duplicate = reduce(first.state, 1, Cancel)
  assert_false(duplicate.accepted)
  assert_eq(duplicate.state.phase, Preparing)
  assert_eq(duplicate.state.accepted_seq, 1)
}

///|
test "tool result requires matching pending call and completes once" {
  let started = reduce(initial("task-1"), 1, Start(complete_spec())).state
  let reasoning = reduce(started, 2, Prepared).state
  let pending = reduce(
      reasoning,
      3,
      ToolRequested(call_id="call-1", tool="market.quote"),
    ).state
  let wrong = reduce(
    pending,
    4,
    ToolSucceeded(call_id="call-2", evidence_ids=[]),
  )
  assert_false(wrong.accepted)
  let done = reduce(
      pending,
      4,
      ToolSucceeded(call_id="call-1", evidence_ids=["ev-1"]),
    ).state
  assert_eq(done.phase, Reviewing)
  let duplicate = reduce(
    done,
    5,
    ToolSucceeded(call_id="call-1", evidence_ids=["ev-2"]),
  )
  assert_false(duplicate.accepted)
}

///|
test "acknowledged cancellation is absorbing" {
  let started = reduce(initial("task-1"), 1, Start(complete_spec())).state
  let cancelled = reduce(started, 2, Cancel).state
  let later = reduce(cancelled, 3, Prepared)
  assert_eq(later.state.phase, Cancelled)
  assert_true(later.effects.is_empty())
}

///|
test "report publication requires successful verification" {
  let started = reduce(initial("task-1"), 1, Start(complete_spec())).state
  let reasoning = reduce(started, 2, Prepared).state
  let pending = reduce(
      reasoning,
      3,
      ToolRequested(call_id="call-1", tool="market.quote"),
    ).state
  let reviewing = reduce(
      pending,
      4,
      ToolSucceeded(call_id="call-1", evidence_ids=["ev-1"]),
    ).state
  let synthesizing = reduce(reviewing, 5, ReviewReady).state
  let verifying = reduce(synthesizing, 6, SynthesisReady).state
  let failed = reduce(
      verifying,
      7,
      VerificationRejected(["claim_without_evidence"]),
    ).state
  assert_eq(failed.phase, VerificationFailed)
  let publish = reduce(failed, 8, VerificationPassed)
  assert_false(publish.accepted)
}

///|
test "review may request another bounded tool wave" {
  let preparing = reduce(
      initial("task-more-evidence"),
      1,
      Start(complete_spec()),
    ).state
  let reasoning = reduce(preparing, 2, Prepared).state
  let awaiting = reduce(
      reasoning,
      3,
      ToolRequested(call_id="prepare", tool="research.agent_prepare_context"),
    ).state
  let reviewing = reduce(
      awaiting,
      4,
      ToolSucceeded(call_id="prepare", evidence_ids=["prepare"]),
    ).state
  let next_reasoning = reduce(reviewing, 5, MoreEvidenceRequired)
  assert_true(next_reasoning.accepted)
  assert_eq(next_reasoning.state.phase, Reasoning)
  let completed = reduce(next_reasoning.state, 6, EvidenceReviewCompleted)
  assert_true(completed.accepted)
  assert_eq(completed.state.phase, Reviewing)
}

///|
test "interrupted tools reconcile without pretending they completed" {
  let preparing = reduce(initial("task-reconcile"), 1, Start(complete_spec())).state
  let state = reduce(preparing, 2, Prepared).state
  let requested = reduce(
    state,
    state.accepted_seq + 1,
    ToolRequested(
      call_id="call-interrupted",
      tool="research.agent_prepare_context",
    ),
  )
  inspect(requested.accepted, content="true")
  let reconciled = reduce(
    requested.state,
    requested.state.accepted_seq + 1,
    ReconcileInterruptedWorkflow,
  )
  inspect(reconciled.accepted, content="true")
  inspect(reconciled.state.phase_name(), content="reasoning")
  inspect(reconciled.state.pending_tools.length(), content="0")
  inspect(reconciled.state.completed_call_ids.length(), content="0")
  assert_eq(reconciled.state.reconciled_call_ids, ["call-interrupted"])
}

///|
test "provider suspension preserves a research plan and resumes safely" {
  let preparing = reduce(
      initial("task-provider-resume"),
      1,
      Start(complete_spec()),
    ).state
  let state = reduce(preparing, 2, Prepared).state
  let selected = reduce(
    state,
    state.accepted_seq + 1,
    ResearchPlanSelected(["300308", "000001"]),
  )
  inspect(selected.accepted, content="true")
  let suspended = reduce(
    selected.state,
    selected.state.accepted_seq + 1,
    ProviderSuspended("provider_unavailable"),
  )
  inspect(suspended.state.phase_name(), content="suspended")
  assert_eq(suspended.state.research_symbols, ["300308", "000001"])
  let resumed = reduce(
    suspended.state,
    suspended.state.accepted_seq + 1,
    Resume,
  )
  inspect(resumed.accepted, content="true")
  inspect(resumed.state.phase_name(), content="reasoning")
  assert_true(resumed.state.terminal_reason is None)
}
