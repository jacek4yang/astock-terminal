//! 服务器探测选路与连接池：并发 TCP 测速 + 深度探测（握手 + 0x044E API 延迟），
//! top-N 长连接入池，30s 心跳，失败黑名单（带冷却）+ 自动切换。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify};
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::conn::Conn;
use crate::error::{Result, TdxError};
use crate::protocol::frame::build_security_count_packet;
use crate::protocol::parse::parse_security_count;
use crate::servers::{Server, ALL_SERVERS, PRIMARY_SERVERS};

/// 池配置。
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 单台服务器探测/请求超时
    pub timeout: Duration,
    /// 池中保持的长连接数（top-N）
    pub pool_size: usize,
    /// 心跳间隔
    pub heartbeat_interval: Duration,
    /// 黑名单冷却时长
    pub blacklist_cooldown: Duration,
    /// TCP 测速后进入深度探测的台数上限（按 TCP 延迟升序取前 N）
    pub deep_probe_limit: usize,
    /// 额外候选服务器（自定义入口），探测时排在最前
    pub extra_servers: Vec<Server>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            pool_size: 3,
            heartbeat_interval: Duration::from_secs(30),
            blacklist_cooldown: Duration::from_secs(600),
            deep_probe_limit: 20,
            extra_servers: Vec::new(),
        }
    }
}

/// 单台服务器的深度探测结果。
#[derive(Debug, Clone, Copy)]
pub struct ProbeResult {
    pub server: Server,
    pub tcp_ms: f64,
    /// 握手延迟（含在 TCP 建连之后，三步握手 + 首个 API 前）
    pub handshake_ms: f64,
    /// 0x044E API 往返延迟
    pub api_ms: f64,
}

type ServerKey = (String, u16);

fn key(s: Server) -> ServerKey {
    (s.ip.to_string(), s.port)
}

struct Slot {
    server: Server,
    conn: Option<Conn>,
}

pub(crate) struct State {
    slots: Vec<Mutex<Slot>>,
    ranked: Vec<Server>,
    blacklist: Mutex<HashMap<ServerKey, Instant>>,
}

/// 第一阶段：并发 TCP 测速，返回按延迟升序的 (server, tcp_ms)。
pub async fn tcp_speed_test(servers: &[Server], timeout: Duration) -> Vec<(Server, f64)> {
    let mut set = JoinSet::new();
    for &server in servers {
        set.spawn(async move {
            let start = Instant::now();
            let addr = format!("{}:{}", server.ip, server.port);
            match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await {
                Ok(Ok(_)) => Some((server, start.elapsed().as_secs_f64() * 1000.0)),
                _ => None,
            }
        });
    }
    let mut ok: Vec<(Server, f64)> = set.join_all().await.into_iter().flatten().collect();
    ok.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    ok
}

/// 第二阶段：深度探测（建连 + 握手 + 0x044E API 延迟，并验证响应可解析）。
pub async fn deep_probe(server: Server, timeout: Duration) -> Result<ProbeResult> {
    let start = Instant::now();
    let addr = format!("{}:{}", server.ip, server.port);
    let stream = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(&addr)).await??;
    let tcp_ms = start.elapsed().as_secs_f64() * 1000.0;
    drop(stream);

    let hs_start = Instant::now();
    let mut conn = Conn::connect(server, timeout).await?; // 含三步握手
    let handshake_ms = hs_start.elapsed().as_secs_f64() * 1000.0;

    let api_start = Instant::now();
    let body = conn.request(&build_security_count_packet(1)).await?;
    let api_ms = api_start.elapsed().as_secs_f64() * 1000.0;
    parse_security_count(&body)?;

    Ok(ProbeResult {
        server,
        tcp_ms,
        handshake_ms,
        api_ms,
    })
}

