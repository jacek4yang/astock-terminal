// GENERATED from protocol/schema; schema-sha256=1f27c82e0cb609ecfb01ecff51788842ffad59c078ff41310f107e6faab441b1
// Run: node protocol/codegen.mjs

///|
pub let protocol_version : Int = 1

///|
pub let max_frame_bytes : Int = 8 * 1024 * 1024

///|
pub let max_page_size : Int = 500

///|
pub let release_version : String = "7.0.0"

///|
pub let engine_startup_required_capabilities : Array[String] = ["market","research","data_quality","agent_advanced_analysis_v1","storage","credentials","agent_event_store_v2"]

///|
pub let agent_startup_required_capabilities : Array[String] = ["pure_reducer","replay","evidence_gate","advanced_tool_planning","closed_engine_effects","deterministic_report_verification","sse_stream_recovery"]

///|
pub let agent_request_kinds : Array[String] = ["system.handshake","diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.restore","agent.event","agent.research.workflow","agent.research.workflow.continue","agent.task.snapshot"]

///|
pub let agent_renderer_request_kinds : Array[String] = ["diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.event","agent.research.workflow"]

///|
pub let agent_service_methods : Array[String] = ["task.create","task.list","task.get","task.branch","task.resume","task.cancel","task.answer"]

///|
pub let host_renderer_request_kinds : Array[String] = ["diagnostics.status","window.state","window.minimize","window.toggle_maximize","window.begin_drag","window.system_menu"]

///|
pub(all) enum AgentPhase {
  Idle
  Preparing
  WaitingForUser
  Reasoning
  AwaitingTools
  Reviewing
  Synthesizing
  Verifying
  Suspended
  Completed
  VerificationFailed
  Cancelled
  HardFailed
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend AgentPhase with ToJson::{to_json}

///|
pub extend AgentPhase with FromJson::{from_json}

///|
pub(all) struct TaskSpec {
  objective : String
  security_universe : Array[String]
  as_of : String
  research_start : String
  research_end : String
  investment_horizon : String
  comparison_benchmark : String
  output_type : String
  evidence_requirement : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend TaskSpec with ToJson::{to_json}

///|
pub extend TaskSpec with FromJson::{from_json}

///|
pub(all) struct ClarificationOption {
  id : String
  label : String
  description : String?
  recommended : Bool
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationOption with ToJson::{to_json}

///|
pub extend ClarificationOption with FromJson::{from_json}

///|
pub(all) struct ClarificationQuestion {
  id : String
  header : String?
  question : String
  kind : String
  options : Array[ClarificationOption]
  allow_other : Bool
  target_fields : Array[String]
} derive(Debug, Eq, ToJson, FromJson)

///|
pub type AgentQuestion = ClarificationQuestion

///|
pub extend ClarificationQuestion with ToJson::{to_json}

///|
pub extend ClarificationQuestion with FromJson::{from_json}

///|
pub(all) struct ClarificationRequest {
  title : String
  description : String?
  questions : Array[ClarificationQuestion]
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationRequest with ToJson::{to_json}

///|
pub extend ClarificationRequest with FromJson::{from_json}

///|
pub(all) struct ClarificationAnswer {
  question_id : String
  option_ids : Array[String]
  answer : String?
  decision_mode : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ClarificationAnswer with ToJson::{to_json}

///|
pub extend ClarificationAnswer with FromJson::{from_json}

///|
pub(all) struct ConversationSummary {
  conversation_id : String
  title : String
  phase : AgentPhase
  message_count : Int
  evidence_count : Int
  parent_conversation_id : String?
  branch_from_message_id : String?
  branch_from_checkpoint_task_id : String?
  branch_from_checkpoint_seq : Int?
  created_at : Int64
  updated_at : Int64
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ConversationSummary with ToJson::{to_json}

///|
pub extend ConversationSummary with FromJson::{from_json}

///|
pub(all) struct TaskCheckpoint {
  task_id : String
  phase : AgentPhase
  accepted_seq : Int
  pending_tool_ids : Array[String]
  completed_tool_ids : Array[String]
  evidence_ids : Array[String]
  state_version : String
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend TaskCheckpoint with ToJson::{to_json}

///|
pub extend TaskCheckpoint with FromJson::{from_json}

///|
pub(all) struct ToolActivity {
  call_id : String
  kind : String
  title : String
  detail : String
  status : String
  cache_hit : Bool
  evidence_count : Int
  started_at_ms : Int64?
  finished_at_ms : Int64?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ToolActivity with ToJson::{to_json}

///|
pub extend ToolActivity with FromJson::{from_json}

///|
pub(all) struct EvidenceRef {
  evidence_id : String
  source : String
  source_version_id : String?
  as_of : String?
  fetched_at : String?
  quality_status : String
  original_url : String?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend EvidenceRef with ToJson::{to_json}

///|
pub extend EvidenceRef with FromJson::{from_json}

///|
pub(all) struct VerificationFinding {
  code : String
  severity : String
  message : String
  evidence_ids : Array[String]
  blocking : Bool
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend VerificationFinding with ToJson::{to_json}

///|
pub extend VerificationFinding with FromJson::{from_json}

///|
pub(all) struct ProviderQuota {
  provider : String
  model_name : String
  interval_used : Int64?
  interval_total : Int64?
  interval_remaining_percent : Double?
  interval_reset_at_ms : Int64?
  weekly_used : Int64?
  weekly_total : Int64?
  weekly_remaining_percent : Double?
  weekly_reset_at_ms : Int64?
} derive(Debug, Eq, ToJson, FromJson)

///|
pub extend ProviderQuota with ToJson::{to_json}

///|
pub extend ProviderQuota with FromJson::{from_json}
