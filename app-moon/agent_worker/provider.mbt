///|
/// Read the MiniMax credential directly from Windows Credential Manager.
/// The returned secret remains inside this worker process and is only used to
/// construct HTTPS Authorization headers.
extern "C" fn read_minimax_key() -> String = "astock_read_minimax_key"

///|
struct AvailableModel {
  id : String
} derive(FromJson)

///|
struct AvailableModelsResponse {
  data : Array[AvailableModel]
} derive(FromJson)

///|
struct ChatMessageResponse {
  content : String
} derive(FromJson)

///|
struct ChatChoiceResponse {
  message : ChatMessageResponse
} derive(FromJson)

///|
struct ChatCompletionResponse {
  choices : Array[ChatChoiceResponse]
} derive(FromJson)

///|
struct ProviderConfig {
  api_host : String
  model : String
  fast_model : String
  deep_model : String
  verifier_model : String
  available_models : Array[String]
  multi_agent_enabled : Bool
  max_parallel_agents : Int
} derive(Debug)

///|
struct ModelRouting {
  coordinator_model : String
  fast_model : String
  deep_model : String
  verifier_model : String
  multi_agent_enabled : Bool
  max_parallel_agents : Int
} derive(FromJson, ToJson)

///|
extend ModelRouting with ToJson::{to_json}

///|
fn default_model_routing() -> ModelRouting {
  ModelRouting::{
    coordinator_model: "auto",
    fast_model: "auto",
    deep_model: "auto",
    verifier_model: "auto",
    multi_agent_enabled: true,
    max_parallel_agents: 3,
  }
}

///|
fn selected_model(
  available : Array[String],
  requested : String,
  fallback : String,
) -> String raise {
  let requested = requested.trim().to_owned()
  if requested.is_empty() || requested == "auto" {
    return fallback
  }
  if !available.contains(requested) {
    fail("configured_model_not_available:" + requested)
  }
  requested
}

///|
fn ProviderConfig::with_routing(
  self : ProviderConfig,
  routing : ModelRouting,
) -> ProviderConfig raise {
  if routing.max_parallel_agents < 1 || routing.max_parallel_agents > 4 {
    fail("invalid_max_parallel_agents")
  }
  ProviderConfig::{
    api_host: self.api_host,
    model: selected_model(
      self.available_models,
      routing.coordinator_model,
      self.model,
    ),
    fast_model: selected_model(
      self.available_models,
      routing.fast_model,
      self.fast_model,
    ),
    deep_model: selected_model(
      self.available_models,
      routing.deep_model,
      self.model,
    ),
    verifier_model: selected_model(
      self.available_models,
      routing.verifier_model,
      self.model,
    ),
    available_models: self.available_models,
    multi_agent_enabled: routing.multi_agent_enabled,
    max_parallel_agents: routing.max_parallel_agents,
  }
}

///|
/// Bounded liquid-market candidate supplied by the Rust Engine. This is a
/// discovery pool, never a recommendation by itself.
struct ResearchCandidate {
  symbol : String
  name : String
  market : String?
  board : String?
  industry : String?
  price : Double?
  pct : Double?
  amount : Double?
  lot_cost : Double?
} derive(FromJson, ToJson)

///|
/// Model-selected symbols are validated against the Engine candidate pool
/// before any expensive per-security tools run.
struct ResearchPlan {
  symbols : Array[String]
  selection_summary : String
  verification_focus : Array[String]
  rejected_risks : Array[String]
} derive(FromJson, ToJson)

///|
extend ResearchPlan with ToJson::{to_json}

///|
/// Deliberately minimal wire contract: providers sometimes serialize
/// explanatory lists as prose. Symbol selection is the only field allowed to
/// control tool execution; all other visible planning notes are deterministic.
struct ResearchPlanSelection {
  symbols : Array[String]
} derive(FromJson)

///|
fn provider_headers(key : String) -> @http.Headers {
  Map([
    ("Authorization", "Bearer " + key),
    ("Content-Type", "application/json"),
    ("Accept", "application/json"),
  ])
}

///|
/// MiniMax OpenAI-compatible responses may keep native `<think>` content in
/// `message.content`. Structured Agent calls must never parse or display that
/// private reasoning; locate only the final JSON object after the tag.
fn model_json_content(content : String) -> Json raise {
  let raw = content.trim().to_owned()
  // Some entitled endpoints can ignore reasoning_split. Starting the JSON
  // scan after the final closing tag prevents braces in private reasoning
  // from corrupting the published structured value.
  let text = match raw.rev_find("</think>") {
    Some(end) => raw[end + 8:].trim().to_owned()
    None => raw
  }
  let start = text.find("{")
  let end = text.rev_find("}")
  match (start, end) {
    (Some(first), Some(last)) if last >= first =>
      @json.parse(text[first:last + 1].to_owned())
    _ =>
      fail(
        "MiniMax响应未包含可用的结构化结果；模型思考内容已安全丢弃，请重试",
      )
  }
}

