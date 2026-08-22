//! Optional iwencai (同花顺问财) natural-language A-share screening provider.
//!
//! Self-contained: the `hexin-v` anti-bot token is signed by an embedded
//! QuickJS engine running akshare's reverse-engineered `ths.js` (no
//! node.js), and iwencai's slider captcha can be solved automatically when
//! the `captcha` feature is enabled (pure-Rust `ddddocr-tract`, no ONNX
//! Runtime).
//!
//! The crate is strictly optional — nothing else in the workspace depends
//! on it, and iwencai may challenge (captcha), rate-limit, or change its
//! schema at any time. Callers should treat every query as best-effort.

pub mod error;
mod hexin;
mod pace;
mod wencai;

#[cfg(feature = "captcha")]
mod captcha;

pub use error::WencaiError;
pub use hexin::hexin_v;
pub use wencai::{WencaiClient, WencaiResult, WencaiRow};