/// 完整两阶段探测：先 TCP 测速全量候选，再对最快的前 `deep_limit` 台做深度探测。
/// 返回按 API 延迟升序的可用服务器。
pub async fn probe_servers(
    candidates: &[Server],
    timeout: Duration,
    deep_limit: usize,
) -> Vec<ProbeResult> {
    let reachable = tcp_speed_test(candidates, timeout).await;
    debug!(reachable = reachable.len(), "tdx tcp speed test done");

    let mut set = JoinSet::new();
    for (server, tcp_ms) in reachable.into_iter().take(deep_limit) {
        set.spawn(async move {
            let mut r = deep_probe(server, timeout).await.ok()?;
            r.tcp_ms = tcp_ms;
            Some(r)
        });
    }
    let mut results: Vec<ProbeResult> = set.join_all().await.into_iter().flatten().collect();
    results.sort_by(|a, b| {
        a.api_ms
            .partial_cmp(&b.api_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results
}

/// 在池上执行一次请求；失败自动换服务器重试，全池失败报错。
///
/// 锁序约定：同一时刻最多持有一个 slot 锁；黑名单锁单独获取，互不嵌套。
pub(crate) async fn pool_request(
    state: &State,
    config: &PoolConfig,
    packet: &[u8],
) -> Result<Vec<u8>> {
    let mut last_err = TdxError::NoServerAvailable;
    for slot in &state.slots {
        let mut guard = slot.lock().await;
        if guard.conn.is_none() {
            try_reconnect(state, config, &mut guard).await;
        }
        let Some(conn) = guard.conn.as_mut() else {
            continue;
        };
        match conn.request(packet).await {
            Ok(body) => return Ok(body),
            Err(e) => {
                warn!(server = %conn.server.ip, error = %e, "tdx request failed, switching");
                last_err = e;
                let server = guard.server;
                guard.conn = None;
                drop(guard);
                block_server(state, server).await;
            }
        }
    }
    Err(last_err)
}

/// 心跳一轮：向池内每条连接发 0x044E，失败的标记坏死并进黑名单。
pub(crate) async fn heartbeat_once(state: &State, config: &PoolConfig) {
    for slot in &state.slots {
        let mut guard = slot.lock().await;
        let mut dead = None;
        if let Some(conn) = guard.conn.as_mut() {
            let pkt = build_security_count_packet(0);
            let alive =
                matches!(conn.request(&pkt).await, Ok(body) if parse_security_count(&body).is_ok());
            if !alive {
                warn!(server = %conn.server.ip, "tdx heartbeat failed");
                dead = Some(guard.server);
                guard.conn = None;
            }
        }
        if let Some(server) = dead {
            block_server(state, server).await;
        }
        if guard.conn.is_none() {
            try_reconnect(state, config, &mut guard).await;
        }
    }
}

/// 拉黑一台服务器（冷却期内不参与重建）。
pub(crate) async fn block_server(state: &State, server: Server) {
    state
        .blacklist
        .lock()
        .await
        .insert(key(server), Instant::now());
}

async fn is_blocked(state: &State, config: &PoolConfig, server: Server) -> bool {
    state
        .blacklist
        .lock()
        .await
        .get(&key(server))
        .is_some_and(|t| t.elapsed() < config.blacklist_cooldown)
}

/// 从排序列表取第一台未拉黑的服务器重建该槽位连接。
///
/// 坏死服务器已被拉黑（冷却期内跳过），无需额外排除当前槽位。
/// 刻意不做跨槽位「已在池内」去重：调用方持有一个 slot 锁，再去锁其它
/// slot 会与并发请求构成 ABBA 死锁。允许短暂的重复连接，下一轮心跳会
/// 自行收敛；数据正确性不受影响。
async fn try_reconnect(state: &State, config: &PoolConfig, guard: &mut Slot) {
    for &candidate in &state.ranked {
        if is_blocked(state, config, candidate).await {
            continue;
        }
        match Conn::connect(candidate, config.timeout).await {
            Ok(conn) => {
                debug!(server = %candidate.ip, "tdx reconnected");
                guard.server = candidate;
                guard.conn = Some(conn);
                return;
            }
            Err(_) => block_server(state, candidate).await,
        }
    }
}

async fn heartbeat_loop(state: Arc<State>, config: PoolConfig, stop: Arc<Notify>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.heartbeat_interval) => {
                heartbeat_once(&state, &config).await;
            }
            _ = stop.notified() => break,
        }
    }
}

