//! hexin-v cookie signing without node.js: akshare's reverse-engineered
//! `ths.js` (2019, hardcoded `TOKEN_SERVER_TIME`) evaluated inside an
//! embedded QuickJS engine (`rquickjs`, pure C, MSVC-friendly).
//!
//! The generated token is used both as the `v` cookie and as the `hexin-v`
//! request header, mirroring akshare/pywencai. Server-side acceptance of
//! this 2019 algorithm was re-verified live on 2026-08-21 (THS still
//! accepts the token; iwencai then challenges with a slider captcha).

use crate::error::WencaiError;

/// akshare's vendored `akshare/data/ths.js` (39 KB, unmodified).
const THS_JS: &str = include_str!("ths.js");

/// Compute a fresh hexin-v token by evaluating `v()` from `ths.js`.
///
/// Each call spins up a throwaway QuickJS runtime; this takes a few
/// milliseconds and keeps the API free of `Send`/lifetime headaches
/// (QuickJS runtimes are not `Send`).
pub fn hexin_v() -> Result<String, WencaiError> {
    let runtime = rquickjs::Runtime::new().map_err(|e| WencaiError::Js(e.to_string()))?;
    let context = rquickjs::Context::full(&runtime).map_err(|e| WencaiError::Js(e.to_string()))?;
    context.with(|ctx| {
        // ths.js is 2019 obfuscated sloppy-mode code: it assigns implicit
        // globals (e.g. `BROWSER_LIST = {...}`) inside function bodies.
        // rquickjs 0.12 evals with `strict: true` by default, which makes
        // those assignments throw ReferenceError — force sloppy mode.
        let mut options = rquickjs::context::EvalOptions::default();
        options.strict = false;
        ctx.eval_with_options::<(), _>(THS_JS, options)
            .map_err(|e| WencaiError::Js(format!("ths.js load: {e}")))?;
        // Route JS exceptions back as strings so the message survives.
        let token: String = ctx
            .eval("try { String(v()) } catch (e) { 'JSERR:' + e + '\\n' + (e && e.stack || '') }")
            .map_err(|e| WencaiError::Js(format!("v() call: {e}")))?;
        if let Some(detail) = token.strip_prefix("JSERR:") {
            return Err(WencaiError::Js(format!("v() threw: {detail}")));
        }
        Ok(token)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_plausible_token(token: &str) {
        // Observed tokens (node + QuickJS) are ~48 chars from a URL-safe
        // base64-ish alphabet; accept a generous band to avoid brittle tests.
        assert!(
            (32..=64).contains(&token.len()),
            "unexpected token length {}: {token:?}",
            token.len()
        );
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=')),
            "unexpected token charset: {token:?}"
        );
    }

    #[test]
    fn generates_plausible_token() {
        let token = hexin_v().expect("hexin_v should succeed");
        assert_plausible_token(&token);
    }

    #[test]
    fn back_to_back_tokens_remain_valid() {
        // The vendored signer includes wall-clock state, but two executions
        // within the same timer tick are allowed to return the same token.
        // Equality is not part of the protocol contract; validity is.
        let first = hexin_v().expect("first hexin_v should succeed");
        let second = hexin_v().expect("second hexin_v should succeed");
        assert_plausible_token(&first);
        assert_plausible_token(&second);
    }
}