///|
/// Return only the user-visible answer when a provider ignored
/// `reasoning_split`; never surface `<think>` blocks in the React transcript.
fn visible_model_text(content : String) -> String raise {
  let text = content.trim().to_owned()
  match text.rev_find("</think>") {
    Some(end) => {
      let visible = text[end + 8:].trim().to_owned()
      if visible.is_empty() {
        fail(
          "MiniMax仅返回了思考内容，没有可发布的研究正文，请重试",
        )
      }
      visible
    }
    None => {
      if text.has_prefix("<think>") {
        fail(
          "MiniMax思考响应不完整，未发布私有推理内容，请重试",
        )
      }
      text
    }
  }
}

///|
/// Select the strongest available reasoning model without baking one model
/// into the renderer. MiniMax's model catalog is authoritative.
fn choose_model(models : Array[AvailableModel]) -> String raise {
  guard !models.is_empty() else { fail("provider_model_catalog_empty") }
  for preferred in ["MiniMax-M3", "MiniMax-M2.7", "MiniMax-M2.5"] {
    for model in models {
      if model.id == preferred {
        return model.id
      }
    }
  }
  for model in models {
    if model.id.contains("MiniMax") {
      return model.id
    }
  }
  models[0].id
}

///|
/// Planning and schema-constrained review favor an entitled high-speed model
/// while final synthesis keeps the strongest catalog model.
fn choose_fast_model(
  models : Array[AvailableModel],
  fallback : String,
) -> String {
  for
    preferred in [
      "MiniMax-M2.7-highspeed", "MiniMax-M2.5-highspeed", "MiniMax-M3", "MiniMax-M2.7",
    ] {
    for model in models {
      if model.id == preferred {
        return model.id
      }
    }
  }
  fallback
}

///|
async fn discover_provider(key : String) -> ProviderConfig {
  let headers = provider_headers(key)
  let failures : Array[String] = []
  for host in ["https://api.minimaxi.com", "https://api.minimax.io"] {
    let result = try {
      let (response, body) = @http.get(host + "/v1/models", headers~)
      if response.code != 200 {
        failures.push(host + ":http_" + response.code.to_string())
        None
      } else {
        let catalog : AvailableModelsResponse = @json.from_json(body.json())
        let model = choose_model(catalog.data)
        let available_models = catalog.data.map(fn(item) { item.id })
        Some(ProviderConfig::{
          api_host: host,
          model,
          fast_model: choose_fast_model(catalog.data, model),
          deep_model: model,
          verifier_model: model,
          available_models,
          multi_agent_enabled: true,
          max_parallel_agents: 3,
        })
      }
    } catch {
      error => {
        // Never include headers or the credential in diagnostic text.
        ignore(error)
        failures.push(host + ":unreachable")
        None
      }
    }
    match result {
      Some(provider) => return provider
      None => ()
    }
  }
  fail("minimax_service_unavailable:" + failures.join(","))
}

///|
async fn chat_json_once(
  provider : ProviderConfig,
  key : String,
  system : String,
  user : String,
  model : String,
) -> Json {
  let request : Json = {
    "model": model,
    "messages": [
      { "role": "system", "content": system },
      { "role": "user", "content": user },
    ],
    "stream": false,
    "temperature": 0.1,
    "reasoning_split": true,
    // MiniMax-M3 can disable thinking for schema-only recovery calls. M2.x
    // accepts this field but keeps thinking enabled, so the strong M3 retry
    // remains the reliable bounded path when a high-speed model exhausts its
    // completion budget before publishing JSON.
    "thinking": { "type": "disabled" },
    "max_completion_tokens": 16384,
  }
  let headers = provider_headers(key)
  let (response, body) = @http.post(
    provider.api_host + "/v1/chat/completions",
    request,
    headers~,
  )
  if response.code != 200 {
    fail("minimax_chat_http_" + response.code.to_string())
  }
  let payload = body.json() catch {
    _ =>
      fail(
        "MiniMax返回了非JSON响应，可能是服务网关或区域端点异常；请在配置页刷新额度后重试",
      )
  }
  let completion : ChatCompletionResponse = @json.from_json(payload)
  if completion.choices.is_empty() {
    fail("minimax_chat_empty_choices")
  }
  let choice = completion.choices[0]
  model_json_content(choice.message.content)
}

