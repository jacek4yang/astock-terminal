///| Single coarse-grained, versioned Host route. Domain operations remain
/// coarse-grained inside Engine and Agent; the Host does not mirror hundreds
/// of legacy Tauri commands.
pub(all) struct AppRequest {
  target : String
  request : Json
} derive(ToJson, FromJson)

///|
pub extend AppRequest with ToJson::{to_json}

///|
pub extend AppRequest with FromJson::{from_json}

///|
pub let app_request : @proton_contract.Command[AppRequest, Json] = @proton_contract.command(
  "request",
)

///| Batched Worker event stream; renderer coalesces these to at most 10 Hz.
pub let worker_event : @proton_contract.Event[Json] = @proton_contract.event(
  "worker_event",
)
