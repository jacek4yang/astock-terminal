use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use astock_minimax::{
    ChatMessage, ChatRequest, MinimaxClient, MinimaxError, ToolCall, ToolCallFunction, ToolSpec,
};

use crate::error::{ProviderError, ProviderErrorKind};
use crate::model::{
    Message, MessageRole, ModelChunk, ModelProvider, ModelRequest, ModelStream, ModelToolCall,
};

pub struct MinimaxProvider {
    client: Arc<MinimaxClient>,
}

impl MinimaxProvider {
    pub fn new(client: MinimaxClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}

#[async_trait]
impl ModelProvider for MinimaxProvider {
    fn name(&self) -> &'static str {
        "minimax"
    }

    async fn selected_model(&self) -> Result<String, ProviderError> {
        self.client.selected_model().await.map_err(map_error)
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        let messages = request.messages.iter().map(to_minimax_message).collect();
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                ToolSpec::function(
                    tool.name.clone(),
                    tool.description.clone(),
                    tool.input_schema.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut chat = ChatRequest::new(request.model, messages)
            .with_max_tokens(request.max_tokens)
            .with_temperature(request.temperature);
        if !tools.is_empty() {
            chat = chat.with_tools(tools);
        }
        // Keep private reasoning separate from visible content whenever the
        // selected MiniMax endpoint supports the extension.
        chat.extra
            .insert("reasoning_split".into(), serde_json::Value::Bool(true));
        let stream = self.client.chat_stream(&chat).await.map_err(map_error)?;
        let state = AdapterState {
            stream: Box::pin(stream),
            pending: VecDeque::new(),
            visibility: VisibilityFilter::default(),
            done: false,
        };
        Ok(Box::pin(futures::stream::unfold(
            state,
            |mut state| async move {
                loop {
                    if let Some(item) = state.pending.pop_front() {
                        return Some((item, state));
                    }
                    if state.done {
                        return None;
                    }
                    match state.stream.next().await {
                        Some(Ok(chunk)) => {
                            if let Some(raw) = chunk.raw_delta() {
                                let visible = state.visibility.push(&raw);
                                if !visible.is_empty() {
                                    state.pending.push_back(Ok(ModelChunk::TextDelta(visible)));
                                }
                            }
                            for (position, call) in chunk.tool_calls().iter().enumerate() {
                                state.pending.push_back(Ok(ModelChunk::ToolCallDelta {
                                    index: call.index.unwrap_or(position as u32),
                                    id: call.id.clone(),
                                    name: call
                                        .function
                                        .as_ref()
                                        .and_then(|value| value.name.clone()),
                                    arguments: call
                                        .function
                                        .as_ref()
                                        .and_then(|value| value.arguments.clone()),
                                }));
                            }
                            if let Some(reason) = chunk.finish_reason() {
                                state.pending.push_back(Ok(ModelChunk::Finished {
                                    reason: Some(reason.to_owned()),
                                }));
                            }
                        }
                        Some(Err(error)) => return Some((Err(map_error(error)), state)),
                        None => {
                            let tail = state.visibility.finish();
                            if !tail.is_empty() {
                                state.pending.push_back(Ok(ModelChunk::TextDelta(tail)));
                            }
                            state.done = true;
                        }
                    }
                }
            },
        )))
    }

    async fn quota(&self) -> Result<Option<serde_json::Value>, ProviderError> {
        self.client
            .quota()
            .await
            .map(|quota| serde_json::to_value(quota).ok())
            .map_err(map_error)
    }
}

type MinimaxStream = std::pin::Pin<
    Box<dyn futures::Stream<Item = Result<astock_minimax::ChatChunk, MinimaxError>> + Send>,
>;

struct AdapterState {
    stream: MinimaxStream,
    pending: VecDeque<Result<ModelChunk, ProviderError>>,
    visibility: VisibilityFilter,
    done: bool,
}

fn to_minimax_message(message: &Message) -> ChatMessage {
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    ChatMessage {
        role: role.into(),
        content: (!message.content.is_empty())
            .then(|| serde_json::Value::String(message.content.clone())),
        tool_calls: (!message.tool_calls.is_empty()).then(|| {
            message
                .tool_calls
                .iter()
                .map(to_minimax_tool_call)
                .collect()
        }),
        tool_call_id: message.tool_call_id.clone(),
        ..Default::default()
    }
}

fn to_minimax_tool_call(call: &ModelToolCall) -> ToolCall {
    ToolCall {
        id: Some(call.id.clone()),
        kind: Some("function".into()),
        index: Some(call.index),
        function: Some(ToolCallFunction {
            name: Some(call.name.clone()),
            arguments: Some(call.arguments.clone()),
        }),
    }
}

fn map_error(error: MinimaxError) -> ProviderError {
    match error {
        MinimaxError::Auth(message) => {
            ProviderError::new(ProviderErrorKind::Authentication, message, false)
        }
        MinimaxError::RateLimited { retry_after } => ProviderError {
            kind: ProviderErrorKind::RateLimited,
            message: "MiniMax rate limit reached".into(),
            retryable: true,
            retry_after,
        },
        MinimaxError::QuotaExhausted { .. } => ProviderError::new(
            ProviderErrorKind::Quota,
            "MiniMax Token Plan quota is exhausted",
            true,
        ),
        MinimaxError::Network(message) => {
            ProviderError::new(ProviderErrorKind::Network, message, true)
        }
        MinimaxError::Parse(message) => {
            ProviderError::new(ProviderErrorKind::MalformedResponse, message, false)
        }
        MinimaxError::Api { code, msg } => ProviderError::new(
            ProviderErrorKind::Unavailable,
            format!("MiniMax API {code}: {msg}"),
            code >= 500,
        ),
        MinimaxError::KeyStore(message) => {
            ProviderError::new(ProviderErrorKind::Unavailable, message, false)
        }
    }
}

#[derive(Default)]
struct VisibilityFilter {
    pending: String,
    inside_reasoning: bool,
}

impl VisibilityFilter {
    fn push(&mut self, text: &str) -> String {
        const OPEN: &str = "<think>";
        const CLOSE: &str = "</think>";
        self.pending.push_str(text);
        let mut visible = String::new();
        loop {
            if self.inside_reasoning {
                if let Some(end) = self.pending.find(CLOSE) {
                    self.pending.drain(..end + CLOSE.len());
                    self.inside_reasoning = false;
                    continue;
                }
                let keep = suffix_prefix_len(&self.pending, CLOSE);
                let discard = self.pending.len().saturating_sub(keep);
                self.pending.drain(..discard);
                break;
            }
            if let Some(start) = self.pending.find(OPEN) {
                visible.push_str(&self.pending[..start]);
                self.pending.drain(..start + OPEN.len());
                self.inside_reasoning = true;
                continue;
            }
            let keep = suffix_prefix_len(&self.pending, OPEN);
            let emit = self.pending.len().saturating_sub(keep);
            visible.push_str(&self.pending[..emit]);
            self.pending.drain(..emit);
            break;
        }
        visible
    }

    fn finish(&mut self) -> String {
        if self.inside_reasoning {
            self.pending.clear();
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

fn suffix_prefix_len(text: &str, marker: &str) -> usize {
    (1..marker.len())
        .rev()
        .find(|length| text.ends_with(&marker[..*length]))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::VisibilityFilter;

    #[test]
    fn fragmented_private_reasoning_never_becomes_visible() {
        let mut filter = VisibilityFilter::default();
        assert_eq!(filter.push("公开<th"), "公开");
        assert_eq!(filter.push("ink>秘密</thi"), "");
        assert_eq!(filter.push("nk>结论"), "结论");
        assert_eq!(filter.finish(), "");
    }
}
