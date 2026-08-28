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

/// One or many queries in a single call.
///
/// A moderate report declares ten to twenty figures and each needs an identifier. With
/// one question per call the model spent one model round per figure — a live balanced
/// task issued 42 distinct searches and never reached a publishable report. The single
/// form stays accepted so nothing that already works breaks.
fn evidence_search_schema() -> Value {
    let query = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "symbol": {"type": "string", "pattern": "^[0-9]{6}$"},
            "source": {"type": "string", "maxLength": 40},
            "field": {"type": "string", "maxLength": 120},
            "keyword": {"type": "string", "maxLength": 80},
            "only_calculations": {"type": "boolean"},
            "limit": {"type": "integer", "minimum": 1, "maximum": 50}
        }
    });
    json!({
        "type": "object",
        "additionalProperties": false,
        "description": "Prefer the batch form: one call, one entry per figure you need an identifier for.",
        "properties": {
            "queries": {
                "type": "array",
                "minItems": 1,
                "maxItems": crate::catalog::MAX_BATCH_QUERIES,
                "items": query.clone(),
                "description": "Answer many lookups in one round."
            },
            "limit": {"type": "integer", "minimum": 1, "maximum": 50},
            "symbol": query["properties"]["symbol"].clone(),
            "source": query["properties"]["source"].clone(),
            "field": query["properties"]["field"].clone(),
            "keyword": query["properties"]["keyword"].clone(),
            "only_calculations": query["properties"]["only_calculations"].clone()
        }
    })
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
        "description": "AST operators use an `op` discriminator and are always nested JSON objects — never a string, never a bare number. Sources: {\"op\":\"scalar\",\"value\":1.95}, {\"op\":\"var\",\"name\":\"price\"}. Arithmetic: add/sub/mul/div{left,right}, neg/abs{input}, clip{input,min,max}. Series: lag{input,periods}, diff/returns/log_returns/cumulative_return{input}, sma/ema/zscore/rsi{input,window}, rolling_std{input,window,annualization?}, tail{input,count}. Reductions: mean/std/sum/min/max/last/count/max_drawdown{input}, correlation{left,right}. Inputs are arrays of numbers (or null), never strings: {\"price\":[34.63],\"eps\":[1.95]}. Worked PE example: {\"version\":1,\"inputs\":{\"price\":[34.63],\"eps\":[1.95]},\"outputs\":{\"pe\":{\"op\":\"div\",\"left\":{\"op\":\"var\",\"name\":\"price\"},\"right\":{\"op\":\"var\",\"name\":\"eps\"}}}}. Prefer one-element scalar inputs for ratios (PE, PB, market_cap, YoY); do not paste a 250-day close series unless a rolling operator needs it."
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
        // Which extra fields each provenance class requires.
        //
        // Listing them as optional siblings told the model nothing about valid
        // combinations, so it repeatedly sent `provenance: "calculated"` with no
        // `calculation_evidence_id`. That fails to decode rather than validating, and
        // a live moderate run spent its whole finalization budget on it — six
        // submissions, the same missing field each time. The conditional says it in
        // the schema, where the model is already looking, instead of only in prose.
        //
        // A provider that ignores `allOf`/`if` loses nothing: the contract still
        // rejects the same drafts, with a diagnostic that now names the exact field.
        "allOf": [
            {
                "if": {"properties": {"provenance": {"const": "observed"}}, "required": ["provenance"]},
                "then": {"required": ["evidence_id"]}
            },
            {
                "if": {"properties": {"provenance": {"const": "calculated"}}, "required": ["provenance"]},
                "then": {"required": ["calculation_evidence_id", "operation", "input_evidence_ids"]}
            },
            {
                "if": {"properties": {"provenance": {"const": "estimated"}}, "required": ["provenance"]},
                "then": {"required": ["method", "basis_evidence_ids"]}
            }
        ],
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
    // A const, not a free string.
    //
    // A live run's first submission failed on `contract_version_mismatch`, which is
    // a pure formality: the model had to reproduce a version string from memory with
    // nothing constraining it. Pinning it in the schema makes that failure
    // unrepresentable instead of merely diagnosed.
    properties.insert(
        "version".into(),
        json!({
            "type": "string",
            "enum": [crate::report::REPORT_CONTRACT_VERSION],
            "description": "Send exactly this value.",
        }),
    );
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
            "Latest quote for one A-share security, with source, observation time and quality metadata.",
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
            "Bounded historical K-line series for trend, volatility and structure analysis.",
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
            "Fundamental evidence: statements, key ratios, valuation and share counts.",
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
            "Run a bounded, reproducible, fuel-metered financial calculation AST in the Engine. One program may declare many named outputs; compute everything you need in ONE call rather than one call per figure. Supports sequential let bindings, series arithmetic, returns, moving averages, volatility, z-score, RSI, correlation, max drawdown and reductions. No code strings, files, processes, network, clock or randomness. Use this for material arithmetic instead of computing in prose.",
            "research.compute",
            json!({
                "type": "object",
                "additionalProperties": false,
                "description": "Supply `programs` to compute several figures in one call, or `program` for one.",
                "properties": {
                    "program": computation_program_schema(false),
                    "programs": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 12,
                        "items": computation_program_schema(false),
                        "description": "Preferred: one call, one program per figure."
                    }
                }
            }),
            20,
            "deterministic_input_snapshot",
        ),
        read_tool(
            "research_news",
            "Bounded recent news evidence from multiple financial sources. A single failed source is reported as degraded coverage, not silently dropped.",
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
            "Deterministic current market-regime assessment.",
            "research.market.regime",
            object_schema(json!({}), &[]),
            30,
            "latest_market_data",
        ),
        read_tool(
            "get_joinquant_context",
            "Forward-adjusted daily bars, valuation, benchmark constituents and macro CPI from the user-authorised JoinQuant research environment. Use only when a JoinQuant session is configured.",
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
            "Fetch bounded forward-adjusted daily bars from an authorised JoinQuant session, inject open/high/low/close/volume/amount/turnover/pct into the bounded calculation AST, and evaluate deterministically in the local Engine. Model-generated code is never sent to any remote environment.",
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
            "Bounded parallel research snapshot: market breadth, macro context, news and candidate pool.",
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
            "Parallel research for 1 to 5 securities: quotes, fundamentals, disclosures, news, cross-source checks and selected advanced analysis.",
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
            "Search this task's bounded evidence catalog for canonical evidence identifiers, with source, time, unit and quality state. Ask for everything you need in ONE call: pass `queries` as a list, one entry per figure you intend to cite. Use before finalization whenever a claim needs provenance. Never invent an identifier.",
            evidence_search_schema(),
        ),
        // Foolproof arithmetic over already-gathered evidence.
        //
        // Live Case C runs burned up to sixteen rounds on malformed calculation
        // ASTs after coverage was complete. This tool never asks the model to
        // author an AST: it names an op and two operands (evidence ids or
        // scalars), and the Runtime builds the program and dispatches it.
        runtime_tool(
            "compute_from_evidence",
            "Compute a ratio or product from evidence already in this task's catalog, or from a scalar. Preferred for PE, PB, market_cap, YoY and similar figures. Pass every figure you need in ONE call via `calculations`. Do not author a calculation AST — name the op and the operands. Operands are either {\"evidence_id\":\"evf_…\"} or {\"value\":34.63}. Ops: div, mul, add, sub.",
            json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["calculations"],
                "properties": {
                    "calculations": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 12,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["label", "op", "left", "right"],
                            "properties": {
                                "label": {"type": "string", "maxLength": 80},
                                "op": {"type": "string", "enum": ["div", "mul", "add", "sub"]},
                                "left": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "evidence_id": {"type": "string", "pattern": "^evf_[A-Za-z0-9_]+$", "maxLength": 80},
                                        "value": {"type": "number"}
                                    }
                                },
                                "right": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "evidence_id": {"type": "string", "pattern": "^evf_[A-Za-z0-9_]+$", "maxLength": 80},
                                        "value": {"type": "number"}
                                    }
                                }
                            }
                        }
                    }
                }
            }),
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
            "Submit the final structured research draft for validation and publication. Do not write citation markup; supply canonical identifiers and the runtime renders citations. Every printed figure must be declared as a numeric_item or contained in cited evidence. Prefer referencing by label in braces anywhere in prose, so `close {close}` prints the verified value. Provenance per item: observed (evidence_id), calculated (calculation_evidence_id, operation, input_evidence_ids), user_assumption (a user-supplied scenario parameter), or estimated (method, basis_evidence_ids, ideally a range). Never estimate a quantity the Engine can compute. Prefer compute_from_evidence for PE, market_cap and YoY rather than authoring a calculation AST. Prose uses the task output_language.",
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