///|
/// Reasoning models occasionally consume the first completion budget before
/// emitting their final JSON. Use the entitled high-speed model only for the
/// first attempt, then recover with the strongest catalog model. Retry at most
/// twice with an explicit final-only correction; never parse or expose
/// reasoning_details as task output.
async fn chat_json(
  provider : ProviderConfig,
  key : String,
  system : String,
  user : String,
) -> Json {
  let attempts = [
    user,
    user +
    "\n上一次未产生最终JSON。请停止继续分析，现在只返回契约要求的JSON对象。",
    user +
    "\n这是最后一次有界重试：只输出一个完整JSON对象，不要思考文本、解释或Markdown。",
  ]
  for index, prompt in attempts {
    let model = if index == 0 {
      provider.fast_model
    } else if index == 1 {
      provider.model
    } else {
      provider.verifier_model
    }
    let result : Json? = Some(
      chat_json_once(provider, key, system, prompt, model),
    ) catch {
      error => {
        ignore(error)
        None
      }
    }
    match result {
      Some(value) => return value
      None => ()
    }
  }
  fail(
    "MiniMax结构化响应连续三次缺失；任务检查点已保留，请稍后重试",
  )
}

///|
/// Text completion for the final user-visible report. Unlike clarification
/// calls, this deliberately keeps the response as plain text.
async fn chat_text(
  provider : ProviderConfig,
  key : String,
  system : String,
  user : String,
) -> String {
  let request : Json = {
    "model": provider.deep_model,
    "messages": [
      { "role": "system", "content": system },
      { "role": "user", "content": user },
    ],
    "stream": false,
    "temperature": 0.1,
    "reasoning_split": true,
    "max_completion_tokens": 8192,
  }
  let headers = provider_headers(key)
  let (response, body) = @http.post(
    provider.api_host + "/v1/chat/completions",
    request,
    headers~,
  )
  if response.code != 200 {
    fail("minimax_chat_http_" + response.code.to_string())
  }
  let payload = body.json() catch {
    _ =>
      fail(
        "MiniMax返回了非JSON响应，可能是服务网关或区域端点异常；请在配置页刷新额度后重试",
      )
  }
  let completion : ChatCompletionResponse = @json.from_json(payload)
  if completion.choices.is_empty() {
    fail("minimax_chat_empty_choices")
  }
  let text = visible_model_text(completion.choices[0].message.content)
  if text.length() < 120 {
    fail("minimax_research_report_too_short")
  }
  text
}

///|
/// Evidence-first synthesis over data already fetched by the desktop Engine.
/// The Worker never talks to database paths and never invents missing values.
async fn generate_research_report(
  provider : ProviderConfig,
  key : String,
  spec : @protocol.TaskSpec,
  context : Json,
) -> String {
  let output_rules = if spec.output_type == "manual_plan" {
    "本次主要输出是人工计划：从objective识别资金总额；计划分配金额加现金备用金不得超过该总额。必须遵守A股普通股票100股一手，按输入最新价检查每项最低可执行成本并预留佣金、规费与滑点；不能满足时改为观察名单或现金，不得用零碎股数凑数。必须给出候选取舍、现金比例、单一标的和组合风险上限、观察条件、触发条件、失效条件、复核时间点。所有价格条件必须追溯到输入快照；快照过时、停牌、涨跌停或缺少可靠价位时明确暂停并要求刷新，不得虚构。"
  } else {
    "本次主要输出是股票行情与投资研究分析：优先解释趋势、量价、资金、基本面、估值、事件与市场环境之间的一致和冲突，不把单一技术信号当成确定结论。"
  }
  let review_system = "你是A股研究质量审计员。只基于输入证据做结构化审查，不输出私有推理链。逐个候选列出支持证据、反方证据、新闻与公告时效、基本面/估值缺口、数据冲突、可能淘汰原因。不能从缺失值推断结论。只输出JSON对象，字段必须为candidate_reviews、market_regime、portfolio_constraints、data_gaps；candidate_reviews为数组。"
  let review_input : Json = {
    "task_spec": spec.to_json(),
    "engine_research_context": context,
    "review_round": "evidence_assessment",
  }
  let evidence_review = chat_json(
    provider,
    key,
    review_system,
    review_input.stringify(),
  )
  let challenge_system = "你是独立反方复核员。审查第一轮证据评估与原始资料，主动寻找过时新闻、单一来源、幸存者偏差、价格不可成交、A股100股一手约束、费用滑点、相关性过高、基本面与技术面冲突。输出JSON对象，字段必须为unsupported_claims、candidate_rejections、required_corrections、residual_risks、release_decision。不得补造数据。"
  let challenge_input : Json = {
    "task_spec": spec.to_json(),
    "engine_research_context": context,
    "evidence_review": evidence_review,
    "review_round": "adversarial_challenge",
  }
  let adversarial_review = chat_json(
    provider,
    key,
    challenge_system,
    challenge_input.stringify(),
  )
  let system = "你是A股证据优先高级研究Agent。最终报告必须同时服从原始Engine证据、第一轮证据审查和独立反方复核；release_decision不允许发布时应明确暂停而非强行给方案。缺失字段必须写暂无数据，禁止补造价格、财务指标、新闻、来源或结论。输出纯文本中文，不使用Markdown表格或代码围栏。按固定顺序给出：执行摘要、市场环境、候选逐项复核、资金与仓位方案、触发/失效条件、反方证据与不确定性、主要风险、人工复核清单、数据来源与时点、未覆盖能力。每个关键判断紧邻标注来自行情、K线、资金流、基本面、估值、新闻/公告或复核记录中的哪一部分。不得声称自动下单，不得保证收益。" +
    output_rules
  let user : Json = {
    "task_spec": spec.to_json(),
    "engine_research_context": context,
    "evidence_review": evidence_review,
    "adversarial_review": adversarial_review,
    "output_contract": "astock-evidence-report/plain-text-v1",
  }
  chat_text(provider, key, system, user.stringify())
}

