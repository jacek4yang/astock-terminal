//! Web login, session liveness probe and slider-captcha fallback.
//!
//! Protocol facts (docs/data-source-joinquant-v2.md §1):
//!
//! - `POST /user/login/doLoginByText`, form fields `username` / `pwd`
//!   (NOT `CyLoginForm[...]` — that is the login-dialog bundle's field set
//!   and fails with "用户不存在或密码错误"). Password goes plaintext over
//!   HTTPS, no client-side hashing.
//! - Response `code`: `"00000"` success (string), `"10000"` field error,
//!   `"20000"` generic failure, `105` (number) → slider captcha required.
//! - Captcha: `POST /common/verifyCode/captchar` → base64 data-URI images
//!   (`bgImg` background with notch, `hqImg` puzzle piece); then
//!   `POST /common/verifyCode/validate` with the single scalar `axisX`
//!   (no trajectory upload). On success the returned `token` is replayed as
//!   `valideCode` on the login request.

use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use reqwest::Client;
use serde_json::Value;

use crate::error::JoinQuantError;
use crate::Credentials;

/// Site origin shared by the web app, the hub and the notebook server.
pub(crate) const BASE: &str = "https://www.joinquant.com";

/// Desktop Chrome UA used for all requests (doc §4.3).
pub(crate) const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Bounded captcha retries (doc §4.3: N = 5).
const CAPTCHA_MAX_ATTEMPTS: usize = 5;

/// Outcome of a login attempt, parsed from the response JSON.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LoginOutcome {
    /// `code == "00000"`.
    Success,
    /// `code == 105` — slider captcha required.
    NeedCaptcha,
    /// Any other code.
    Failed {
        /// Server error code.
        code: String,
        /// Server message.
        message: String,
    },
}

/// Parse the `doLoginByText` response. `code` is a string on success
/// (`"00000"`) but a number (`105`) when the captcha triggers — handle both.
pub(crate) fn parse_login_response(v: &Value) -> LoginOutcome {
    let code = match v.get("code") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    match code.as_str() {
        "00000" => LoginOutcome::Success,
        "105" => LoginOutcome::NeedCaptcha,
        other => {
            let message = v
                .get("msg")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| v.get("error").map(ToString::to_string))
                .unwrap_or_else(|| "unknown login error".to_string());
            LoginOutcome::Failed {
                code: other.to_string(),
                message,
            }
        }
    }
}

/// One `doLoginByText` round; `valide_code` is attached after a passed
/// slider validation.
pub(crate) async fn do_login(
    http: &Client,
    creds: &Credentials,
    valide_code: Option<&str>,
) -> Result<LoginOutcome, JoinQuantError> {
    let mut form = vec![
        ("username", creds.username.as_str()),
        ("pwd", creds.password.as_str()),
    ];
    if let Some(vc) = valide_code {
        form.push(("valideCode", vc));
    }
    let resp: Value = http
        .post(format!("{BASE}/user/login/doLoginByText"))
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&form)
        .send()
        .await?
        .json()
        .await?;
    Ok(parse_login_response(&resp))
}

/// Session liveness probe: `GET /user/index/isLogin` → `data.isLogin == 1`.
pub(crate) async fn is_logged_in(http: &Client) -> Result<bool, JoinQuantError> {
    let resp: Value = http
        .get(format!("{BASE}/user/index/isLogin"))
        .header("X-Requested-With", "XMLHttpRequest")
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.pointer("/data/isLogin").and_then(Value::as_i64) == Some(1))
}

/// Decode a base64 data-URI (`data:image/png;base64,....`) into raw bytes.
pub(crate) fn decode_data_uri(s: &str) -> Result<Vec<u8>, JoinQuantError> {
    let (_, payload) = s
        .split_once(',')
        .ok_or_else(|| JoinQuantError::Protocol("invalid data URI".into()))?;
    Ok(B64.decode(payload.trim())?)
}

/// Slider challenge as returned by `captchar`.
#[derive(Debug)]
pub(crate) struct CaptchaChallenge {
    /// Background image bytes (PNG, with notch).
    pub background: Vec<u8>,
    /// Puzzle-piece image bytes (PNG, with alpha), when served.
    pub piece: Option<Vec<u8>>,
}

/// Parse the `captchar` response JSON into a [`CaptchaChallenge`].
pub(crate) fn parse_captcha_response(v: &Value) -> Result<CaptchaChallenge, JoinQuantError> {
    let data = v
        .get("data")
        .ok_or_else(|| JoinQuantError::Protocol("captchar: missing data".into()))?;
    let bg = data
        .get("bgImg")
        .and_then(Value::as_str)
        .ok_or_else(|| JoinQuantError::Protocol("captchar: missing bgImg".into()))?;
    let piece = data.get("hqImg").and_then(Value::as_str);
    Ok(CaptchaChallenge {
        background: decode_data_uri(bg)?,
        piece: piece.map(decode_data_uri).transpose()?,
    })
}

/// Parse the `validate` response: `Some(token)` when `data.result == true`.
pub(crate) fn parse_validate_response(v: &Value) -> Option<String> {
    let data = v.get("data")?;
    if data.get("result").and_then(Value::as_bool) == Some(true) {
        data.get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(Some(String::new()))
    } else {
        None
    }
}

