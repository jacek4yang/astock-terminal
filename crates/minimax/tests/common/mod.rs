//! Minimal in-process HTTP/1.1 test server.
//!
//! Serves canned responses from a handler closure over a real TCP socket, so
//! the production `reqwest` transport is exercised end-to-end. Each request
//! gets a fresh connection (`Connection: close`); SSE responses can be
//! "dripped" in small chunks to exercise incremental parsing.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use astock_minimax::http::ReqwestHttp;
use astock_minimax::{
    Http, MinimaxClient, RateGate, RateGateConfig, Region, RegionDetector, SecretKey,
};

/// A request observed by the test server.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

/// A canned response.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// When true, the body is written in small flushed chunks without a
    /// Content-Length, exercising incremental SSE parsing.
    pub drip: bool,
}

impl RawResponse {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: body.as_bytes().to_vec(),
            drip: false,
        }
    }

    pub fn sse(body: &str) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".to_string(), "text/event-stream".to_string())],
            body: body.as_bytes().to_vec(),
            drip: true,
        }
    }
}

pub struct TestServer {
    pub url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    _acceptor: JoinHandle<()>,
}

impl TestServer {
    /// All recorded requests, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// How many requests hit paths containing `needle`.
    pub fn count_path(&self, needle: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path.contains(needle))
            .count()
    }
}

/// Spawn a server on a random localhost port.
pub fn spawn<F>(handler: F) -> TestServer
where
    F: Fn(&RecordedRequest) -> RawResponse + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let handler: Handler = Arc::new(handler);
    let acceptor = {
        let requests = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let handler = handler.clone();
                let requests = requests.clone();
                std::thread::spawn(move || {
                    let _ = handle_conn(&mut stream, &handler, &requests);
                });
            }
        })
    };
    TestServer {
        url: format!("http://{addr}"),
        requests,
        _acceptor: acceptor,
    }
}

type Handler = Arc<dyn Fn(&RecordedRequest) -> RawResponse + Send + Sync>;

fn handle_conn(
    stream: &mut TcpStream,
    handler: &Handler,
    requests: &Arc<Mutex<Vec<RecordedRequest>>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut content_length = 0usize;
    let mut authorization = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        let trimmed = line.trim_end();
        if n == 0 || trimmed.is_empty() {
            break;
        }
        let lower = trimmed.to_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            authorization = trimmed.split_once(':').map(|(_, v)| v.trim().to_string());
        }
    }

    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;

    let recorded = RecordedRequest {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    requests.lock().unwrap().push(recorded.clone());

    let resp = handler(&recorded);
    let reason = match resp.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {} {}\r\n", resp.status, reason);
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }

    if resp.drip {
        head.push_str("connection: close\r\n\r\n");
        stream.write_all(head.as_bytes())?;
        for chunk in resp.body.chunks(8) {
            stream.write_all(chunk)?;
            stream.flush()?;
            std::thread::sleep(Duration::from_millis(5));
        }
        // Connection closes when the stream drops: end of body.
    } else {
        head.push_str(&format!(
            "content-length: {}\r\nconnection: close\r\n\r\n",
            resp.body.len()
        ));
        stream.write_all(head.as_bytes())?;
        stream.write_all(&resp.body)?;
        stream.flush()?;
    }
    Ok(())
}

/// A client pointed at the test server. Region candidates are path prefixes
/// (`{url}/cn`, `{url}/intl`) so a handler can emulate per-region behavior;
/// both use a fast gate so retries stay test-friendly.
pub fn test_client(key: &str, url: &str) -> MinimaxClient {
    let http: Arc<dyn Http> = Arc::new(ReqwestHttp::new_direct());
    let detector = RegionDetector::with_hosts(
        http.clone(),
        vec![
            (Region::Cn, format!("{url}/cn"), format!("{url}/cn")),
            (Region::Intl, format!("{url}/intl"), format!("{url}/intl")),
        ],
    );
    MinimaxClient::with_http(SecretKey::new(key), http)
        .with_detector(detector)
        .with_gate(RateGate::new(RateGateConfig {
            max_attempts: 4,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(50),
        }))
}

/// Canned `token_plan/remains` success body for `percent` remaining on the
/// rolling window of `MiniMax-M2.5`.
pub fn quota_body(percent: u64) -> String {
    format!(
        r#"{{"model_remains":[{{"start_time":1755763200000,"end_time":1755781200000,"remains_time":3600000,"current_interval_total_count":100,"current_interval_usage_count":{},"model_name":"MiniMax-M2.5","current_weekly_total_count":1000,"current_weekly_usage_count":10,"weekly_start_time":1755504000000,"weekly_end_time":1756108800000,"weekly_remains_time":600000000,"current_interval_status":1,"current_interval_remaining_percent":{},"current_weekly_status":1,"current_weekly_remaining_percent":99}}],"base_resp":{{"status_code":0,"status_msg":"success"}}}}"#,
        100 - percent,
        percent
    )
}
