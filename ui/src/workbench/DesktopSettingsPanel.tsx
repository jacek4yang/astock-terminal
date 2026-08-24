import { useCallback, useEffect, useRef, useState } from "react";
import { requestNative } from "../bridge";
import DataQualityWorkbench from "../components/DataQualityWorkbench";
import { getProviderHealth, type ProviderHealthItem } from "../lib/api";
import { useAppStore } from "../store";
import { useRuntimeStatus } from "./runtime";
import { useWorkspaceStore } from "./store";

type EngineStatus = {
  status: string;
  engine_version: string;
  protocol_version: number;
  data_root?: { path?: string; origin?: string; legacy_path?: string | null };
};

type QuotaModel = {
  model_name: string;
  interval_total?: number | null;
  interval_used?: number | null;
  interval_remaining_percent?: number | null;
  interval_reset_at_ms?: number | null;
  weekly_total?: number | null;
  weekly_used?: number | null;
  weekly_remaining_percent?: number | null;
  weekly_reset_at_ms?: number | null;
};

type QuotaStatus = { fetched_at_ms: number; models: QuotaModel[] };
type ProviderTest = { provider: string; model: string; api_host: string; catalog_verified: boolean };
type CacheStats = {
  kline_parquet_bytes: number;
  kline_parquet_files: number;
  sqlite_bytes: number;
  tool_cache_rows: number;
  tool_cache_bytes: number;
  chat_bytes: number;
  total_bytes: number;
  disk_free_bytes?: number | null;
};

const bytes = (value?: number | null) => {
  if (value == null) return "—";
  if (value < 1024) return `${value} B`;
  if (value < 1024 ** 2) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 ** 3) return `${(value / 1024 ** 2).toFixed(1)} MiB`;
  return `${(value / 1024 ** 3).toFixed(2)} GiB`;
};

const resetTime = (value?: number | null) => value ? new Date(value).toLocaleString("zh-CN", { hour12: false }) : "未知";

