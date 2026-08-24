import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { requestNative } from "../bridge";
import DataQualityWorkbench from "../components/DataQualityWorkbench";
import { getProviderHealth, type AgentModelRoutingSettings, type ProviderHealthItem } from "../lib/api";
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
type ProviderTest = { provider: string; model: string; fast_model?: string; deep_model?: string; verifier_model?: string; available_models?: string[]; api_host: string; catalog_verified: boolean };
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

type DataRootMigration = {
  data_dir: string;
  manifest_path: string;
  files_copied: number;
  bytes_copied: number;
  sqlite_integrity: string;
  source_retained: boolean;
  restart_required: boolean;
  compatibility_warning?: string | null;
};

type DataRootRollback = {
  data_dir: string;
  migrated_copy: string;
  source_sqlite_integrity: string;
  source_retained: boolean;
  migrated_copy_retained: boolean;
  restart_required: boolean;
};

type OptionalProviderState = {
  configured: boolean;
  active: boolean;
  restart_required: boolean;
};

type OptionalProviders = Partial<Record<"tushare" | "iwencai" | "sec_edgar" | "socks5", OptionalProviderState>>;

const DEFAULT_MODEL_ROUTING: AgentModelRoutingSettings = {
  coordinator_model: "auto",
  fast_model: "auto",
  deep_model: "auto",
  verifier_model: "auto",
  multi_agent_enabled: true,
  max_parallel_agents: 3,
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
  const [optionalProviders, setOptionalProviders] = useState<OptionalProviders>({});
  const [quota, setQuota] = useState<QuotaStatus | null>(null);
  const [providerTest, setProviderTest] = useState<ProviderTest | null>(null);
  const [modelRouting, setModelRouting] = useState<AgentModelRoutingSettings>(DEFAULT_MODEL_ROUTING);
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [cleanupTarget, setCleanupTarget] = useState(512);
  const [dataTarget, setDataTarget] = useState("D:\\astock-data\\astock-terminal");
  const [migration, setMigration] = useState<DataRootMigration | null>(null);
  const [credentialBusy, setCredentialBusy] = useState(false);
  const [credentialMessage, setCredentialMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const minimaxInput = useRef<HTMLInputElement | null>(null);
  const joinquantUserInput = useRef<HTMLInputElement | null>(null);
  const joinquantPasswordInput = useRef<HTMLInputElement | null>(null);
  const tushareInput = useRef<HTMLInputElement | null>(null);
  const iwencaiInput = useRef<HTMLInputElement | null>(null);
  const secEdgarInput = useRef<HTMLInputElement | null>(null);
  const socks5Input = useRef<HTMLInputElement | null>(null);

  const load = useCallback(async () => {
    const [engineState, providerRows, credentials, cacheStats, routing] = await Promise.all([
      requestNative<EngineStatus>("engine", "diagnostics.status"),
      getProviderHealth(),
      requestNative<{ providers: { minimax: boolean; joinquant?: boolean; optional?: OptionalProviders } }>("engine", "credentials.status"),
      requestNative<CacheStats>("engine", "storage.cache.stats"),
      requestNative<AgentModelRoutingSettings>("engine", "settings.agent_models.get"),
    ]);
    setEngine(engineState); setProviders(providerRows); setMinimax(credentials.providers.minimax); setJoinquant(Boolean(credentials.providers.joinquant)); setOptionalProviders(credentials.providers.optional ?? {}); setCache(cacheStats); setModelRouting(routing); setError(null);
    void requestNative("agent", "agent.provider.configure", { routing, validate: false }).catch(() => {});
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

  const migrateDataRoot = async () => {
    const destination = dataTarget.trim();
    if (!destination || credentialBusy) return;
    if (!window.confirm(`将对当前研究数据库做在线备份，并逐文件校验后切换到：\n${destination}\n\n当前目录不会删除；桌面应用重启后才使用新目录。继续吗？`)) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null); setMigration(null);
    try {
      const result = await requestNative<DataRootMigration>("engine", "storage.data_root.migrate", { destination }, { deadlineMs: 30 * 60_000 });
      setMigration(result);
      setCredentialMessage(`研究数据迁移与校验完成：${result.files_copied} 个文件、${bytes(result.bytes_copied)}，SQLite ${result.sqlite_integrity}。旧目录完整保留；请重启桌面应用完成切换。`);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const rollbackDataRoot = async () => {
    if (credentialBusy) return;
    if (!window.confirm("将把下次启动的数据目录恢复到迁移前的旧副本。迁移后的副本也会完整保留，但迁移后新增的数据不会自动合并回旧副本。继续吗？")) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const result = await requestNative<DataRootRollback>("engine", "storage.data_root.rollback", {}, { deadlineMs: 120_000 });
      setCredentialMessage(`回滚指针已安全切换到 ${result.data_dir}；SQLite ${result.source_sqlite_integrity}。旧副本和迁移副本均未删除，请重启桌面应用完成切换。`);
      setMigration(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const saveOptionalProvider = async (
    provider: "tushare" | "iwencai" | "sec_edgar" | "socks5",
    input: RefObject<HTMLInputElement | null>,
  ) => {
    const value = input.current?.value.trim() ?? "";
    if (!value || credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const result = await requestNative<{ restart_required: boolean; message: string }>("engine", "credentials.provider.set", { provider, value });
      if (input.current) input.current.value = "";
      setCredentialMessage(result.message);
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const deleteOptionalProvider = async (provider: "tushare" | "iwencai" | "sec_edgar" | "socks5") => {
    if (credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const result = await requestNative<{ restart_required: boolean }>("engine", "credentials.provider.delete", { provider });
      setCredentialMessage(result.restart_required ? "凭据已删除；重启桌面应用后彻底停用该数据源。" : "凭据已删除并在当前进程停用。" );
      await load();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setCredentialBusy(false);
    }
  };

  const saveModelRouting = async () => {
    if (credentialBusy) return;
    setCredentialBusy(true); setCredentialMessage(null); setError(null);
    try {
      const effective = await requestNative<ProviderTest>("agent", "agent.provider.configure", { routing: modelRouting, validate: minimax }, { deadlineMs: 90_000 });
      const saved = await requestNative<AgentModelRoutingSettings>("engine", "settings.agent_models.set", modelRouting);
      setModelRouting(saved);
      if (effective.catalog_verified) setProviderTest(effective);
      setCredentialMessage(effective.catalog_verified ? `模型路由已按 MiniMax 官方目录验证并保存；最终综合使用 ${effective.deep_model ?? effective.model}。` : "模型路由已保存；配置 MiniMax Plus 后可验证实际可用模型。" );
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      void requestNative("agent", "agent.provider.configure", { routing: await requestNative<AgentModelRoutingSettings>("engine", "settings.agent_models.get"), validate: false }).catch(() => {});
    } finally {
      setCredentialBusy(false);
    }
  };

  const optionalCredentialRows = [
    { id: "tushare" as const, label: "Tushare Pro Token", ref: tushareInput, type: "password", placeholder: "输入 Tushare Token" },
    { id: "iwencai" as const, label: "问财 OpenAPI Key", ref: iwencaiInput, type: "password", placeholder: "输入问财 OpenAPI Key" },
    { id: "sec_edgar" as const, label: "SEC Fair Access User-Agent", ref: secEdgarInput, type: "text", placeholder: "应用名称 contact@example.com" },
    { id: "socks5" as const, label: "境外来源 SOCKS5", ref: socks5Input, type: "password", placeholder: "socks5h://host:port" },
  ];

  return <div className="settings-workbench">
    <div className="settings-heading"><div><span className="eyebrow">DESKTOP SETTINGS</span><h2>配置</h2><p>界面、数据、模型凭据和本地运行状态集中在这里。</p></div><span className={`status-pill ${runtime.status ? "phase-completed" : ""}`}>{runtime.status ? "桌面服务已连接" : "正在连接"}</span></div>
    {error && <p className="inline-warning">{error}</p>}
    {credentialMessage && <p className="settings-success">{credentialMessage}</p>}
    <div className="settings-grid">
      <section><h3>界面与布局</h3><label><span>主题</span><select value={theme} onChange={(event) => setTheme(event.target.value as "dark" | "light")}><option value="dark">深色看盘</option><option value="light">浅色界面</option></select></label><label><span>信息密度</span><select value={mode} onChange={(event) => setMode(event.target.value as "pro" | "simple")}><option value="simple">普通股民模式</option><option value="pro">专业完整模式</option></select></label><button className="btn" onClick={resetLayout}>恢复默认比例</button><p>大盘主图与全市场列表之间的分隔线可拖动，比例会自动保存并随窗口响应。</p></section>
      <section><h3>研究数据</h3><dl><dt>数据目录</dt><dd>{engine?.data_root?.path ?? "读取中…"}</dd><dt>接管方式</dt><dd>{engine?.data_root?.origin ?? "—"}</dd><dt>旧目录</dt><dd>{engine?.data_root?.legacy_path ?? "无"}</dd></dl><p>历史数据库原地接管，不自动复制或删除大型 SQLite/Parquet 数据。</p></section>
      <section className="credential-settings"><h3>迁移研究数据到 D 盘</h3><label><span>全新目标目录</span><input value={dataTarget} onChange={(event) => setDataTarget(event.target.value)} spellCheck={false} /></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy || !dataTarget.trim()} onClick={() => void migrateDataRoot()}>{credentialBusy ? "正在备份与校验…" : "备份、校验并准备切换"}</button>{(migration || engine?.data_root?.origin === "migrated_redirect") && <button className="btn" disabled={credentialBusy} onClick={() => void rollbackDataRoot()}>恢复迁移前目录</button>}</div>{migration && <dl><dt>新目录</dt><dd>{migration.data_dir}</dd><dt>校验清单</dt><dd>{migration.manifest_path}</dd><dt>旧副本</dt><dd>{migration.source_retained ? "完整保留" : "异常"}</dd></dl>}<p>目标必须是尚不存在的绝对路径。程序使用 SQLite 在线备份、完整性检查和逐文件 SHA-256 清单，同卷原子完成新目录；运行中的 Engine 不切换，重启后生效。回滚也只切换经过校验的指针，两个副本都不会自动删除。</p></section>
      <section className="credential-settings"><h3>Agent 与 MiniMax Plus</h3><dl><dt>MiniMax Plus</dt><dd className={minimax ? "text-down" : "text-up"}>{minimax ? "Credential Manager 已配置" : "尚未配置"}</dd><dt>Agent Worker</dt><dd>{runtime.status?.agent ? `${runtime.status.agent.status} · PID ${runtime.status.agent.pid}` : "—"}</dd>{providerTest && <><dt>已验证模型</dt><dd>{providerTest.model}</dd><dt>服务区域</dt><dd>{providerTest.api_host.includes("minimaxi.com") ? "中国大陆" : "国际"}</dd></>}</dl><label><span>MiniMax Plus Key</span><input ref={minimaxInput} type="password" autoComplete="off" spellCheck={false} placeholder={minimax ? "输入新密钥以替换" : "输入密钥"} onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); void saveMinimax(); } }} /></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy} onClick={() => void saveMinimax()}>{credentialBusy ? "处理中…" : minimax ? "替换并验证" : "安全保存并验证"}</button>{minimax && <button className="btn" disabled={credentialBusy} onClick={() => void deleteMinimax()}>删除密钥</button>}<button className="btn" disabled={!minimax || credentialBusy} onClick={() => void testMinimax()}>测试连接</button><button className="btn" disabled={!minimax || credentialBusy} onClick={() => void loadQuota()}>刷新额度</button></div>{quota && <div className="quota-list">{quota.models.map((model) => <article key={model.model_name}><b>{model.model_name || "未命名模型"}</b><span>当前窗口 {model.interval_remaining_percent == null ? "未知" : `${model.interval_remaining_percent.toFixed(1)}%`} · {model.interval_used ?? "?"}/{model.interval_total ?? "?"}</span><small>重置 {resetTime(model.interval_reset_at_ms)} · 周额度 {model.weekly_remaining_percent == null ? "未知" : `${model.weekly_remaining_percent.toFixed(1)}%`}</small></article>)}</div>}<p>保存后由 Agent Worker 实际读取模型目录完成验证；额度从 MiniMax 官方 Token Plan 接口读取。API Key 不回显，不进入 React 状态、命令行、SQLite 或日志。</p></section>
      <section className="credential-settings"><h3>Agent 模型路由</h3><datalist id="desktop-minimax-models">{(providerTest?.available_models ?? []).map((model) => <option key={model} value={model} />)}</datalist>{([['coordinator_model','主分析师'],['fast_model','结构化快速模型'],['deep_model','最终深度综合'],['verifier_model','反方复核模型']] as const).map(([field, label]) => <label key={field}><span>{label}</span><input list="desktop-minimax-models" value={modelRouting[field]} onChange={(event) => setModelRouting((current) => ({ ...current, [field]: event.target.value }))} placeholder="auto" /></label>)}<label><span>独立复核</span><select value={modelRouting.multi_agent_enabled ? "enabled" : "disabled"} onChange={(event) => setModelRouting((current) => ({ ...current, multi_agent_enabled: event.target.value === "enabled" }))}><option value="enabled">启用</option><option value="disabled">停用</option></select></label><label><span>最大并行专家</span><select value={modelRouting.max_parallel_agents} onChange={(event) => setModelRouting((current) => ({ ...current, max_parallel_agents: Number(event.target.value) }))}>{[1,2,3,4].map((value) => <option key={value} value={value}>{value}</option>)}</select></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy} onClick={() => void saveModelRouting()}>验证并保存路由</button></div><p>“auto”由 MiniMax 官方模型目录选择。显式模型必须实际出现在当前账号目录中；不可用配置会被拒绝，不会静默回退。最终报告使用深度模型，结构化重试由快速、主分析师和复核模型有界接力。</p></section>
      <section className="credential-settings"><h3>外部研究数据账号</h3><dl><dt>聚宽</dt><dd className={joinquant ? "text-down" : "text-up"}>{joinquant ? "已配置并即时启用" : "尚未配置"}</dd><dt>调用策略</dt><dd>显式调用 · 严格低频 · 不加入自动行情故障切换</dd></dl><label><span>聚宽用户名</span><input ref={joinquantUserInput} autoComplete="off" spellCheck={false} placeholder={joinquant ? "输入新账号以替换" : "用户名"} /></label><label><span>聚宽密码</span><input ref={joinquantPasswordInput} type="password" autoComplete="off" spellCheck={false} placeholder="密码" /></label><div className="credential-actions"><button className="btn-primary" disabled={credentialBusy} onClick={() => void saveJoinquant()}>{joinquant ? "替换并启用" : "安全保存并启用"}</button>{joinquant && <button className="btn" disabled={credentialBusy} onClick={() => void deleteJoinquant()}>删除账号</button>}</div><p>用户名和密码均保存于 Windows Credential Manager，只在 Engine 内构造聚宽客户端；北交所不在该接口覆盖范围内。</p></section>
      <section className="credential-settings"><h3>可选研究来源与境外代理</h3>{optionalCredentialRows.map((row) => { const state = optionalProviders[row.id]; return <div key={row.id} className="provider-credential-row"><div><b>{row.label}</b><span className={state?.active ? "text-down" : state?.configured ? "text-amber-500" : "text-up"}>{state?.active ? "当前进程已启用" : state?.configured ? "已保存 · 重启后启用" : "未配置"}</span></div><label><span className="sr-only">{row.label}</span><input ref={row.ref} type={row.type} autoComplete="off" spellCheck={false} placeholder={state?.configured ? "输入新值以替换" : row.placeholder} /></label><div className="credential-actions"><button className="btn" disabled={credentialBusy} onClick={() => void saveOptionalProvider(row.id, row.ref)}>安全保存</button>{state?.configured && <button className="btn" disabled={credentialBusy} onClick={() => void deleteOptionalProvider(row.id)}>删除</button>}</div></div>; })}<p>所有值只写入 Windows Credential Manager，不写入 SQLite、命令行、React 状态或日志。Tushare、问财与代理由客户端启动时捕获，因此更改后需重启；SEC 身份会即时生效且不会由程序伪造。</p></section>
      <section><h3>空间与缓存</h3><dl><dt>缓存合计</dt><dd>{bytes(cache?.total_bytes)}</dd><dt>行情 Parquet</dt><dd>{bytes(cache?.kline_parquet_bytes)} · {cache?.kline_parquet_files ?? "—"} 个文件</dd><dt>工具缓存</dt><dd>{bytes(cache?.tool_cache_bytes)} · {cache?.tool_cache_rows ?? "—"} 条</dd><dt>SQLite</dt><dd>{bytes(cache?.sqlite_bytes)}（始终保留）</dd><dt>Agent/聊天</dt><dd>{bytes(cache?.chat_bytes)}（始终保留）</dd><dt>数据盘可用</dt><dd>{bytes(cache?.disk_free_bytes)}</dd></dl><label><span>清理后上限</span><select value={cleanupTarget} onChange={(event) => setCleanupTarget(Number(event.target.value))}><option value={2048}>2 GiB</option><option value={1024}>1 GiB</option><option value={512}>512 MiB</option><option value={128}>128 MiB</option><option value={0}>仅保留不可删除数据</option></select></label><div className="credential-actions"><button className="btn" disabled={credentialBusy} onClick={() => void cleanupCache()}>预警确认后清理</button><button className="btn" disabled={credentialBusy} onClick={() => void load()}>重新统计</button></div><p>只淘汰过期工具缓存和最久未使用、可重新获取的行情 Parquet；不会删除数据库、Agent 历史、聊天、报告或用户配置。</p></section>
      <section><h3>本地服务</h3><dl><dt>Engine</dt><dd>{engine ? `${engine.status} · v${engine.engine_version}` : "—"}</dd><dt>协议</dt><dd>{engine ? `v${engine.protocol_version}` : "—"}</dd><dt>Host</dt><dd>{runtime.status?.host_version ?? "—"}</dd></dl><div className="provider-mini-list">{providers.map((provider) => <span key={provider.name} className={provider.available && provider.state === "closed" ? "ready" : "degraded"}>{provider.name}</span>)}</div></section>
    </div>
    <DataQualityWorkbench />
  </div>;
}
