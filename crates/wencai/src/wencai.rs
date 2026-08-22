//! iwencai (同花顺问财) natural-language stock screening client.
//!
//! Flow (mirrors pywencai, verified against live traffic 2026-08-21):
//! 1. POST `http://www.iwencai.com/customized/chart/get-robot-data` with a
//!    fresh `hexin-v` token (header + `v` cookie) and the natural-language
//!    `question`.
//! 2. If the server answers with `data.captcha_url`, the slider captcha is
//!    solved (feature `captcha`, bounded attempts) and the query retried.
//! 3. Result rows are extracted from `answer[0].txt[0].content.components`,
//!    falling back to the `gateway/urp/v7/landing/getDataList` follow-up
//!    endpoint for `xuangu_tableV1` components.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tracing::{debug, instrument};

use crate::error::WencaiError;
use crate::hexin::hexin_v;
use crate::pace::Pacer;

/// Robot-data endpoint used by both akshare-era scripts and current pywencai.
const ROBOT_DATA_URL: &str = "http://www.iwencai.com/customized/chart/get-robot-data";
/// Follow-up table endpoint for `xuangu_tableV1` components.
const DATA_LIST_URL: &str = "http://www.iwencai.com/gateway/urp/v7/landing/getDataList";

/// Default desktop UA; iwencai rejects obvious bot agents before even
/// issuing a captcha.
const DEFAULT_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// How many times a query is retried after solving a captcha challenge.
const MAX_CAPTCHA_ATTEMPTS: u32 = 3;

/// One result row: the well-known columns typed, everything else kept raw.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WencaiRow {
    /// 6-digit stock code (may carry a market suffix depending on source).
    pub code: String,
    /// Stock short name.
    pub name: String,
    /// Latest price, if present in the row.
    pub price: Option<f64>,
    /// Percent change on the day, if present.
    pub pct: Option<f64>,
    /// All remaining columns, keyed by the original Chinese column names.
    pub extra: Map<String, Value>,
}

/// Outcome of one successful screening query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WencaiResult {
    /// Server-reported total hit count (`row_count`), if present.
    pub total: Option<u64>,
    /// Rows of the first result page.
    pub rows: Vec<WencaiRow>,
}

/// iwencai screening client. Cheap to construct; shares one HTTP client,
/// cookie jar and pacer across queries.
pub struct WencaiClient {
    http: reqwest::Client,
    pacer: Pacer,
    ua: String,
    cookies: Mutex<BTreeMap<String, String>>,
    /// Session-stable hexin-v token. The browser computes `v` once per
    /// session (chameleon JS) and reuses it for every request; a passed
    /// captcha is linked server-side to this token, so it must NOT be
    /// regenerated per request.
    v_token: Mutex<Option<String>>,
}