export default function DesktopSettingsPanel() {
  const theme = useAppStore((state) => state.theme);
  const setTheme = useAppStore((state) => state.setTheme);
  const mode = useAppStore((state) => state.mode);
  const setMode = useAppStore((state) => state.setMode);
  const resetLayout = useWorkspaceStore((state) => state.resetLayout);
  const runtime = useRuntimeStatus();
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  const [providers, setProviders] = useState<ProviderHealthItem[]>([]);
  const [minimax, setMinimax] = useState(false);
  const [joinquant, setJoinquant] = useState(false);
  const [quota, setQuota] = useState<QuotaStatus | null>(null);
  const [providerTest, setProviderTest] = useState<ProviderTest | null>(null);
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [cleanupTarget, setCleanupTarget] = useState(512);
  const [credentialBusy, setCredentialBusy] = useState(false);
  const [credentialMessage, setCredentialMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const minimaxInput = useRef<HTMLInputElement | null>(null);
  const joinquantUserInput = useRef<HTMLInputElement | null>(null);
  const joinquantPasswordInput = useRef<HTMLInputElement | null>(null);

  const load = useCallback(async () => {
    const [engineState, providerRows, credentials, cacheStats] = await Promise.all([
      requestNative<EngineStatus>("engine", "diagnostics.status"),
      getProviderHealth(),
      requestNative<{ providers: { minimax: boolean; joinquant?: boolean } }>("engine", "credentials.status"),
      requestNative<CacheStats>("engine", "storage.cache.stats"),
    ]);
    setEngine(engineState); setProviders(providerRows); setMinimax(credentials.providers.minimax); setJoinquant(Boolean(credentials.providers.joinquant)); setCache(cacheStats); setError(null);
  }, []);

  useEffect(() => {
    let live = true;
    void load().catch((cause) => live && setError(cause instanceof Error ? cause.message : String(cause)));
    return () => { live = false; };
  }, [load]);

  const saveMinimax = async () => {
    const input = minimaxInput.current;
    const key = input?.value.trim() ?? "";
    if (!key || credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      await requestNative("engine", "credentials.minimax.set", { key });
      if (input) input.value = "";
      const verified = await requestNative<ProviderTest>("agent", "agent.provider.test", {}, { deadlineMs: 90_000 });
      setProviderTest(verified);
      setCredentialMessage(`已安全写入 Windows Credential Manager，并通过 MiniMax Plus 模型目录验证：${verified.model}。`);
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const testMinimax = async () => {
    if (!minimax || credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const verified = await requestNative<ProviderTest>("agent", "agent.provider.test", {}, { deadlineMs: 90_000 });
      setProviderTest(verified);
      setCredentialMessage(`MiniMax Plus 连接正常，当前选择模型 ${verified.model}。`);
    } catch (cause) {
      setProviderTest(null);
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const deleteMinimax = async () => {
    if (credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      await requestNative("engine", "credentials.minimax.delete");
      setProviderTest(null);
      setCredentialMessage("MiniMax 密钥已从 Windows Credential Manager 删除。");
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const loadQuota = async () => {
    if (!minimax || credentialBusy) return;
    setCredentialBusy(true); setError(null);
    try {
      setQuota(await requestNative<QuotaStatus>("engine", "credentials.minimax.quota", {}, { deadlineMs: 60_000 }));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const saveJoinquant = async () => {
    const username = joinquantUserInput.current?.value.trim() ?? "";
    const password = joinquantPasswordInput.current?.value ?? "";
    if (!username || !password || credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      await requestNative("engine", "credentials.joinquant.set", { username, password });
      if (joinquantUserInput.current) joinquantUserInput.current.value = "";
      if (joinquantPasswordInput.current) joinquantPasswordInput.current.value = "";
      setCredentialMessage("聚宽账号已写入 Credential Manager，并已在当前 Engine 中即时启用。");
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const deleteJoinquant = async () => {
    if (credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      await requestNative("engine", "credentials.joinquant.delete");
      setCredentialMessage("聚宽账号已删除，当前 Engine 已立即停用该数据源。");
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const cleanupCache = async () => {
    if (credentialBusy || !window.confirm("只会删除已过期的工具缓存和可重新下载的 Parquet 行情缓存；不会删除 SQLite、Agent 历史、聊天或研究报告。继续吗？")) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const result = await requestNative<{ after: CacheStats; report: { tool_cache_rows_deleted: number; parquet_files_deleted: number; bytes_freed: number } }>("engine", "storage.cache.cleanup", { target_mb: cleanupTarget }, { deadlineMs: 120_000 });
      setCache(result.after);
      setCredentialMessage(`缓存清理完成：过期工具记录 ${result.report.tool_cache_rows_deleted} 条，Parquet ${result.report.parquet_files_deleted} 个，释放 ${bytes(result.report.bytes_freed)}。`);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  return <div className="settings-workbench">
    <div className="settings-heading"><div><span className="eyebrow">DESKTOP SETTINGS</span><h2>配置</h2><p>界面、数据、模型凭据和本地运行状态集中在这里。</p></div><span className={`status-pill ${runtime.status ? "phase-completed" : ""}`}>{runtime.status ? "桌面服务已连接" : "正在连接"}</span></div>
    {error && <p className="inline-warning">{error}</p>}
    {credentialMessage && <p className="settings-success">{credentialMessage}</p>}
    <div className="settings-grid">
      <section><h3>界面与布局</h3><label><span>主题</span><select value={theme} onChange={(event) => setTheme(event.target.value as "dark" | "light")}><option value="dark">深色看盘</option><option value="light">浅色界面</option></select></label><label><span>信息密度</span><select value={mode} onChange={(event) => setMode(event.target.value as "pro" | "simple")}><option value="simple">普通股民模式</option><option value="pro">专业完整模式</option></select></label><button className="btn" onClick={resetLayout}>恢复默认比例</button><p>大盘主图与全市场列表之间的分隔线可拖动，比例会自动保存并随窗口响应。</p></section>
      <section><h3>研究数据</h3><dl><dt>数据目录</dt><dd>{engine?.data_root?.path ?? "读取中…"}</dd><dt>接管方式</dt><dd>{engine?.data_root?.origin ?? "—"}</dd><dt>旧目录</dt><dd>{engine?.data_root?.legacy_path ?? "无"}</dd></dl><p>历史数据库原地接管，不自动复制或删除大型 SQLite/Parquet 数据。</p></section>
      <section className="credential-settings"><h3>Agent 与 MiniMax Plus</h3><dl><dt>MiniMax Plus</dt><dd className={minimax ? "text-down" : "text-up"}>{minimax ? "Credential Manager 已配置" : "尚未配置"}</dd><dt>Agent Worker</dt><dd>{runtime.status?.agent ? `${runtime.status.agent.status} · PID ${runtime.status.agent.pid}` : "—"}</dd>{providerTest && <><dt>已验证模型</dt><dd>{providerTest.model}</dd><dt>服务区域</dt><dd>{providerTest.api_host.includes("minimaxi.com") ? "中国大陆" : "国际"}</dd></>}</dl><label><span>MiniMax Plus Key</span><input ref={minimaxInput} type="password" autoComplete="off" spellCheck={false} placeholder={minimax ? "输入新密钥以替换" : "输入密钥"} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); void saveMinimax(); } }} /></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy} onClick={() => void saveMinimax()}>{credentialBusy ? "处理中…" : minimax ? "替换并验证" : "安全保存并验证"}</button>{minimax && <button className="btn" disabled={credentialBusy} onClick={() => void deleteMinimax()}>删除密钥</button>}<button className="btn" disabled={!minimax || credentialBusy} onClick={() => void testMinimax()}>测试连接</button><button className="btn" disabled={!minimax || credentialBusy} onClick={() => void loadQuota()}>刷新额度</button></div>{quota && <div className="quota-list">{quota.models.map((model) => <article key={model.model_name}><b>{model.model_name || "未命名模型"}</b><span>当前窗口 {model.interval_remaining_percent == null ? "未知" : `${model.interval_remaining_percent.toFixed(1)}%`} · {model.interval_used ?? "?"}/{model.interval_total ?? "?"}</span><small>重置 {resetTime(model.interval_reset_at_ms)} · 周额度 {model.weekly_remaining_percent == null ? "未知" : `${model.weekly_remaining_percent.toFixed(1)}%`}</small></article>)}</div>}<p>保存后由 Agent Worker 实际读取模型目录完成验证；额度从 MiniMax 官方 Token Plan 接口读取。API Key 不回显，不进入 React 状态、命令行、SQLite 或日志。</p></section>
      <section className="credential-settings"><h3>外部研究数据账号</h3><dl><dt>聚宽</dt><dd className={joinquant ? "text-down" : "text-up"}>{joinquant ? "已配置并即时启用" : "尚未配置"}</dd><dt>调用策略</dt><dd>显式调用 · 严格低频 · 不加入自动行情故障切换</dd></dl><label><span>聚宽用户名</span><input ref={joinquantUserInput} autoComplete="off" spellCheck={false} placeholder={joinquant ? "输入新账号以替换" : "用户名"} /></label><label><span>聚宽密码</span><input ref={joinquantPasswordInput} type="password" autoComplete="off" spellCheck={false} placeholder="密码" /></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy} onClick={() => void saveJoinquant()}>{joinquant ? "替换并启用" : "安全保存并启用"}</button>{joinquant && <button className="btn" disabled={credentialBusy} onClick={() => void deleteJoinquant()}>删除账号</button>}</div><p>用户名和密码均保存于 Windows Credential Manager，只在 Engine 内构造聚宽客户端；北交所不在该接口覆盖范围内。</p></section>
      <section><h3>空间与缓存</h3><dl><dt>缓存合计</dt><dd>{bytes(cache?.total_bytes)}</dd><dt>行情 Parquet</dt><dd>{bytes(cache?.kline_parquet_bytes)} · {cache?.kline_parquet_files ?? "—"} 个文件</dd><dt>工具缓存</dt><dd>{bytes(cache?.tool_cache_bytes)} · {cache?.tool_cache_rows ?? "—"} 条</dd><dt>SQLite</dt><dd>{bytes(cache?.sqlite_bytes)}（始终保留）</dd><dt>Agent/聊天</dt><dd>{bytes(cache?.chat_bytes)}（始终保留）</dd><dt>数据盘可用</dt><dd>{bytes(cache?.disk_free_bytes)}</dd></dl><label><span>清理后上限</span><select value={cleanupTarget} onChange={(event) => setCleanupTarget(Number(event.target.value))}><option value={2048}>2 GiB</option><option value={1024}>1 GiB</option><option value={512}>512 MiB</option><option value={128}>128 MiB</option><option value={0}>仅保留不可删除数据</option></select></label><div className="credential-actions"><button className="btn" disabled={credentialBusy} onClick={() => void cleanupCache()}>预警确认后清理</button><button className="btn" disabled={credentialBusy} onClick={() => void load()}>重新统计</button></div><p>只淘汰过期工具缓存和最久未使用、可重新获取的行情 Parquet；不会删除数据库、Agent 历史、聊天、报告或用户配置。</p></section>
      <section><h3>本地服务</h3><dl><dt>Engine</dt><dd>{engine ? `${engine.status} · v${engine.engine_version}` : "—"}</dd><dt>协议</dt><dd>{engine ? `v${engine.protocol_version}` : "—"}</dd><dt>Host</dt><dd>{runtime.status?.host_version ?? "—"}</dd></dl><div className="provider-mini-list">{providers.map((provider) => <span key={provider.name} className={provider.available && provider.state === "closed" ? "ready" : "degraded"}>{provider.name}</span>)}</div></section>
    </div>
    <DataQualityWorkbench />
  </div>;
}
