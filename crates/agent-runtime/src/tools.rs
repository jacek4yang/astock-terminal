use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRisk {
    ReadOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    None,
    CanonicalRequest,
}

/// Who executes a tool.
///
/// Most tools are bounded read-only Engine operations. Finalization and evidence
/// discovery are different: they act on Runtime state — the draft contract and the
/// evidence catalog assembled during this task — and must not be reachable as
/// Engine mutations. Keeping the distinction in the type prevents a report
/// submission from ever being dispatched as an Engine effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolHandler {
    /// Dispatched to the Engine through the closed request-kind allowlist.
    Engine,
    /// Served by the Runtime itself.
    Runtime,
}

#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub engine_kind: String,
    pub risk: ToolRisk,
    pub timeout: Duration,
    pub cache_policy: CachePolicy,
    pub freshness: String,
    pub handler: ToolHandler,
}

#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    definitions: BTreeMap<String, ToolDefinition>,
}

impl ToolRegistry {
    pub fn register(&mut self, definition: ToolDefinition) -> Result<(), String> {
        if definition.name.is_empty()
            || !definition
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(format!("invalid tool name: {}", definition.name));
        }
        if self
            .definitions
            .insert(definition.name.clone(), definition)
            .is_some()
        {
            return Err("duplicate tool name".into());
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ToolDefinition> {
        self.definitions.get(name)
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.definitions.values().cloned().collect()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.definitions.keys().map(String::as_str)
    }
}

#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(
        &self,
        engine_kind: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, String>;
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn read_tool(
    name: &str,
    description: &str,
    engine_kind: &str,
    input_schema: Value,
    timeout_secs: u64,
    freshness: &str,
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
        engine_kind: engine_kind.into(),
        risk: ToolRisk::ReadOnly,
        timeout: Duration::from_secs(timeout_secs),
        cache_policy: CachePolicy::CanonicalRequest,
        freshness: freshness.into(),
        handler: ToolHandler::Engine,
    }
}

/// A Runtime-served tool.
///
/// Never dispatched to the Engine, so finalization cannot become an Engine
/// mutation and evidence discovery stays a read of Runtime state.
fn runtime_tool(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
        engine_kind: String::new(),
        risk: ToolRisk::ReadOnly,
        timeout: Duration::from_secs(15),
        cache_policy: CachePolicy::None,
        freshness: "runtime_state".into(),
        handler: ToolHandler::Runtime,
    }
}

fn computation_program_schema(joinquant: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["version", "outputs"],
        "properties": {
            "version": {"type": "integer", "enum": [1]},
            "inputs": {
                "type": "object",
                "maxProperties": 32,
                "additionalProperties": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5000,
                    "items": {"type": ["number", "null"]}
                }
            },
            "bindings": {
                "type": "array",
                "maxItems": 64,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["name", "expr"],
                    "properties": {
                        "name": {"type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]{0,63}$"},
                        "expr": {"type": "object"}
                    }
                }
            },
            "outputs": {
                "type": "object",
                "minProperties": 1,
                "maxProperties": 32,
                "additionalProperties": {"type": "object"}
            }
        },
        "description": "AST operators use an `op` discriminator. Sources: scalar{value}, var{name}. Arithmetic: add/sub/mul/div{left,right}, neg/abs{input}, clip{input,min,max}. Series: lag{input,periods}, diff/returns/log_returns/cumulative_return{input}, sma/ema/zscore/rsi{input,window}, rolling_std{input,window,annualization?}, tail{input,count}. Reductions: mean/std/sum/min/max/last/count/max_drawdown{input}, correlation{left,right}. Expressions are nested JSON objects; no code strings."
    });
    if joinquant {
        schema["properties"]["inputs"]["description"] = json!(
            "Optional extra numeric series only. Do not define open/high/low/close/volume/amount/turnover/pct; Engine injects those protected JoinQuant inputs."
        );
    }
    schema
}

