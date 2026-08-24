// GENERATED from protocol/schema; schema-sha256=c6f719cde75d63a1b8f66c69e9537a8025e4b7a79e1e743f86683dc775e84cf4
// Run: node protocol/codegen.mjs

///|
pub let protocol_version : Int = 1

///|
pub let max_frame_bytes : Int = 8 * 1024 * 1024

///|
pub let max_page_size : Int = 500

///|
pub let agent_request_kinds : Array[String] = ["system.handshake","diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.restore","agent.event","agent.research.workflow","agent.research.workflow.continue","agent.task.snapshot"]

///|
pub let agent_renderer_request_kinds : Array[String] = ["diagnostics.status","agent.provider.test","agent.provider.configure","agent.start","agent.restore","agent.event","agent.research.workflow","agent.task.snapshot"]

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