/// 连接池。通过 [`ServerPool::start`] 建立，后台心跳任务随 `Drop` 停止。
pub struct ServerPool {
    state: Arc<State>,
    config: PoolConfig,
    stop: Arc<Notify>,
    heartbeat_task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for ServerPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerPool")
            .field("pool_size", &self.state.slots.len())
            .finish()
    }
}

impl ServerPool {
    /// 启动：探测全量候选 → 选 top-N 建池 → 拉起心跳任务。
    pub async fn start(config: PoolConfig) -> Result<Self> {
        let mut candidates: Vec<Server> = config.extra_servers.clone();
        candidates.extend_from_slice(PRIMARY_SERVERS);
        for &(name, ip, port) in ALL_SERVERS {
            let s = Server { name, ip, port };
            if !candidates.contains(&s) {
                candidates.push(s);
            }
        }

        let probed = probe_servers(&candidates, config.timeout, config.deep_probe_limit).await;
        if probed.is_empty() {
            return Err(TdxError::NoServerAvailable);
        }
        let ranked: Vec<Server> = probed.iter().map(|r| r.server).collect();
        debug!(ranked = ?ranked.iter().map(|s| s.ip).collect::<Vec<_>>(), "tdx servers ranked");

        let mut slots = Vec::with_capacity(config.pool_size);
        let mut any_connected = false;
        for &server in ranked.iter().take(config.pool_size) {
            let conn = Conn::connect(server, config.timeout).await.ok();
            any_connected |= conn.is_some();
            slots.push(Mutex::new(Slot { server, conn }));
        }
        if !any_connected {
            return Err(TdxError::NoServerAvailable);
        }

        let state = Arc::new(State {
            slots,
            ranked,
            blacklist: Mutex::new(HashMap::new()),
        });
        let stop = Arc::new(Notify::new());
        let task = tokio::spawn(heartbeat_loop(
            Arc::clone(&state),
            config.clone(),
            Arc::clone(&stop),
        ));
        Ok(Self {
            state,
            config,
            stop,
            heartbeat_task: std::sync::Mutex::new(Some(task)),
        })
    }

    /// 当前池内服务器（`ip:port` 列表）。
    pub async fn active_servers(&self) -> Vec<String> {
        let mut out = Vec::new();
        for slot in &self.state.slots {
            let s = slot.lock().await;
            out.push(format!("{}:{}", s.server.ip, s.server.port));
        }
        out
    }

    /// 执行一次协议请求（带跨服务器失败切换）。
    pub async fn request(&self, packet: &[u8]) -> Result<Vec<u8>> {
        pool_request(&self.state, &self.config, packet).await
    }

    /// 手动触发一轮心跳（测试/维护用）。
    pub async fn heartbeat_now(&self) {
        heartbeat_once(&self.state, &self.config).await;
    }

    /// 拉黑一台服务器。
    pub async fn block_server(&self, server: Server) {
        block_server(&self.state, server).await;
    }
}