impl Default for WencaiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WencaiClient {
    /// New client with default pacing (burst 3, then 1 request / 2 s).
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client build");
        Self {
            http,
            pacer: Pacer::new(Duration::from_secs(2), 3),
            ua: DEFAULT_UA.to_string(),
            cookies: Mutex::new(BTreeMap::new()),
            v_token: Mutex::new(None),
        }
    }

    /// Override the User-Agent.
    pub fn with_user_agent(mut self, ua: impl Into<String>) -> Self {
        self.ua = ua.into();
        self
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .lock()
            .expect("cookie jar poisoned")
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn store_cookies(&self, resp: &reqwest::Response) {
        let mut jar = self.cookies.lock().expect("cookie jar poisoned");
        for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(s) = value.to_str() {
                if let Some(pair) = s.split(';').next() {
                    if let Some((k, v)) = pair.split_once('=') {
                        jar.insert(k.trim().to_string(), v.trim().to_string());
                    }
                }
            }
        }
    }

    /// Session token: generated once, reused for the whole client lifetime.
    fn session_token(&self) -> Result<String, WencaiError> {
        let mut guard = self.v_token.lock().expect("v_token poisoned");
        if let Some(t) = guard.as_ref() {
            return Ok(t.clone());
        }
        let token = hexin_v()?;
        *guard = Some(token.clone());
        Ok(token)
    }

    /// Attach pacing, UA, the session hexin-v (header + `v` cookie) and jar.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response, WencaiError> {
        self.pacer.wait().await;
        let token = self.session_token()?;
        let mut cookies = self.cookie_header();
        if !cookies.is_empty() {
            cookies.push_str("; ");
        }
        cookies.push_str(&format!("v={token}"));
        let resp = req
            .header(reqwest::header::USER_AGENT, self.ua.as_str())
            .header("hexin-v", &token)
            .header(reqwest::header::COOKIE, cookies)
            // Browser-like hygiene headers; the THS WAF scores header sets.
            .header(reqwest::header::ACCEPT, "application/json, text/plain, */*")
            .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9,en;q=0.8")
            .header("sec-ch-ua", "\"Chromium\";v=\"126\", \"Google Chrome\";v=\"126\", \"Not-A.Brand\";v=\"99\"")
            .header("sec-ch-ua-mobile", "?0")
            .header("sec-ch-ua-platform", "\"Windows\"")
            .header("Sec-Fetch-Site", "same-origin")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Dest", "empty")
            .send()
            .await?;
        self.store_cookies(&resp);
        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::FORBIDDEN
        {
            return Err(WencaiError::RateLimited {
                status: status.as_u16(),
            });
        }
        Ok(resp)
    }

    #[cfg(feature = "captcha")]
    pub(crate) async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, WencaiError> {
        let resp = self.send(self.http.get(url)).await?;
        Ok(resp.bytes().await?.to_vec())
    }

    #[cfg(feature = "captcha")]
    pub(crate) async fn get_text(&self, url: &str) -> Result<String, WencaiError> {
        let resp = self.send(self.http.get(url)).await?;
        Ok(resp.text().await?)
    }

    pub(crate) async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<String, WencaiError> {
        let resp = self.send(self.http.post(url).form(form)).await?;
        Ok(resp.text().await?)
    }

    /// Run one natural-language screening query, e.g.
    /// `连续3天换手率大于5%的主板股票`.
    ///
    /// On a captcha challenge the slider is solved automatically when the
    /// `captcha` feature is enabled (up to 3 attempts), otherwise
    /// [`WencaiError::NeedCaptcha`] is returned.
    #[instrument(skip(self), fields(question = question))]
    pub async fn search(&self, question: &str) -> Result<WencaiResult, WencaiError> {
        let mut captcha_attempts = 0u32;
        let mut last_reason = String::new();
        loop {
            let body = self.robot_data(question).await?;
            match classify(&body)? {
                RobotResponse::Rows(result) => return Ok(result),
                RobotResponse::NeedDataList { url_params, condition } => {
                    return self.data_list(&url_params, &condition).await;
                }
                RobotResponse::Captcha { captcha_url } => {
                    captcha_attempts += 1;
                    if captcha_attempts > MAX_CAPTCHA_ATTEMPTS {
                        return Err(WencaiError::CaptchaFailed {
                            attempts: MAX_CAPTCHA_ATTEMPTS,
                            last_reason,
                        });
                    }
                    match self.solve_captcha(&captcha_url, captcha_attempts).await {
                        Ok(()) => {}
                        // Feature off: surface the challenge URL to the caller.
                        Err(e @ WencaiError::NeedCaptcha { .. }) => return Err(e),
                        // A failed solve burns one attempt; the next loop
                        // iteration re-queries and gets a fresh challenge.
                        Err(e) => last_reason = e.to_string(),
                    }
                }
            }
        }
    }

    async fn robot_data(&self, question: &str) -> Result<Value, WencaiError> {
        let payload = json!({
            "add_info": "{\"urp\":{\"scene\":1,\"company\":1,\"business\":1},\"contentType\":\"json\",\"searchInfo\":true}",
            "perpage": "100",
            "page": 1,
            "source": "Ths_iwencai_Xuangu",
            "log_info": "{\"input_type\":\"click\"}",
            "version": "2.0",
            "secondary_intent": "stock",
            "question": question,
        });
        let resp = self
            .send(
                self.http
                    .post(ROBOT_DATA_URL)
                    .header(reqwest::header::REFERER, "http://www.iwencai.com/unifiedwap/result")
                    .json(&payload),
            )
            .await?;
        let text = resp.text().await?;
        if text.contains("Nginx forbidden") {
            // WAF IP ban (observed live after captcha sequences): HTTP 200
            // with an "<h1>Nginx forbidden.</h1>" body. Treat as throttling.
            return Err(WencaiError::RateLimited { status: 200 });
        }
        serde_json::from_str(&text)
            .map_err(|e| WencaiError::Parse(format!("robot-data not JSON: {e}; body: {}", truncate(&text))))
    }

    #[cfg(feature = "captcha")]
    async fn solve_captcha(&self, captcha_url: &str, attempt: u32) -> Result<(), WencaiError> {
        tracing::warn!(attempt, captcha_url, "iwencai challenged with captcha; solving slider");
        crate::captcha::solve_slider(self).await
    }

    #[cfg(not(feature = "captcha"))]
    async fn solve_captcha(&self, captcha_url: &str, _attempt: u32) -> Result<(), WencaiError> {
        Err(WencaiError::NeedCaptcha {
            captcha_url: captcha_url.to_string(),
        })
    }

    /// Follow-up table fetch for `xuangu_tableV1` components.
    async fn data_list(
        &self,
        url_params: &[(String, String)],
        condition: &Value,
    ) -> Result<WencaiResult, WencaiError> {
        let mut form: Vec<(String, String)> = url_params.to_vec();
        form.push(("perpage".into(), "100".into()));
        form.push(("page".into(), "1".into()));
        form.push(("query_type".into(), "stock".into()));
        form.push(("condition".into(), condition.to_string()));
        let text = self.post_form(DATA_LIST_URL, &form).await?;
        let body: Value = serde_json::from_str(&text)
            .map_err(|e| WencaiError::Parse(format!("getDataList not JSON: {e}; body: {}", truncate(&text))))?;
        let datas = body
            .pointer("/answer/components/0/data/datas")
            .and_then(Value::as_array)
            .ok_or_else(|| WencaiError::Parse(format!("getDataList has no datas: {}", truncate(&text))))?;
        let total = body
            .pointer("/answer/components/0/data/meta/extra/row_count")
            .and_then(Value::as_u64);
        Ok(WencaiResult {
            total,
            rows: datas.iter().filter_map(row_from_value).collect(),
        })
    }
}

