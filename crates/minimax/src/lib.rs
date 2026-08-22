//! MiniMax AI provider for the A-share analysis terminal.
//!
//! Covers both MiniMax services (mainland China `minimaxi.com` and
//! international `minimax.io`):
//!
//! - secure API key storage in the OS keyring with log-safe redaction
//!   ([`KeyStore`], [`SecretKey`], [`redact`]);
//! - region/endpoint auto-detection via the Token Plan endpoint
//!   ([`RegionDetector`], [`MinimaxClient::detect_service`]);
//! - typed Token Plan quota with throttle/pacing hints ([`QuotaStatus`]);
//! - a configurable model fallback chain ([`ModelCatalog`]);
//! - OpenAI-compatible chat with streaming SSE and tool calling
//!   ([`MinimaxClient::chat`], [`MinimaxClient::chat_stream`], [`chat`]);
//! - quota-aware scheduling with jittered backoff ([`RateGate`]) and a
//!   pre-flight quota guard that fails with
//!   [`MinimaxError::QuotaExhausted`] before burning requests on an exhausted
//!   window.
//!
//! All diagnostics go through `tracing` and never include key material.

pub mod chat;
mod client;
pub mod error;
pub mod http;
pub mod key;
pub mod models;
pub mod quota;
pub mod rate_gate;
pub mod region;

pub use chat::{
    split_reasoning, BaseResp, ChatChoice, ChatChunk, ChatMessage, ChatRequest, ChatResponse,
    ChatStream, FunctionSpec, ToolCall, ToolCallFunction, ToolSpec, Usage,
};
pub use client::MinimaxClient;
pub use error::MinimaxError;
pub use http::{Http, HttpResponse, ReqwestHttp};
pub use key::{mask_key, redact, KeyStore, SecretKey};
pub use models::{ModelCatalog, DEFAULT_CHAIN};
pub use quota::{ModelQuota, Pacing, QuotaStatus, THROTTLE_PERCENT};
pub use rate_gate::{RateGate, RateGateConfig};
pub use region::{Region, RegionDetector, ServiceInfo};
