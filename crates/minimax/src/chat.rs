//! OpenAI-compatible chat types plus a hand-rolled SSE parser for streaming.
//!
//! MiniMax speaks the OpenAI chat-completions schema and adds a `base_resp`
//! envelope. MiniMax can either inline thinking in `<think>` blocks or, with
//! `reasoning_split=true`, return it separately in `reasoning_content` and
//! `reasoning_details`. Both representations are preserved for protocol-safe
//! multi-turn tool use while callers only render regular `content`.

use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::error::MinimaxError;
use crate::http::{map_base_resp, ByteStream};

/// `base_resp` envelope present on every MiniMax response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BaseResp {
    /// `0` means success; `2049` means invalid api key.
    #[serde(default)]
    pub status_code: i64,
    /// Human-readable status message.
    #[serde(default)]
    pub status_msg: String,
}

/// A chat message. `content` is kept as raw JSON to tolerate both plain
/// strings and structured (multi-part) content; use the constructors and
/// [`ChatMessage::text`] to avoid touching that directly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `system`, `user`, `assistant` or `tool`. Optional on the wire because
    /// streaming deltas after the first chunk omit it.
    #[serde(default)]
    pub role: String,
    /// Message content: a JSON string, or an array of content parts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    /// MiniMax separated reasoning text. It must be replayed unchanged in a
    /// multi-turn tool-call conversation, but is never user-visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// MiniMax interleaved-thinking details. Kept as raw JSON so newly added
    /// detail kinds remain forward compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<serde_json::Value>,
    /// Tool calls requested by the assistant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// For `role == "tool"`: id of the tool call being answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional participant name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// Build a plain-text message.
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(serde_json::Value::String(content.into())),
            ..Default::default()
        }
    }

    /// A system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self::text("system", content)
    }

    /// A user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self::text("user", content)
    }

    /// An assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text("assistant", content)
    }

    /// A tool-result message answering the given tool call.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(serde_json::Value::String(content.into())),
            tool_call_id: Some(tool_call_id.into()),
            ..Default::default()
        }
    }

    /// Content as plain text. String content is returned as-is; structured
    /// parts contribute their `text` fields concatenated.
    pub fn content_text(&self) -> Option<String> {
        content_to_text(self.content.as_ref()?)
    }
}

/// A tool call requested by the model (OpenAI `function` shape, all fields
/// optional for tolerance against schema drift).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    /// Server-assigned id; must be echoed back in the tool-result message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Usually `"function"`.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Streaming index of this tool call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// Function name plus JSON-encoded arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunction>,
}

/// Function payload of a [`ToolCall`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallFunction {
    /// Function name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Arguments as a JSON-encoded string (may arrive fragmented when streaming).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// A tool offered to the model (OpenAI function-calling schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Always `"function"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Function definition.
    pub function: FunctionSpec,
}

impl ToolSpec {
    /// A function tool with a JSON Schema `parameters` value.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            function: FunctionSpec {
                name: name.into(),
                description: Some(description.into()),
                parameters,
            },
        }
    }
}

/// Function definition inside a [`ToolSpec`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSpec {
    /// Function name.
    pub name: String,
    /// What the function does, shown to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// A chat completion request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatRequest {
    /// Model name, e.g. `MiniMax-M2.5`.
    pub model: String,
    /// Conversation so far.
    pub messages: Vec<ChatMessage>,
    /// Tools the model may call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSpec>>,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Maximum tokens to generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Whether to stream (set automatically by the streaming client method).
    #[serde(default)]
    pub stream: bool,
    /// Extra OpenAI-compatible fields (`top_p`, `stop`, ...) passed through.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ChatRequest {
    /// A request for `model` with the given messages.
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            ..Default::default()
        }
    }

    /// Attach tools.
    pub fn with_tools(mut self, tools: Vec<ToolSpec>) -> Self {
        self.tools = Some(tools);
        self
    }

    /// Set `max_tokens`.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Set `temperature`.
    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }
}

/// Token usage counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens.
    #[serde(default)]
    pub prompt_tokens: Option<u64>,
    /// Completion tokens.
    #[serde(default)]
    pub completion_tokens: Option<u64>,
    /// Total tokens.
    #[serde(default)]
    pub total_tokens: Option<u64>,
}

/// One completion choice. Non-streaming responses fill `message`, streaming
/// chunks fill `delta`; both are optional so one type serves both flows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatChoice {
    /// Choice index.
    #[serde(default)]
    pub index: Option<u32>,
    /// Full message (non-streaming).
    #[serde(default)]
    pub message: Option<ChatMessage>,
    /// Incremental message (streaming).
    #[serde(default)]
    pub delta: Option<ChatMessage>,
    /// Why generation stopped (`stop`, `tool_calls`, `length`, ...).
    #[serde(default)]
    pub finish_reason: Option<String>,
}

/// A chat completion response; also used per-chunk when streaming.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatResponse {
    /// Response id.
    #[serde(default)]
    pub id: Option<String>,
    /// Completion choices.
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
    /// Token usage, when reported.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// MiniMax status envelope.
    #[serde(default)]
    pub base_resp: Option<BaseResp>,
}

