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
    ];
    for tool in tools {
        registry
            .register(tool)
            .expect("the built-in tool registry is valid");
    }
    registry
}
