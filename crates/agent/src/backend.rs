//! Chat backend seam: the orchestrator talks to the model through this
//! trait, so tests can substitute a scripted fake for [`MinimaxClient`].

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;

use astock_minimax::{ChatChunk, ChatRequest, MinimaxClient, MinimaxError};

/// A boxed stream of chat chunks, as produced by [`ChatBackend::chat_stream`].
pub type ChatChunkStream = BoxStream<'static, Result<ChatChunk, MinimaxError>>;

/// What the orchestrator needs from the LLM provider: model selection and a
/// streaming chat completion. Implemented by [`MinimaxClient`] in production
/// and by scripted fakes in tests.
#[async_trait]
pub trait ChatBackend: Send + Sync {
    /// The model to use when the task/config does not pin one.
    async fn selected_model(&self) -> Result<String, MinimaxError>;

    /// Streaming chat completion. `request.stream` is forced to `true` by the
    /// implementation; mid-stream failures surface as `Err` items.
    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, MinimaxError>;
}

#[async_trait]
impl ChatBackend for MinimaxClient {
    async fn selected_model(&self) -> Result<String, MinimaxError> {
        MinimaxClient::selected_model(self).await
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, MinimaxError> {
        let stream = MinimaxClient::chat_stream(self, request).await?;
        Ok(stream.boxed())
    }
}

/// Lets a shared `Arc<T>` (e.g. the app-wide MiniMax client) be handed to
/// the engine as `Arc<dyn ChatBackend>` without wrapping.
#[async_trait]
impl<T: ChatBackend + ?Sized> ChatBackend for std::sync::Arc<T> {
    async fn selected_model(&self) -> Result<String, MinimaxError> {
        (**self).selected_model().await
    }

    async fn chat_stream(&self, request: &ChatRequest) -> Result<ChatChunkStream, MinimaxError> {
        (**self).chat_stream(request).await
    }
}
