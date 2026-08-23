//! Security boundary for external research content.
//!
//! Search snippets, HTML, PDFs, announcements and news are untrusted data.
//! This crate centralizes SSRF prevention, bounded fetching, prompt-injection
//! signals, secret redaction and tool permission domains so individual data
//! providers cannot silently weaken those controls.

mod content;
mod fetch;
mod permission;
mod redact;
mod url_policy;

pub use content::{inspect_external_text, InjectionFinding, UntrustedExternalText};
pub use fetch::{SafeFetchError, SafeFetchLimits, SafeFetchResult, SafeFetcher};
pub use permission::{authorize_tool, InvocationOrigin, ToolPermissionDomain};
pub use redact::{fingerprint_json, redact_json, redact_text};
pub use url_policy::{ResolvedSafeUrl, SafeUrl, UrlSecurityError, UrlSecurityPolicy};
