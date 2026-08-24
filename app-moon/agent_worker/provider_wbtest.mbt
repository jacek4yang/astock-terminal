///|
test "structured response discards MiniMax think tags" {
  let value = model_json_content(
    "<think>private reasoning with {braces} that must not reach the UI</think>\n```json\n{\"answer\":\"ok\"}\n```",
  )
  assert_eq(value.stringify(), "{\"answer\":\"ok\"}")
}

///|
test "plain response publishes only final visible text" {
  let visible = visible_model_text(
    "<think>private reasoning that must not reach the UI</think>\n研究结论：证据仍不完整，应继续人工复核。",
  )
  assert_eq(
    visible, "研究结论：证据仍不完整，应继续人工复核。",
  )
}

///|
test "SSE decoder preserves split UTF-8 and ignores private reasoning deltas" {
  let wire = "data: {\"choices\":[{\"delta\":{\"reasoning_details\":\"private\",\"content\":\"研究\"}}]}\n\n" +
    "data: {\"choices\":[{\"delta\":{\"content\":\"结论\"}}]}\r\n\r\n" +
    "data: [DONE]\n\n"
  let bytes = @utf8.encode(wire)
  let chunks = [
    bytes[:7].to_owned(),
    bytes[7:43].to_owned(),
    bytes[43:47].to_owned(),
    bytes[47:bytes.length() - 5].to_owned(),
    bytes[bytes.length() - 5:].to_owned(),
  ]
  assert_eq(decode_sse_chunks(chunks), "研究结论")
}

///|
test "SSE decoder rejects a stream without DONE" {
  let result : String? = Some(
    decode_sse_chunks([
      @utf8.encode(
        "data: {\"choices\":[{\"delta\":{\"content\":\"残缺\"}}]}\n\n",
      ),
    ]),
  ) catch {
    _ => None
  }
  assert_true(result is None)
}

///|
test "SSE decoder ignores comments and metadata" {
  let text = decode_sse_chunks([
    @utf8.encode(
      ": keep-alive\nevent: message\nid: 7\ndata: {\"choices\":[{\"delta\":{\"content\":\"可见\"}}]}\n\ndata: [DONE]\n",
    ),
  ])
  assert_eq(text, "可见")
}

///|
async test "stream recovery discards a failed partial attempt and restarts" {
  let mut calls = 0
  let result = retry_complete_text(["first", "second", "third"], 1000, prompt => {
    calls += 1
    if calls == 1 {
      return decode_sse_chunks([
        @utf8.encode(
          "data: {\"choices\":[{\"delta\":{\"content\":\"partial-must-be-discarded\"}}]}\n\n",
        ),
      ])
    }
    decode_sse_chunks([
      @utf8.encode(
        "data: {\"choices\":[{\"delta\":{\"content\":\"complete:" +
        prompt +
        "\"}}]}\n\ndata: [DONE]\n\n",
      ),
    ])
  })
  assert_eq(calls, 2)
  assert_eq(result, "complete:second")
  assert_false(result.contains("partial"))
}

///|
test "model routing selects only catalog models" {
  let provider = ProviderConfig::{
    api_host: "https://api.example.invalid",
    model: "MiniMax-M3",
    fast_model: "MiniMax-M2.7-highspeed",
    deep_model: "MiniMax-M3",
    verifier_model: "MiniMax-M3",
    available_models: ["MiniMax-M3", "MiniMax-M2.7-highspeed"],
    multi_agent_enabled: true,
    max_parallel_agents: 3,
  }
  let routed = provider.with_routing(ModelRouting::{
    coordinator_model: "MiniMax-M3",
    fast_model: "MiniMax-M2.7-highspeed",
    deep_model: "MiniMax-M3",
    verifier_model: "MiniMax-M3",
    multi_agent_enabled: false,
    max_parallel_agents: 2,
  })
  assert_eq(routed.model, "MiniMax-M3")
  assert_eq(routed.fast_model, "MiniMax-M2.7-highspeed")
  assert_eq(routed.max_parallel_agents, 2)
  assert_false(routed.multi_agent_enabled)
}

///|
test "publication verifier rejects thin drafts and accepts complete manual plans" {
  let task = @protocol.TaskSpec::{
    objective: "两万元投资计划",
    security_universe: [],
    as_of: "2026-08-24T12:00:00+08:00",
    research_start: "2025-08-24",
    research_end: "2026-08-24",
    investment_horizon: "三个月",
    comparison_benchmark: "000300",
    output_type: "manual_plan",
    evidence_requirement: "strict",
  }
  let thin = verify_research_report("立即买入并保证收益", task, {})
  assert_true(thin.contains("report_too_short_for_evidence_review"))
  assert_true(thin.contains("manual_execution_boundary_missing"))
  let complete_section = "执行摘要：只形成供人工复核的研究计划，不自动下单。\n" +
    "市场环境：行情仍有不确定性。\n候选逐项复核：仅依据输入证据。\n" +
    "资金与仓位方案：资金两万元，至少保留现金；普通股票按一手100股检查。\n" +
    "触发/失效条件：触发后仍需人工复核，失效时保持现金。\n" +
    "反方证据与不确定性：单一来源不能形成结论。\n主要风险：价格和事件风险。\n" +
    "人工复核清单：刷新报价、公告和费用。\n数据来源与时点：以Engine retrieved_at为准。\n" +
    "未覆盖能力：不连接券商。\n"
  let complete = complete_section +
    complete_section +
    complete_section +
    complete_section +
    complete_section +
    complete_section
  let accepted = verify_research_report(complete, task, {
    "retrieved_at": "2026-08-24T12:00:00+08:00",
    "evidence_inventory": {},
  })
  assert_true(accepted.is_empty())
}

///|
async test "worker rejects duplicate request identifiers on one channel" {
  let worker = Worker::new()
  let request = RequestEnvelope::{
    protocol_version: 1,
    request_id: "duplicate-request",
    kind: "system.handshake",
    payload: { "app_version": "test", "protocol_version": 1 },
    deadline_ms: Some(1000),
    cancellation_id: None,
  }
  let first = worker.handle(request)
  assert_true(first is { "ok": true, .. })
  let duplicate = worker.handle(request)
  assert_true(
    duplicate
    is { "ok": false, "error": { "code": "duplicate_request_id", .. }, .. },
  )
}
