//! Test doubles: a scripted [`ChatBackend`] and a counting echo tool.
//!
//! Compiled into the library (not `cfg(test)`) so integration tests under
//! `tests/` can use them too. They have no extra dependencies.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use futures::StreamExt;
use serde_json::{json, Value};

use astock_market_data::DataProvider;
use astock_minimax::{
    ChatChoice, ChatChunk, ChatMessage, ChatRequest, MinimaxError, ToolCall, ToolCallFunction,
};

use crate::backend::{ChatBackend, ChatChunkStream};
use crate::error::Result;
use crate::tools::{AgentTool, ToolContext, ToolResult};

/// A `DataProvider` that serves nothing (all methods return `NoProvider`).
/// For tests whose tools never touch the market.
pub struct NoopMarket;

#[async_trait]
impl DataProvider for NoopMarket {
    fn name(&self) -> &'static str {
        "noop"
    }
}

/// One scripted reply to a chat request.
pub enum ScriptedReply {
    /// Chunks streamed back in order.
    Chunks(Vec<ChatChunk>),
    /// Stream establishment fails with this error (e.g. QuotaExhausted).
    Error(MinimaxError),
}

/// A fake [`ChatBackend`] that replays a script of replies, one per call.
pub struct ScriptedChat {
    model: String,
    script: Mutex<VecDeque<ScriptedReply>>,
    /// Every request received, for assertions.
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl ScriptedChat {
    /// An empty scripted backend reporting `model`.
    pub fn new(model: &str) -> Self {
        ScriptedChat {
            model: model.to_string(),
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Append a reply to the script.
    pub fn push(&self, reply: ScriptedReply) -> &Self {
        self.script.lock().unwrap().push_back(reply);
        self
    }

    /// Script a plain-text answer (streamed as one chunk).
    pub fn push_text(&self, text: &str) -> &Self {
        self.push(ScriptedReply::Chunks(vec![content_chunk(text, "stop")]))
    }

    /// Script a tool-call round. `arguments` must be a JSON value; it is
    /// serialized into the `arguments` string and split across two chunks to
    /// exercise the fragment-merge logic.
    pub fn push_tool_call(&self, id: &str, name: &str, arguments: Value) -> &Self {
        let args = arguments.to_string();
        let split = args.len() / 2;
        let (first, second) = args.split_at(split);
        let chunks = vec![
            tool_call_chunk(ToolCall {
                id: Some(id.to_string()),
                kind: Some("function".to_string()),
                index: Some(0),
                function: Some(ToolCallFunction {
                    name: Some(name.to_string()),
                    arguments: Some(first.to_string()),
                }),
            }),
            tool_call_chunk(ToolCall {
                id: None,
                kind: None,
                index: Some(0),
                function: Some(ToolCallFunction {
                    name: None,
                    arguments: Some(second.to_string()),
                }),
            }),
        ];
        self.push(ScriptedReply::Chunks(chunks))
    }

    /// Script a quota-exhausted failure at stream establishment.
    pub fn push_quota_exhausted(&self) -> &Self {
        self.push(ScriptedReply::Error(MinimaxError::QuotaExhausted {
            window_reset_at: Some(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
            ),
        }))
    }
}

fn content_chunk(text: &str, finish: &str) -> ChatChunk {
    ChatChunk {
        choices: vec![ChatChoice {
            index: Some(0),
            delta: Some(ChatMessage {
                role: "assistant".to_string(),
                content: Some(Value::String(text.to_string())),
                ..Default::default()
            }),
            finish_reason: Some(finish.to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn tool_call_chunk(call: ToolCall) -> ChatChunk {
    let last = call
        .function
        .as_ref()
        .and_then(|f| f.name.as_ref())
        .is_none();
    ChatChunk {
        choices: vec![ChatChoice {
            index: Some(0),
            delta: Some(ChatMessage {
                role: "assistant".to_string(),
                tool_calls: Some(vec![call]),
                ..Default::default()
            }),
            finish_reason: last.then(|| "tool_calls".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[async_trait]
impl ChatBackend for ScriptedChat {
    async fn selected_model(&self) -> std::result::Result<String, MinimaxError> {
        Ok(self.model.clone())
    }

    async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> std::result::Result<ChatChunkStream, MinimaxError> {
        self.requests.lock().unwrap().push(request.clone());
        match self.script.lock().unwrap().pop_front() {
            Some(ScriptedReply::Chunks(chunks)) => {
                Ok(stream::iter(chunks.into_iter().map(Ok)).boxed())
            }
            Some(ScriptedReply::Error(e)) => Err(e),
            None => Err(MinimaxError::Api {
                code: -1,
                msg: "scripted chat: script exhausted".to_string(),
            }),
        }
    }
}

/// A trivial tool that echoes its `text` argument; counts executions so
/// tests can assert results are replayed, not re-executed.
pub struct EchoTool {
    /// Number of times `execute` ran.
    pub calls: Arc<AtomicUsize>,
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EchoTool {
    /// A fresh echo tool with a zeroed counter.
    pub fn new() -> Self {
        EchoTool {
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl AgentTool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "回显输入文本（测试用）"
    }
    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok(ToolResult {
            summary_json: json!({"echo": text}),
            full_json: None,
            cache_key: String::new(),
            source: "test".to_string(),
            fetched_at: "2026-01-01T00:00:00Z".to_string(),
        })
    }
}