/// Evidence identifier pattern.
///
/// Constraining the shape in the schema is the first line of defence against an
/// invented namespace such as `计算-BPS`: the model cannot even submit one. The
/// contract still checks existence, because a well-shaped identifier can still be
/// fabricated.
fn evidence_id_schema() -> Value {
    json!({"type": "string", "pattern": "^evf_[A-Za-z0-9_]+$", "maxLength": 80})
}

fn evidence_id_list_schema(max_items: usize) -> Value {
    json!({"type": "array", "maxItems": max_items, "items": evidence_id_schema()})
}

/// One number and the provenance it must declare.
///
/// Assembled in pieces rather than one literal: a single nested `json!` for the
/// whole report exceeded the macro recursion limit, and the parts are clearer read
/// separately.
fn numeric_item_schema() -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("label".into(), json!({"type": "string", "maxLength": 80}));
    properties.insert("value".into(), json!({"type": "number"}));
    properties.insert("unit".into(), json!({"type": "string", "maxLength": 20}));
    properties.insert(
        "provenance".into(),
        json!({
            "type": "string",
            "enum": ["observed", "calculated", "user_assumption", "estimated"]
        }),
    );
    // Observed
    properties.insert("evidence_id".into(), evidence_id_schema());
    properties.insert("field".into(), json!({"type": "string", "maxLength": 120}));
    // Calculated
    properties.insert("calculation_evidence_id".into(), evidence_id_schema());
    properties.insert(
        "operation".into(),
        json!({"type": "string", "maxLength": 80}),
    );
    properties.insert("input_evidence_ids".into(), evidence_id_list_schema(12));
    // User assumption
    properties.insert(
        "stated_in_message_id".into(),
        json!({"type": "string", "maxLength": 64}),
    );
    // Estimated
    properties.insert("method".into(), json!({"type": "string", "maxLength": 300}));
    properties.insert("basis_evidence_ids".into(), evidence_id_list_schema(12));
    properties.insert(
        "range".into(),
        json!({"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "number"}}),
    );

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["label", "value", "provenance"],
        "properties": Value::Object(properties),
    })
}

fn claim_schema() -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("id".into(), json!({"type": "string", "maxLength": 64}));
    properties.insert(
        "kind".into(),
        json!({
            "type": "string",
            "enum": [
                "observed_fact",
                "deterministic_calculation",
                "inference",
                "estimate",
                "scenario",
                "unknown"
            ]
        }),
    );
    properties.insert(
        "statement".into(),
        json!({"type": "string", "maxLength": 2000}),
    );
    properties.insert("evidence_ids".into(), evidence_id_list_schema(12));
    properties.insert(
        "confidence".into(),
        json!({"type": "string", "maxLength": 40}),
    );
    properties.insert(
        "uncertainty".into(),
        json!({"type": "string", "maxLength": 600}),
    );
    properties.insert(
        "assumptions".into(),
        json!({"type": "array", "maxItems": 12, "items": {"type": "string", "maxLength": 300}}),
    );
    properties.insert("disclosed_conflicts".into(), evidence_id_list_schema(12));
    properties.insert(
        "numeric_items".into(),
        json!({"type": "array", "maxItems": 16, "items": numeric_item_schema()}),
    );

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "kind", "statement"],
        "properties": Value::Object(properties),
    })
}

fn submit_report_schema() -> Value {
    let section = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["heading", "claim_ids"],
        "properties": {
            "heading": {"type": "string", "maxLength": 120},
            "claim_ids": {
                "type": "array",
                "minItems": 1,
                "items": {"type": "string", "maxLength": 64}
            }
        }
    });
    let mut properties = serde_json::Map::new();
    properties.insert("version".into(), json!({"type": "string", "maxLength": 64}));
    properties.insert("title".into(), json!({"type": "string", "maxLength": 200}));
    properties.insert(
        "executive_summary".into(),
        json!({"type": "string", "maxLength": 4000}),
    );
    properties.insert(
        "overall_uncertainty".into(),
        json!({"type": "string", "maxLength": 2000}),
    );
    properties.insert(
        "limitations".into(),
        json!({"type": "array", "maxItems": 20, "items": {"type": "string", "maxLength": 500}}),
    );
    properties.insert(
        "sections".into(),
        json!({"type": "array", "minItems": 1, "maxItems": 24, "items": section}),
    );
    properties.insert(
        "claims".into(),
        json!({"type": "array", "minItems": 1, "maxItems": 240, "items": claim_schema()}),
    );

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["version", "title", "executive_summary", "sections", "claims"],
        "properties": Value::Object(properties),
    })
}