async fn fetch_challenge(http: &Client) -> Result<CaptchaChallenge, JoinQuantError> {
    let resp: Value = http
        .post(format!("{BASE}/common/verifyCode/captchar"))
        .header("X-Requested-With", "XMLHttpRequest")
        .send()
        .await?
        .json()
        .await?;
    parse_captcha_response(&resp)
}

async fn validate_axis(http: &Client, axis_x: u32) -> Result<Option<String>, JoinQuantError> {
    let resp: Value = http
        .post(format!("{BASE}/common/verifyCode/validate"))
        .header("X-Requested-With", "XMLHttpRequest")
        .form(&[("axisX", axis_x.to_string())])
        .send()
        .await?
        .json()
        .await?;
    Ok(parse_validate_response(&resp))
}

/// Run the bounded slider-captcha loop; returns the `valideCode` token.
///
/// Only a single `axisX` scalar is submitted — no drag trajectory (doc
/// §1.4). `LowConfidence` from the solver and `result=false` from the
/// server both trigger a fresh challenge (bounded by
/// [`CAPTCHA_MAX_ATTEMPTS`]).
async fn solve_captcha(http: &Client) -> Result<String, JoinQuantError> {
    for attempt in 1..=CAPTCHA_MAX_ATTEMPTS {
        let challenge = fetch_challenge(http).await?;
        let bg = image::load_from_memory(&challenge.background)?;
        let piece = challenge
            .piece
            .as_deref()
            .map(image::load_from_memory)
            .transpose()?;
        let solution = match astock_captcha::solve_slider(&bg, piece.as_ref()) {
            Ok(s) => s,
            Err(astock_captcha::CaptchaError::LowConfidence { confidence, .. }) => {
                tracing::debug!(
                    attempt,
                    confidence,
                    "slider low confidence, refreshing captcha"
                );
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(token) = validate_axis(http, solution.distance).await? {
            return Ok(token);
        }
        tracing::debug!(
            attempt,
            distance = solution.distance,
            "slider rejected, refreshing"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(JoinQuantError::CaptchaExhausted {
        attempts: CAPTCHA_MAX_ATTEMPTS,
    })
}

/// Full login with slider-captcha fallback. On success the shared cookie
/// jar holds `PHPSESSID` and `token`.
pub(crate) async fn login(http: &Client, creds: &Credentials) -> Result<(), JoinQuantError> {
    match do_login(http, creds, None).await? {
        LoginOutcome::Success => Ok(()),
        LoginOutcome::NeedCaptcha => {
            let token = solve_captcha(http).await?;
            match do_login(http, creds, Some(&token)).await? {
                LoginOutcome::Success => Ok(()),
                LoginOutcome::NeedCaptcha => Err(JoinQuantError::LoginFailed {
                    code: "105".into(),
                    message: "captcha required again after validation".into(),
                }),
                LoginOutcome::Failed { code, message } => {
                    Err(JoinQuantError::LoginFailed { code, message })
                }
            }
        }
        LoginOutcome::Failed { code, message } => {
            Err(JoinQuantError::LoginFailed { code, message })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_success_with_string_code() {
        let v = json!({
            "data": {"redirect": "/default/research/index", "user": {"user": "847392"}},
            "status": "0",
            "code": "00000"
        });
        assert_eq!(parse_login_response(&v), LoginOutcome::Success);
    }

    #[test]
    fn parse_need_captcha_with_numeric_code() {
        let v = json!({"code": 105, "msg": "需要验证"});
        assert_eq!(parse_login_response(&v), LoginOutcome::NeedCaptcha);
    }

    #[test]
    fn parse_failed_with_message() {
        let v = json!({"code": "20000", "msg": "用户不存在或密码错误"});
        assert_eq!(
            parse_login_response(&v),
            LoginOutcome::Failed {
                code: "20000".into(),
                message: "用户不存在或密码错误".into(),
            }
        );
    }

    #[test]
    fn parse_missing_code_is_failure() {
        let v = json!({"foo": 1});
        match parse_login_response(&v) {
            LoginOutcome::Failed { code, .. } => assert!(code.is_empty()),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn decode_data_uri_strips_prefix() {
        let uri = format!("data:image/png;base64,{}", B64.encode(b"\x89PNG\r\n"));
        assert_eq!(decode_data_uri(&uri).unwrap(), b"\x89PNG\r\n");
        assert!(decode_data_uri("no-comma-here").is_err());
    }

    #[test]
    fn parse_captcha_response_extracts_images() {
        let v = json!({
            "data": {
                "bgImg": format!("data:image/png;base64,{}", B64.encode(b"BG")),
                "hqImg": format!("data:image/png;base64,{}", B64.encode(b"PIECE")),
                "axisY": 40
            }
        });
        let ch = parse_captcha_response(&v).unwrap();
        assert_eq!(ch.background, b"BG");
        assert_eq!(ch.piece.as_deref(), Some(b"PIECE".as_slice()));
    }

    #[test]
    fn parse_captcha_response_without_piece() {
        let v = json!({"data": {"bgImg": format!("data:,{}", B64.encode(b"BG"))}});
        let ch = parse_captcha_response(&v).unwrap();
        assert!(ch.piece.is_none());
    }

    #[test]
    fn parse_validate_response_result_and_token() {
        let ok = json!({"data": {"result": true, "token": "tok123", "action": "login"}});
        assert_eq!(parse_validate_response(&ok).as_deref(), Some("tok123"));
        let bad = json!({"data": {"result": false}});
        assert!(parse_validate_response(&bad).is_none());
    }
}