/// One server-sent event chunk of a streaming completion.
pub type ChatChunk = ChatResponse;

impl ChatResponse {
    /// Fail when the `base_resp` envelope reports a non-zero status.
    pub(crate) fn check_base_resp(&self) -> Result<(), MinimaxError> {
        if let Some(base) = &self.base_resp {
            if base.status_code != 0 {
                return Err(map_base_resp(base.status_code, &base.status_msg));
            }
        }
        Ok(())
    }

    /// Raw content of the first choice's message (non-streaming).
    pub fn raw_content(&self) -> Option<String> {
        self.choices.first()?.message.as_ref()?.content_text()
    }

    /// Raw content of the first choice's delta (streaming).
    pub fn raw_delta(&self) -> Option<String> {
        self.choices.first()?.delta.as_ref()?.content_text()
    }

    /// User-facing text of the first choice, with `<think>` blocks removed.
    pub fn text(&self) -> Option<String> {
        let raw = self.raw_content().or_else(|| self.raw_delta())?;
        let (_, content) = split_reasoning(&raw);
        Some(content)
    }

    /// Chain-of-thought extracted from `<think>` blocks, when present.
    pub fn reasoning(&self) -> Option<String> {
        let raw = self.raw_content().or_else(|| self.raw_delta())?;
        split_reasoning(&raw).0
    }

    /// Tool calls requested in the first choice, if any.
    pub fn tool_calls(&self) -> &[ToolCall] {
        self.choices
            .first()
            .and_then(|c| c.message.as_ref().or(c.delta.as_ref()))
            .and_then(|m| m.tool_calls.as_deref())
            .unwrap_or(&[])
    }

    /// `finish_reason` of the first choice, when set.
    pub fn finish_reason(&self) -> Option<&str> {
        self.choices.first()?.finish_reason.as_deref()
    }
}

/// Split `<think>...</think>` blocks out of `text`.
///
/// Returns `(reasoning, content)`: concatenated think-block contents (or
/// `None`) and the remaining user-facing text. An unterminated `<think>`
/// swallows the rest of the text as reasoning, matching how reasoning models
/// stream before closing the block.
pub fn split_reasoning(text: &str) -> (Option<String>, String) {
    let mut reasoning = String::new();
    let mut content = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<think>") {
        content.push_str(&rest[..start]);
        let after = &rest[start + "<think>".len()..];
        match after.find("</think>") {
            Some(end) => {
                reasoning.push_str(&after[..end]);
                rest = &after[end + "</think>".len()..];
            }
            None => {
                reasoning.push_str(after);
                rest = "";
            }
        }
    }
    content.push_str(rest);
    let reasoning = reasoning.trim().to_string();
    let content = content.trim().to_string();
    (
        if reasoning.is_empty() {
            None
        } else {
            Some(reasoning)
        },
        content,
    )
}

fn content_to_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
                .collect();
            Some(text)
        }
        _ => None,
    }
}

/// One parsed SSE event.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SseEvent {
    /// Payload of a `data:` event (multi-line data joined with `\n`).
    Data(String),
    /// The terminal `data: [DONE]` sentinel.
    Done,
}

/// Incremental SSE frame parser. Feed arbitrary byte slices; complete events
/// come back. Tolerates CRLF, comment lines and data split across chunks.
#[derive(Debug, Default)]
struct SseParser {
    buf: Vec<u8>,
    data_lines: Vec<String>,
}

impl SseParser {
    fn new() -> Self {
        Self::default()
    }

    fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // the '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.process_line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// Flush any trailing line/event not terminated by a blank line (EOF).
    fn finish(&mut self) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if !self.buf.is_empty() {
            let mut line = std::mem::take(&mut self.buf);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.process_line(&line) {
                events.push(event);
            }
        }
        if let Some(event) = self.dispatch() {
            events.push(event);
        }
        events
    }

    fn process_line(&mut self, line: &[u8]) -> Option<SseEvent> {
        if line.is_empty() {
            return self.dispatch();
        }
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            self.data_lines
                .push(String::from_utf8_lossy(data).into_owned());
        }
        // Anything else (`event:`, `id:`, `:` comments) is ignored.
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        if self.data_lines.is_empty() {
            return None;
        }
        let payload = std::mem::take(&mut self.data_lines).join("\n");
        if payload.trim() == "[DONE]" {
            Some(SseEvent::Done)
        } else {
            Some(SseEvent::Data(payload))
        }
    }
}

/// A stream of [`ChatChunk`]s parsed from an SSE byte stream.
///
/// Yields `Err` items for malformed event payloads and transport errors, then
/// terminates; it does not retry mid-stream (retries happen around stream
/// establishment only).
pub struct ChatStream {
    inner: ByteStream,
    parser: SseParser,
    pending: VecDeque<Result<ChatChunk, MinimaxError>>,
    terminated: bool,
}

impl ChatStream {
    /// Wrap a raw byte stream.
    pub fn from_byte_stream(inner: ByteStream) -> Self {
        Self {
            inner,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            terminated: false,
        }
    }

