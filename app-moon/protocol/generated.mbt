// GENERATED from protocol/schema; schema-sha256=6328583d2712698d4258c6d78bc971b666436f297f26e4f5cadfcc8614db8c52
// Run: node protocol/codegen.mjs

///|
pub let protocol_version : Int = 1

///|
pub let max_frame_bytes : Int = 8 * 1024 * 1024

///|
pub let max_page_size : Int = 500

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