///|
/// Deterministic publication gate applied after model synthesis. This does
/// not judge investment merit; it rejects structurally incomplete drafts,
/// leaked private reasoning and reports that omit the manual-only boundary.
fn verify_research_report(
  report : String,
  spec : @protocol.TaskSpec,
  context : Json,
) -> Array[String] {
  let errors : Array[String] = []
  if report.trim().length() < 800 {
    errors.push("report_too_short_for_evidence_review")
  }
  if report.contains("<think>") || report.contains("reasoning_details") {
    errors.push("private_reasoning_leak")
  }
  for
    section in [
      "执行摘要", "市场环境", "反方证据", "主要风险", "人工复核清单",
      "数据来源与时点", "未覆盖能力",
    ] {
    if !report.contains(section) {
      errors.push("missing_section:" + section)
    }
  }
  if !report.contains("人工") || !report.contains("不自动下单") {
    errors.push("manual_execution_boundary_missing")
  }
  if spec.output_type == "manual_plan" {
    for
      requirement in [
        "资金", "现金", "一手", "触发", "失效", "复核",
      ] {
      if !report.contains(requirement) {
        errors.push("manual_plan_requirement_missing:" + requirement)
      }
    }
  }
  let context_text = context.stringify()
  if !context_text.contains("evidence_inventory") ||
    !context_text.contains("retrieved_at") {
    errors.push("evidence_inventory_missing")
  }
  errors
}

///|
/// Cheap first pass over a bounded liquid universe. The final report may
/// still reject every selected symbol after detailed evidence collection.
async fn generate_research_plan(
  provider : ProviderConfig,
  key : String,
  spec : @protocol.TaskSpec,
  candidates : Array[ResearchCandidate],
  market_context : Json,
) -> ResearchPlan {
  if candidates.is_empty() {
    fail("research_candidate_pool_empty")
  }
  let system = "你是A股研究工具规划器。候选池只是按流动性得到的发现列表，不代表推荐。只能从candidate_pool的symbol中选1至5个进入深度取证；考虑用户资金、A股100股一手、分散度、成交活跃度、当日极端波动和任务边界。此阶段不能给最终买卖结论。只输出形如{\"symbols\":[\"000001\"]}的JSON对象，不要任何其他字段。"
  let user : Json = {
    "task_spec": spec.to_json(),
    "candidate_pool": candidates.to_json(),
    "market_context": market_context,
  }
  let value = chat_json(provider, key, system, user.stringify())
  let selection : ResearchPlanSelection = @json.from_json(value)
  if selection.symbols.is_empty() || selection.symbols.length() > 5 {
    fail("invalid_research_plan_symbol_count")
  }
  let unique : Array[String] = []
  for symbol in selection.symbols {
    if !candidates.any(fn(candidate) { candidate.symbol == symbol }) {
      fail("research_plan_selected_unknown_symbol")
    }
    if !unique.contains(symbol) {
      unique.push(symbol)
    }
  }
  if unique.is_empty() {
    fail("research_plan_has_no_valid_symbol")
  }
  ResearchPlan::{
    symbols: unique,
    selection_summary: "候选仅用于进入深度取证；最终报告可淘汰全部候选并保留现金",
    verification_focus: [
      "行情与K线跨源一致性", "资金流、财务三表与估值的相互印证",
      "公告优先于聚合新闻，检查旧闻重发与单一来源", "2万元资金、100股一手、费用滑点与组合风险边界",
    ],
    rejected_risks: [
      "风险警示或退市名称已由Engine预筛除", "最低一手成本超出资金约束的证券已由Engine预筛除",
      "最终仍须依据详细证据排除冲突或缺失严重的候选",
    ],
  }
}