/// What the robot-data response turned out to be.
enum RobotResponse {
    Rows(WencaiResult),
    NeedDataList {
        url_params: Vec<(String, String)>,
        condition: Value,
    },
    Captcha {
        captcha_url: String,
    },
}

/// Classify and pre-parse a robot-data response body.
fn classify(body: &Value) -> Result<RobotResponse, WencaiError> {
    // Captcha challenge: {"code":0,"data":{"captcha_url":"..."}}
    if let Some(url) = body.pointer("/data/captcha_url").and_then(Value::as_str) {
        return Ok(RobotResponse::Captcha {
            captcha_url: url.to_string(),
        });
    }

    // Normal answer: data.answer[0].txt[0].content (object or JSON string).
    let content = match body.pointer("/data/answer/0/txt/0/content") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).map_err(|e| {
            WencaiError::Parse(format!("content string is not JSON: {e}"))
        })?,
        Some(v) => v.clone(),
        None => {
            return Err(WencaiError::Parse(format!(
                "no answer content and no captcha_url: {}",
                truncate(&body.to_string())
            )))
        }
    };
    let components = content
        .get("components")
        .and_then(Value::as_array)
        .ok_or_else(|| WencaiError::Parse("content has no components array".into()))?;

    // Direct rows on any component.
    for comp in components {
        if let Some(datas) = comp.pointer("/data/datas").and_then(Value::as_array) {
            let total = comp
                .pointer("/data/meta/extra/row_count")
                .and_then(Value::as_u64);
            return Ok(RobotResponse::Rows(WencaiResult {
                total,
                rows: datas.iter().filter_map(row_from_value).collect(),
            }));
        }
    }

    // xuangu_tableV1: rows live behind the landing/getDataList endpoint.
    if let Some(comp) = components.first() {
        if comp.get("show_type").and_then(Value::as_str) == Some("xuangu_tableV1") {
            let footer_url = comp
                .pointer("/config/other_info/footer_info/url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let url_params = parse_query(footer_url);
            let condition = comp
                .pointer("/data/meta/extra/condition")
                .cloned()
                .unwrap_or(Value::Null);
            debug!(?url_params, "rows require getDataList follow-up");
            return Ok(RobotResponse::NeedDataList { url_params, condition });
        }
    }

    Err(WencaiError::Parse(format!(
        "no datas and not xuangu_tableV1: {}",
        truncate(&content.to_string())
    )))
}