pub fn default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::default();
    let tools = [
        read_tool(
            "get_quote",
            "获取一只A股的最新行情；返回来源、观测时间和质量信息。",
            "market.quote",
            object_schema(
                json!({"symbol": {"type": "string", "pattern": "^[0-9]{6}$"}}),
                &["symbol"],
            ),
            20,
            "live",
        ),
        read_tool(
            "get_kline",
            "获取有界历史K线，用于趋势、波动和结构分析。",
            "market.kline",
            object_schema(
                json!({
                    "symbol": {"type": "string", "pattern": "^[0-9]{6}$"},
                    "period": {"type": "string", "enum": ["day", "week", "month"]},
                    "adjust": {"type": "string", "enum": ["qfq", "hfq", "none"]},
                    "count": {"type": "integer", "minimum": 1, "maximum": 500}
                }),
                &["symbol", "period", "adjust", "count"],
            ),
            30,
            "market_close_or_live",
        ),
        read_tool(
            "get_fundamentals",
            "获取财务报表、关键比率、估值和股本等基本面证据。",
            "research.fundamentals",
            object_schema(
                json!({"symbol": {"type": "string", "pattern": "^[0-9]{6}$"}}),
                &["symbol"],
            ),
            45,
            "latest_disclosed",
        ),
        read_tool(
            "run_financial_calculation",
            "执行受限、可复现、带燃料上限的金融计算 AST。支持顺序 let 绑定、序列算术、收益率、均线、波动率、Z-score、RSI、相关性、最大回撤和归约；禁止代码字符串、文件、进程、任意网络、时钟和随机数。",
            "research.compute",
            object_schema(
                json!({"program": computation_program_schema(false)}),
                &["program"],
            ),
            20,
            "deterministic_input_snapshot",
        ),
        read_tool(
            "research_news",
            "从多类财经资讯源获取有界的近期证据；单源失败应保留为降级状态。",
            "research.news",
            object_schema(
                json!({
                    "sources": {"type": "array", "items": {"type": "string"}, "maxItems": 12},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                }),
                &["sources", "limit"],
            ),
            45,
            "live",
        ),
        read_tool(
            "get_market_regime",
            "用确定性引擎计算当前市场状态。",
            "research.market.regime",
            object_schema(json!({}), &[]),
            30,
            "latest_market_data",
        ),
        read_tool(
            "get_joinquant_context",
            "从用户授权的聚宽研究环境获取前复权日线、估值、基准成分和宏观CPI；仅在聚宽会话已配置时使用。",
            "research.joinquant_context",
            object_schema(
                json!({
                    "symbol": {"type": "string", "pattern": "^[0-9]{6}$"},
                    "start": {"type": "string", "format": "date"},
                    "end": {"type": "string", "format": "date"},
                    "benchmark": {"type": "string", "pattern": "^[0-9]{6}$"}
                }),
                &["symbol", "start", "end", "benchmark"],
            ),
            240,
            "explicit_joinquant_research_session",
        ),
        read_tool(
            "run_joinquant_calculation",
            "从已授权聚宽会话获取有界前复权日线，将 open/high/low/close/volume/amount/turnover/pct 注入受限金融计算 AST，并在本地 Rust Engine 中确定性执行；绝不向远端提交模型生成的任意代码。",
            "research.joinquant_compute",
            object_schema(
                json!({
                    "symbol": {"type": "string", "pattern": "^[0-9]{6}$"},
                    "start": {"type": "string", "format": "date"},
                    "end": {"type": "string", "format": "date"},
                    "program": computation_program_schema(true)
                }),
                &["symbol", "start", "end", "program"],
            ),
            180,
            "explicit_joinquant_qfq_snapshot_then_deterministic_compute",
        ),
        read_tool(
            "prepare_market_research",
            "并行获取市场广度、宏观、资讯和候选池的有界研究快照。",
            "research.agent_prepare_context",
            object_schema(
                json!({
                    "depth": {"type": "string", "enum": ["fast", "balanced", "deep", "exhaustive"]},
                    "capital": {"type": "number", "exclusiveMinimum": 0}
                }),
                &["depth"],
            ),
            90,
            "live",
        ),
        read_tool(
            "research_securities",
            "并行获取1至5只证券的行情、基本面、公告、资讯、交叉核验及选定高级分析。",
            "research.agent_security_context",
            object_schema(
                json!({
                    "symbols": {"type": "array", "minItems": 1, "maxItems": 5, "items": {"type": "string", "pattern": "^[0-9]{6}$"}},
                    "depth": {"type": "string", "enum": ["fast", "balanced", "deep", "exhaustive"]},
                    "tool_policy": {"type": "string", "enum": ["auto", "market", "evidence", "full"]},
                    "analysis_modules": {"type": "array", "items": {"type": "string", "enum": ["earnings_driver", "industry_graph", "relationship", "market_regime", "historical_backtest"]}},
                    "benchmark": {"type": "string"},
                    "start": {"type": "string", "format": "date"},
                    "end": {"type": "string", "format": "date"}
                }),
                &[
                    "symbols",
                    "depth",
                    "tool_policy",
                    "analysis_modules",
                    "benchmark",
                    "start",
                    "end",
                ],
            ),
            120,
            "mixed_live_and_disclosed",
        ),
        // Bounded evidence discovery.
        //
        // A live task registered 6,578 distinct identifiers and the report cited
        // 37. Putting them all in context is impossible and unnecessary; the model
        // needs a way to ask for the canonical identifier of a fact it wants to
        // state. Read-only over Runtime state, so it costs no upstream call.
        runtime_tool(
            "search_evidence",
            "在本次研究已收集的证据中按证券、来源、字段或关键词有界检索，返回可直接引用的规范证据标识及其来源、时间、单位和质量状态。用于在撰写结论前找到应引用的证据，不要凭记忆编造标识。",
            object_schema(
                json!({
                    "symbol": {"type": "string", "pattern": "^[0-9]{6}$"},
                    "source": {"type": "string", "maxLength": 40},
                    "field": {"type": "string", "maxLength": 120},
                    "keyword": {"type": "string", "maxLength": 80},
                    "only_calculations": {"type": "boolean"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50}
                }),
                &["limit"],
            ),
        ),
        // Structured finalization.
        //
        // Replaces "write Markdown and hope the verifier can reconstruct it".
        // The model supplies semantic evidence identifiers and numeric provenance;
        // the Runtime validates, renders the citations, and only then hands the
        // canonical form to the independent verifier. This is not an alternate
        // publication path: the verifier still runs and still fails closed.
        runtime_tool(
            "submit_report",
            "提交结构化研究报告用于校验与发布。不要在 statement 中手写【E:...】引用；只需在 evidence_ids 与 numeric_items 中给出规范证据标识，引用格式由 Runtime 渲染。每个数字必须声明来源：observed（实测，需 evidence_id）、calculated（确定性计算，需计算结果标识、运算与输入证据）、user_assumption（用户给定的情景参数）或 estimated（需方法、依据证据，尽量给区间）。可确定性计算的数值不得用 estimated。",
            submit_report_schema(),
        ),
    ];
    for tool in tools {
        registry
            .register(tool)
            .expect("the built-in tool registry is valid");
    }
    registry
}