    fn push_events(&mut self, events: Vec<SseEvent>) {
        for event in events {
            match event {
                SseEvent::Data(payload) => self.pending.push_back(parse_chunk(&payload)),
                SseEvent::Done => self.terminated = true,
            }
        }
    }
}

fn parse_chunk(payload: &str) -> Result<ChatChunk, MinimaxError> {
    serde_json::from_str(payload)
        .map_err(|e| MinimaxError::Parse(format!("sse chunk: {e}: {payload:.120}")))
}

impl Stream for ChatStream {
    type Item = Result<ChatChunk, MinimaxError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Poll::Ready(Some(item));
            }
            if self.terminated {
                return Poll::Ready(None);
            }
            match self.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let events = self.parser.feed(&bytes);
                    self.push_events(events);
                }
                Poll::Ready(Some(Err(e))) => {
                    self.terminated = true;
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(None) => {
                    self.terminated = true;
                    let events = self.parser.finish();
                    self.push_events(events);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_payloads(events: Vec<SseEvent>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                SseEvent::Data(p) => Some(p),
                SseEvent::Done => None,
            })
            .collect()
    }

    #[test]
    fn parses_event_split_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: {\"cho").is_empty());
        assert!(p.feed(b"ices\":[]}").is_empty());
        let events = p.feed(b"\n\n");
        assert_eq!(data_payloads(events), vec!["{\"choices\":[]}"]);
    }

    #[test]
    fn handles_done_sentinel() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: {\"a\":1}\n\ndata: [DONE]\n\ndata: ignored\n\n");
        assert_eq!(events[0], SseEvent::Data("{\"a\":1}".to_string()));
        assert_eq!(events[1], SseEvent::Done);
        // Parser itself still yields later events; ChatStream stops at Done.
        assert_eq!(events[2], SseEvent::Data("ignored".to_string()));
    }

    #[test]
    fn joins_multi_line_data() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: line1\ndata:line2\ndata:  line3\n\n");
        assert_eq!(data_payloads(events), vec!["line1\nline2\n line3"]);
    }

    #[test]
    fn tolerates_crlf_comments_and_other_fields() {
        let mut p = SseParser::new();
        let events = p.feed(b": keep-alive\r\nevent: message\r\nid: 7\r\ndata: hello\r\n\r\n");
        assert_eq!(data_payloads(events), vec!["hello"]);
    }

    #[test]
    fn finish_flushes_unterminated_event() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: tail").is_empty());
        assert_eq!(data_payloads(p.finish()), vec!["tail"]);
    }

    #[test]
    fn chat_stream_yields_chunks_until_done() {
        let body: &[u8] = b"data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"<think>ponder</think>hi\"}}]}\n\ndata: [DONE]\n\n";
        let stream = futures::stream::iter(vec![Ok::<_, MinimaxError>(body.to_vec())]);
        let mut chat = ChatStream::from_byte_stream(Box::pin(stream));
        let chunks: Vec<ChatChunk> = futures::executor::block_on(async {
            let mut out = Vec::new();
            while let Some(item) = futures::StreamExt::next(&mut chat).await {
                out.push(item.unwrap());
            }
            out
        });
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].reasoning().as_deref(), Some("ponder"));
        assert_eq!(chunks[0].text().as_deref(), Some("hi"));
    }

    #[test]
    fn chat_stream_surfaces_parse_errors() {
        let body: &[u8] = b"data: {not json}\n\n";
        let stream = futures::stream::iter(vec![Ok::<_, MinimaxError>(body.to_vec())]);
        let mut chat = ChatStream::from_byte_stream(Box::pin(stream));
        let item = futures::executor::block_on(futures::StreamExt::next(&mut chat));
        assert!(matches!(item, Some(Err(MinimaxError::Parse(_)))));
    }

    #[test]
    fn split_reasoning_extracts_think_blocks() {
        let (reasoning, content) = split_reasoning("<think>a</think>answer");
        assert_eq!(reasoning.as_deref(), Some("a"));
        assert_eq!(content, "answer");

        let (reasoning, content) = split_reasoning("no reasoning here");
        assert_eq!(reasoning, None);
        assert_eq!(content, "no reasoning here");

        let (reasoning, content) = split_reasoning("<think>unterminated");
        assert_eq!(reasoning.as_deref(), Some("unterminated"));
        assert_eq!(content, "");
    }

    #[test]
    fn tool_call_response_parses() {
        let body = r#"{
            "id": "abc",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_quote", "arguments": "{\"symbol\":\"600519\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
            "base_resp": {"status_code": 0, "status_msg": "success"}
        }"#;
        let resp: ChatResponse = serde_json::from_str(body).unwrap();
        resp.check_base_resp().unwrap();
        let calls = resp.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(
            calls[0].function.as_ref().unwrap().name.as_deref(),
            Some("get_quote")
        );
        assert_eq!(resp.finish_reason(), Some("tool_calls"));
        assert_eq!(resp.usage.unwrap().total_tokens, Some(15));
    }
}