/// Parse a URL's query string into key/value pairs (no decoding needed for
/// the ASCII params iwencai uses; percent-encoded values are kept verbatim).
fn parse_query(url: &str) -> Vec<(String, String)> {
    let Some((_, query)) = url.split_once('?') else {
        return Vec::new();
    };
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Map one raw `datas` object into a typed row.
fn row_from_value(v: &Value) -> Option<WencaiRow> {
    let obj = v.as_object()?;
    let find = |preds: &[&str]| -> Option<(&String, &Value)> {
        obj.iter().find(|(k, _)| preds.iter().any(|p| k.contains(p)))
    };
    let code = find(&["股票代码", "code"])
        .and_then(|(_, v)| v.as_str().map(str::to_string))
        .or_else(|| find(&["股票代码", "code"]).and_then(|(_, v)| v.as_u64().map(|n| n.to_string())))?;
    // iwencai codes sometimes look like "600519.SH" or "600519"; keep the digits.
    let code: String = code.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    let name = find(&["股票简称", "名称"])
        .and_then(|(_, v)| v.as_str())
        .unwrap_or_default()
        .to_string();
    let number = |preds: &[&str]| -> Option<f64> {
        let (_, v) = find(preds)?;
        v.as_f64().or_else(|| v.as_str()?.parse().ok())
    };
    let price = number(&["最新价", "现价"]);
    let pct = number(&["涨跌幅"]);
    let known = ["股票代码", "code", "股票简称", "名称", "最新价", "现价", "涨跌幅"];
    let extra: Map<String, Value> = obj
        .iter()
        .filter(|(k, _)| !known.iter().any(|p| k.contains(p)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Some(WencaiRow {
        code,
        name,
        price,
        pct,
        extra,
    })
}

fn truncate(s: &str) -> String {
    s.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_captcha() {
        let body = json!({"code":0,"msg":null,"data":{"captcha_url":"http://www.iwencai.com/ac_verification/captcha/?host=x"}});
        match classify(&body).unwrap() {
            RobotResponse::Captcha { captcha_url } => {
                assert!(captcha_url.contains("ac_verification"))
            }
            _ => panic!("expected captcha"),
        }
    }

    #[test]
    fn classify_rows_direct() {
        let body: Value = serde_json::from_str(include_str!("../tests/fixtures/robot_data_rows.json")).unwrap();
        match classify(&body).unwrap() {
            RobotResponse::Rows(result) => {
                assert_eq!(result.total, Some(2));
                assert_eq!(result.rows.len(), 2);
                let row = &result.rows[0];
                assert_eq!(row.code, "300750");
                assert_eq!(row.name, "宁德时代");
                assert_eq!(row.price, Some(265.3));
                assert_eq!(row.pct, Some(1.87));
                assert!(row.extra.contains_key("换手率"));
            }
            _ => panic!("expected rows"),
        }
    }

    #[test]
    fn classify_xuangu_table_v1() {
        let body: Value = serde_json::from_str(include_str!("../tests/fixtures/robot_data_xuangu.json")).unwrap();
        match classify(&body).unwrap() {
            RobotResponse::NeedDataList { url_params, condition } => {
                assert!(url_params.iter().any(|(k, _)| k == "urp_sort_way"));
                assert!(condition.is_array());
            }
            _ => panic!("expected NeedDataList"),
        }
    }

    #[test]
    fn classify_garbage() {
        let body = json!({"code": -1, "msg": "whatever"});
        assert!(matches!(classify(&body), Err(WencaiError::Parse(_))));
    }
}