///|
fn validate_clarification(
  request : @protocol.ClarificationRequest,
  missing : Array[String],
) -> Unit raise {
  if request.title.trim().is_empty() ||
    request.questions.length() < 1 ||
    request.questions.length() > 3 {
    fail("invalid_model_clarification_question_count")
  }
  let covered : Array[String] = []
  for question in request.questions {
    if question.id.trim().is_empty() ||
      question.question.trim().is_empty() ||
      question.options.length() < 2 ||
      question.options.length() > 6 ||
      question.target_fields.is_empty() {
      fail("invalid_model_clarification_question")
    }
    for field in question.target_fields {
      if !missing.contains(field) {
        fail("model_clarification_targets_non_missing_field")
      }
      if !covered.contains(field) {
        covered.push(field)
      }
    }
    for option in question.options {
      if option.id.trim().is_empty() || option.label.trim().is_empty() {
        fail("invalid_model_clarification_option")
      }
    }
  }
  for field in missing {
    if !covered.contains(field) {
      fail("model_clarification_did_not_cover_all_missing_fields")
    }
  }
}

///|
/// The provider creates questions and options from the current goal. There
/// are intentionally no locally defined question templates or business
/// defaults in this worker.
async fn generate_clarification(
  provider : ProviderConfig,
  key : String,
  spec : @protocol.TaskSpec,
  missing : Array[String],
) -> @protocol.ClarificationRequest {
  let system = "你是A股证据优先研究任务的澄清规划器。只询问会实质改变研究路线且当前缺失的信息，不询问Agent可通过工具查到的事实。问题和候选项必须针对用户本次目标动态生成，禁止套用固定问卷。将相关字段合并成1至3个问题，每题2至6个易懂选项，并给出简短说明和至多一个推荐项。不得给出投资结论。只输出一个JSON对象，不要Markdown、代码围栏或额外文字。JSON严格包含title、description、questions；每个问题包含id、header、question、kind、options、allow_other、target_fields；每个选项包含id、label、description、recommended。target_fields必须覆盖且只能使用给出的missing_fields。"
  let user : Json = {
    "task_spec": spec.to_json(),
    "missing_fields": missing,
    "output_contract": "astock-clarification/v1",
  }
  let value = chat_json(provider, key, system, user.stringify())
  let request : @protocol.ClarificationRequest = @json.from_json(value)
  validate_clarification(request, missing)
  request
}

///|
/// Convert the user's structured response into a complete TaskSpec. The
/// model may map bundled answers to several fields. Explicit Agent-best
/// authorizations are represented by a visible sentinel and are not
/// disguised as choices made by the user.
async fn resolve_clarification(
  provider : ProviderConfig,
  key : String,
  spec : @protocol.TaskSpec,
  request : @protocol.ClarificationRequest,
  answers : Array[@protocol.ClarificationAnswer],
) -> @protocol.TaskSpec {
  let system = "你是A股研究任务规范化器。根据模型先前提出的问题和用户的结构化回答，补全TaskSpec。不得改变已存在的非空字段，不得创造用户未授权的风险偏好或投资期限。decision_mode为user_selected时严格采用用户答案；为agent_best_with_evidence时，该问题覆盖的字段统一写入明确标记AGENT_BEST_AFTER_EVIDENCE，表示后续取证后再决定，不得伪装成用户选择。只输出TaskSpec JSON，不要Markdown或额外字段。字段必须为objective、security_universe、as_of、research_start、research_end、investment_horizon、comparison_benchmark、output_type、evidence_requirement。"
  let user : Json = {
    "current_task_spec": spec.to_json(),
    "clarification_request": request.to_json(),
    "answers": answers.to_json(),
  }
  let value = chat_json(provider, key, system, user.stringify())
  let resolved : @protocol.TaskSpec = @json.from_json(value)
  if !@agent.missing_task_fields(resolved).is_empty() {
    fail("model_did_not_complete_task_spec")
  }
  resolved
}