impl Drop for ServerPool {
    fn drop(&mut self) {
        self.stop.notify_waiters();
        if let Ok(mut guard) = self.heartbeat_task.lock() {
            if let Some(task) = guard.take() {
                task.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::constants::{SETUP_CMD1, SETUP_CMD2, SETUP_CMD3};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// 最小 mock tdx 服务器：应答三步握手，之后对每个请求回一个
    /// 合法的 0x044E 响应（count=0x1539）。`drop_after_handshake`=true 时
    /// 握手完成即断开（模拟坏死服务器）。
    async fn spawn_mock(drop_after_handshake: bool) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    // 三步握手：读固定字节，回 16 字节空头（zip=unzip=0）
                    for cmd_len in [SETUP_CMD1.len(), SETUP_CMD2.len(), SETUP_CMD3.len()] {
                        let mut buf = vec![0u8; cmd_len];
                        if sock.read_exact(&mut buf).await.is_err() {
                            return;
                        }
                        if sock.write_all(&[0u8; 16]).await.is_err() {
                            return;
                        }
                    }
                    if drop_after_handshake {
                        return; // 连接随 sock  drop 断开
                    }
                    // 请求循环：请求头 12 字节可知 zip_len（含 payload+2），
                    // 简化：按 18 字节心跳/数量包读取，回 count=0x1539
                    loop {
                        let mut req = [0u8; 18];
                        if sock.read_exact(&mut req).await.is_err() {
                            return;
                        }
                        let mut rsp = Vec::with_capacity(18);
                        rsp.extend_from_slice(&[0u8; 12]); // seq/method/reserved
                        rsp.extend_from_slice(&2u16.to_le_bytes()); // zip_size
                        rsp.extend_from_slice(&2u16.to_le_bytes()); // unzip_size
                        rsp.extend_from_slice(&0x1539u16.to_le_bytes());
                        if sock.write_all(&rsp).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Server {
            name: "mock",
            ip: "127.0.0.1",
            port,
        }
    }

    fn test_config() -> PoolConfig {
        PoolConfig {
            timeout: Duration::from_secs(2),
            pool_size: 1,
            heartbeat_interval: Duration::from_millis(50),
            blacklist_cooldown: Duration::from_secs(60),
            deep_probe_limit: 5,
            extra_servers: Vec::new(),
        }
    }

    fn make_state(slots: Vec<Slot>, ranked: Vec<Server>) -> State {
        State {
            slots: slots.into_iter().map(Mutex::new).collect(),
            ranked,
            blacklist: Mutex::new(HashMap::new()),
        }
    }

    #[tokio::test]
    async fn deep_probe_against_mock() {
        let server = spawn_mock(false).await;
        let r = deep_probe(server, Duration::from_secs(2)).await.unwrap();
        assert!(r.api_ms >= 0.0);
        let reachable = tcp_speed_test(&[server], Duration::from_secs(1)).await;
        assert_eq!(reachable.len(), 1);
    }

    #[tokio::test]
    async fn request_succeeds_on_healthy_mock() {
        let server = spawn_mock(false).await;
        let conn = Conn::connect(server, Duration::from_secs(2)).await.unwrap();
        let state = make_state(
            vec![Slot {
                server,
                conn: Some(conn),
            }],
            vec![server],
        );
        let body = pool_request(&state, &test_config(), &build_security_count_packet(1))
            .await
            .unwrap();
        assert_eq!(parse_security_count(&body).unwrap(), 0x1539);
    }

    #[tokio::test]
    async fn failover_switches_server_and_blacklists() {
        let dead = spawn_mock(true).await; // 握手后即断
        let good = spawn_mock(false).await;
        let dead_conn = Conn::connect(dead, Duration::from_secs(2)).await.unwrap();
        let state = make_state(
            vec![
                Slot {
                    server: dead,
                    conn: Some(dead_conn),
                },
                Slot {
                    server: good,
                    conn: None, // 触发 try_reconnect 到 good
                },
            ],
            vec![dead, good],
        );
        let config = test_config();
        let body = pool_request(&state, &config, &build_security_count_packet(1))
            .await
            .unwrap();
        assert_eq!(parse_security_count(&body).unwrap(), 0x1539);
        // dead 进了黑名单
        assert!(is_blocked(&state, &config, dead).await);
        assert!(!is_blocked(&state, &config, good).await);
    }

    #[tokio::test]
    async fn heartbeat_marks_dead_and_reconnects() {
        let dead = spawn_mock(true).await;
        let good = spawn_mock(false).await;
        let dead_conn = Conn::connect(dead, Duration::from_secs(2)).await.unwrap();
        let state = make_state(
            vec![Slot {
                server: dead,
                conn: Some(dead_conn),
            }],
            vec![good],
        );
        let config = test_config();
        // 心跳发现 dead 坏死 → 拉黑并从 ranked 重建到 good
        heartbeat_once(&state, &config).await;
        assert!(is_blocked(&state, &config, dead).await);
        let guard = state.slots[0].lock().await;
        assert_eq!(guard.server, good);
        assert!(guard.conn.is_some());
    }
}
