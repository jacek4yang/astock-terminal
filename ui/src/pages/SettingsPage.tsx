import { useCallback, useEffect, useState } from "react";
import {
  minimaxSetKey,
  minimaxStatus,
  cacheStats,
  cacheCleanup,
  getDataDir,
  settingsGetProviderStatus,
  settingsSetProviderCredentials,
  settingsGetAgentModelRouting,
  settingsSetAgentModelRouting,
  errMsg,
  type MinimaxStatus,
  type ModelQuotaStatus,
  type QuotaStatus,
  type CacheStats,
  type ProviderStatus,
  type AgentModelRoutingSettings,
} from "../lib/api";
import { fmtBytes } from "../lib/format";
import { ErrorBox, Term } from "../components/ui";
import ProviderHealth from "../components/ProviderHealth";
import NewsEventClusters from "../components/NewsEventClusters";
import SourceEvidenceWorkbench from "../components/SourceEvidenceWorkbench";
import DataQualityWorkbench from "../components/DataQualityWorkbench";
import { applyTheme, useAppStore, type Theme, type UiMode } from "../store";

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="card">
      <div className="card-title">{title}</div>
      <div className="space-y-3 p-4">{children}</div>
    </div>
  );
}

/** 数据源凭证字段定义(secret 项用 password 输入框,绝不回显已存值) */
const CRED_FIELDS: {
  key: keyof ProviderStatus;
  label: string;
  secret: boolean;
  hint: string;
}[] = [
  { key: "tushare_token", label: "Tushare 访问凭证", secret: true, hint: "输入 Tushare 专业版访问凭证" },
  { key: "iwencai_key", label: "问财访问密钥", secret: true, hint: "输入同花顺问财开放接口访问密钥" },
  { key: "jq_user", label: "聚宽账号", secret: false, hint: "输入聚宽(JoinQuant)账号" },
  { key: "jq_pwd", label: "聚宽密码", secret: true, hint: "输入聚宽密码" },
  { key: "socks5", label: "SOCKS5 代理", secret: false, hint: "如 socks5://127.0.0.1:10808" },
];

type CredValues = Record<keyof ProviderStatus, string>;

const EMPTY_CREDS: CredValues = {
  tushare_token: "",
  iwencai_key: "",
  jq_user: "",
  jq_pwd: "",
  socks5: "",
};

const DEFAULT_MODEL_ROUTING: AgentModelRoutingSettings = {
  coordinator_model: "auto",
  fast_model: "auto",
  deep_model: "auto",
  verifier_model: "auto",
  multi_agent_enabled: true,
  max_parallel_agents: 3,
};

const MODEL_ROUTE_FIELDS: {
  key: "coordinator_model" | "fast_model" | "deep_model" | "verifier_model";
  label: string;
  help: string;
}[] = [
  { key: "coordinator_model", label: "主分析师", help: "普通深度研究与最终综合" },
  { key: "fast_model", label: "快速任务", help: "行情快问和低延迟整理" },
  { key: "deep_model", label: "极深任务", help: "计划模式、大额资金和复杂策略" },
  { key: "verifier_model", label: "复核专家", help: "证据、风险、量化和政策独立审计" },
];

function quotaDate(value: number | null): string {
  if (value == null || value <= 0) return "未知";
  const millis = Math.abs(value) < 100_000_000_000 ? value * 1000 : value;
  return new Date(millis).toLocaleString("zh-CN", { hour12: false });
}

function QuotaWindow({
  label,
  used,
  total,
  remainingPercent,
  resetAt,
}: {
  label: string;
  used: number | null;
  total: number | null;
  remainingPercent: number | null;
  resetAt: number | null;
}) {
  const usedPct =
    total != null && total > 0 && used != null
      ? Math.min(100, Math.max(0, (used / total) * 100))
      : remainingPercent != null
        ? Math.min(100, Math.max(0, 100 - remainingPercent))
        : null;
  const remaining = remainingPercent ?? (usedPct == null ? null : 100 - usedPct);
  return (
    <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
      <div className="mb-1.5 flex flex-wrap justify-between gap-2 text-xs">
        <span className="font-medium">{label}</span>
        <span className="num muted">
          {used != null && total != null ? `已用 ${used.toLocaleString()} / ${total.toLocaleString()}` : "请求数暂不可用"}
          {remaining != null ? ` · 剩余 ${remaining.toFixed(1)}%` : ""}
        </span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
        <div
          className={
            "h-full rounded-full transition-all " +
            (usedPct != null && usedPct >= 95
              ? "bg-up"
              : usedPct != null && usedPct >= 80
                ? "bg-amber-500"
                : "bg-blue-600")
          }
          style={{ width: `${usedPct ?? 0}%` }}
        />
      </div>
      <div className="num muted mt-1.5 text-[11px]">窗口重置：{quotaDate(resetAt)}</div>
    </div>
  );
}

function QuotaModel({ item, selected }: { item: ModelQuotaStatus; selected: boolean }) {
  return (
    <div className={"rounded-lg border p-2.5 " + (selected ? "border-blue-300 bg-blue-50/60 dark:border-blue-800 dark:bg-blue-950/20" : "border-slate-200 dark:border-slate-800")}>
      <div className="mb-2 flex items-center gap-2 text-xs">
        <span className="num font-semibold">{item.model_name || "未命名模型"}</span>
        {selected && <span className="chip bg-blue-600/10 text-blue-600 dark:text-blue-400">当前模型</span>}
      </div>
      <div className="grid gap-2 sm:grid-cols-2">
        <QuotaWindow
          label="滚动 5 小时窗口"
          used={item.current_interval_usage_count}
          total={item.current_interval_total_count}
          remainingPercent={item.current_interval_remaining_percent}
          resetAt={item.end_time}
        />
        <QuotaWindow
          label="每周窗口"
          used={item.current_weekly_usage_count}
          total={item.current_weekly_total_count}
          remainingPercent={item.current_weekly_remaining_percent}
          resetAt={item.weekly_end_time}
        />
      </div>
    </div>
  );
}

function QuotaBar({ quota, selectedModel }: { quota: QuotaStatus; selectedModel?: string }) {
  const models = [...quota.models].sort((a, b) => {
    if (a.model_name === selectedModel) return -1;
    if (b.model_name === selectedModel) return 1;
    return a.model_name.localeCompare(b.model_name);
  });
  if (models.length === 0) {
    return <div className="muted text-xs">订阅额度接口已连接，但当前订阅未返回模型额度明细。</div>;
  }
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
        <span className="font-medium">MiniMax 模型订阅额度</span>
        <span className="num muted">快照：{quotaDate(quota.fetched_at)}</span>
      </div>
      {models.map((item) => (
        <QuotaModel key={item.model_name} item={item} selected={item.model_name === selectedModel} />
      ))}
      <div className="muted text-[11px]">额度按 MiniMax 官方滚动窗口统计；本页只在进入、保存 Key 或手动刷新时查询，不会高频轮询。</div>
    </div>
  );
}

export default function SettingsPage() {
  const theme = useAppStore((s) => s.theme);
  const mode = useAppStore((s) => s.mode);
  const setTheme = useAppStore((s) => s.setTheme);
  const setMode = useAppStore((s) => s.setMode);

  const [key, setKey] = useState("");
  const [mmStatus, setMmStatus] = useState<MinimaxStatus | null>(null);
  const [mmMsg, setMmMsg] = useState<string | null>(null);
  const [mmErr, setMmErr] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [modelRouting, setModelRouting] = useState<AgentModelRoutingSettings>(DEFAULT_MODEL_ROUTING);
  const [modelRoutingMsg, setModelRoutingMsg] = useState<string | null>(null);
  const [modelRoutingSaving, setModelRoutingSaving] = useState(false);

  const [dataDir, setDataDir] = useState<string | null>(null);
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [cacheMsg, setCacheMsg] = useState<string | null>(null);
  const [cacheErr, setCacheErr] = useState<string | null>(null);
  const [cleaning, setCleaning] = useState(false);

  const [creds, setCreds] = useState<CredValues>(EMPTY_CREDS);
  const [provStatus, setProvStatus] = useState<ProviderStatus | null>(null);
  const [provMsg, setProvMsg] = useState<string | null>(null);
  const [provErr, setProvErr] = useState<string | null>(null);
  const [provSaving, setProvSaving] = useState(false);

  const loadMinimax = useCallback(async () => {
    try {
      setMmStatus(await minimaxStatus());
      setMmErr(null);
    } catch (e) {
      setMmErr(errMsg(e));
    }
  }, []);

  const loadCache = useCallback(async () => {
    try {
      setCache(await cacheStats());
      setCacheErr(null);
    } catch (e) {
      setCacheErr(errMsg(e));
    }
    try {
      setDataDir(await getDataDir());
    } catch {
      /* 非致命 */
    }
  }, []);

  const loadProviders = useCallback(async () => {
    try {
      setProvStatus(await settingsGetProviderStatus());
      setProvErr(null);
    } catch (e) {
      setProvErr(errMsg(e));
    }
  }, []);

  const loadModelRouting = useCallback(async () => {
    try {
      setModelRouting(await settingsGetAgentModelRouting());
    } catch (e) {
      setMmErr(errMsg(e));
    }
  }, []);

  useEffect(() => {
    loadMinimax();
    loadCache();
    loadProviders();
    loadModelRouting();
  }, [loadMinimax, loadCache, loadProviders, loadModelRouting]);

  const saveModelRouting = async () => {
    setModelRoutingSaving(true);
    setModelRoutingMsg(null);
    try {
      const saved = await settingsSetAgentModelRouting(modelRouting);
      setModelRouting(saved);
      setModelRoutingMsg("模型路由与多专家上限已保存，下一轮研究任务生效");
    } catch (e) {
      setMmErr(errMsg(e));
    } finally {
      setModelRoutingSaving(false);
    }
  };

  // 逐项提交五个字段;空串 = 清除该项(后端契约,缺失/空值即清除,绝不回显)
  const saveProviders = async () => {
    setProvSaving(true);
    setProvMsg(null);
    try {
      const r = await settingsSetProviderCredentials({
        tushare_token: creds.tushare_token.trim(),
        iwencai_key: creds.iwencai_key.trim(),
        jq_user: creds.jq_user.trim(),
        jq_pwd: creds.jq_pwd.trim(),
        socks5: creds.socks5.trim(),
      });
      setProvStatus(r.status);
      setCreds(EMPTY_CREDS);
      setProvMsg(`${r.message};部分数据源需重启生效`);
    } catch (e) {
      setProvErr(errMsg(e));
    } finally {
      setProvSaving(false);
    }
  };

  const saveKey = async () => {
    if (!key.trim()) return;
    setSaving(true);
    setMmMsg(null);
    try {
      await minimaxSetKey(key.trim());
      setKey("");
      setMmMsg("访问密钥已安全保存（存入 Windows 凭据管理器，不会回显）");
      await loadMinimax();
    } catch (e) {
      setMmErr(errMsg(e));
    } finally {
      setSaving(false);
    }
  };

  const cleanup = async () => {
    setCleaning(true);
    setCacheMsg(null);
    try {
      const r = await cacheCleanup(512);
      setCacheMsg(`已清理 ${fmtBytes(r.freed_bytes)},删除 ${r.removed_files} 个文件`);
      await loadCache();
    } catch (e) {
      setCacheErr(errMsg(e));
    } finally {
      setCleaning(false);
    }
  };

  const changeTheme = (t: Theme) => {
    setTheme(t);
    applyTheme(t);
  };
  const changeMode = (m: UiMode) => setMode(m);

  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-3xl space-y-3 p-4">
        <h1 className="text-sm font-semibold text-slate-700 dark:text-slate-300">设置</h1>

        {/* 数据源健康(5s 轮询) */}
        <ProviderHealth />
        <DataQualityWorkbench />
        <NewsEventClusters />
        <SourceEvidenceWorkbench />

      {/* MiniMax 访问密钥 */}
      <Section title="MiniMax 访问密钥">
        {mmErr && <ErrorBox message={mmErr} onRetry={loadMinimax} />}
        <div className="flex flex-wrap items-center gap-2">
          <input
            className="input w-80"
            type="password"
            placeholder={mmStatus?.has_key ? "已保存（已脱敏，输入新密钥可覆盖）" : "输入访问密钥"}
            value={key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && saveKey()}
          />
          <button className="btn-primary" onClick={saveKey} disabled={saving || !key.trim()}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
        {mmMsg && <div className="text-xs text-down">{mmMsg}</div>}
        {mmStatus && (
          <div className="space-y-2 text-xs">
            <div className="flex flex-wrap gap-x-6 gap-y-1">
              <span>
                状态:{" "}
                {mmStatus.has_key ? (
                  <span className="text-down">已配置(sk-••••••)</span>
                ) : (
                  <span className="text-amber-500">未配置</span>
                )}
              </span>
              {mmStatus.region && (
                <span>
                  区域:<span className="num">{mmStatus.region}</span>
                </span>
              )}
              {mmStatus.api_host && (
                <span>
                  接口:<span className="num">{mmStatus.api_host}</span>
                </span>
              )}
              {mmStatus.model && (
                <span>
                  模型:<span className="num">{mmStatus.model}</span>
                </span>
              )}
              <button className="btn !px-2 !py-0.5" onClick={loadMinimax} disabled={saving}>
                刷新服务与额度
              </button>
            </div>
            {mmStatus.quota ? (
              <QuotaBar quota={mmStatus.quota} selectedModel={mmStatus.model} />
            ) : mmStatus.has_key ? (
              <div className="rounded border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs text-amber-700 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
                MiniMax 访问密钥已配置，但订阅额度服务暂不可达或该账号未开通模型套餐。可稍后手动刷新；额度耗尽时分析任务会安全挂起（错误码 2056）。
              </div>
            ) : null}
          </div>
        )}
      </Section>

      <Section title="Agent 模型路由与多专家协作">
        <div className="muted text-xs leading-relaxed">
          模型列表来自 MiniMax 官方接口，不写死版本。选择“auto”由系统自动探测；也可直接填写未来新增的模型 ID。
        </div>
        <datalist id="minimax-model-options">
          <option value="auto">自动选择可用模型</option>
          {(mmStatus?.available_models ?? []).map((model) => (
            <option key={model.id} value={model.id} />
          ))}
        </datalist>
        <div className="grid gap-2 sm:grid-cols-2">
          {MODEL_ROUTE_FIELDS.map((field) => (
            <label key={field.key} className="rounded border border-slate-200 p-2.5 text-xs dark:border-slate-800">
              <span className="font-medium">{field.label}</span>
              <span className="muted ml-2">{field.help}</span>
              <input
                className="input mt-2 w-full"
                list="minimax-model-options"
                value={modelRouting[field.key]}
                onChange={(event) =>
                  setModelRouting((routing) => ({ ...routing, [field.key]: event.target.value }))
                }
              />
            </label>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-4 rounded border border-slate-200 p-2.5 text-xs dark:border-slate-800">
          <label className="flex cursor-pointer items-center gap-2">
            <input
              type="checkbox"
              checked={modelRouting.multi_agent_enabled}
              onChange={(event) =>
                setModelRouting((routing) => ({
                  ...routing,
                  multi_agent_enabled: event.target.checked,
                }))
              }
            />
            深度任务启用独立专家复核
          </label>
          <label className="flex items-center gap-2">
            <span>极深任务最多并行专家</span>
            <select
              className="input !w-20"
              value={modelRouting.max_parallel_agents}
              onChange={(event) =>
                setModelRouting((routing) => ({
                  ...routing,
                  max_parallel_agents: Number(event.target.value),
                }))
              }
            >
              {[1, 2, 3, 4].map((count) => (
                <option key={count} value={count}>{count}</option>
              ))}
            </select>
          </label>
          <span className="muted">
            深入模式最多 2 位，极深/计划模式按此上限；主分析师统一工具权限并负责最终结论。
          </span>
        </div>
        <div className="flex items-center gap-3">
          <button className="btn-primary" onClick={saveModelRouting} disabled={modelRoutingSaving}>
            {modelRoutingSaving ? "保存中…" : "保存模型路由"}
          </button>
          {modelRoutingMsg && <span className="text-xs text-down">{modelRoutingMsg}</span>}
        </div>
      </Section>

      <Section title="访问频率与重试治理">
        <div className="grid gap-3 text-xs lg:grid-cols-2">
          <div className="rounded-lg border border-slate-200 p-3 dark:border-slate-800">
            <div className="font-medium">MiniMax 智能助手</div>
            <div className="muted mt-2 space-y-1 leading-relaxed">
              <div>· 最多 4 条推理流并发；单任务的模型轮次始终串行，工具最多 6 项并行。</div>
              <div>· 网络连接失败或访问过快时最多重试 5 次，从 1 秒起逐步延长并随机错峰，单次最多等待 60 秒；优先遵循服务器建议的等待时间。</div>
              <div>· 已有部分输出后不盲目重放流，避免重复工具调用；任务链会持久化并支持安全恢复。</div>
              <div>· 额度快照缓存 30 秒；滚动额度剩余 ≤20% 时跨任务间隔 2 秒，≤5% 时 10 秒，耗尽则挂起。</div>
            </div>
          </div>
          <div className="rounded-lg border border-slate-200 p-3 dark:border-slate-800">
            <div className="font-medium">行情与研究数据源</div>
            <div className="muted mt-2 space-y-1 leading-relaxed">
              <div>· 同一上游主机的健康基线间隔 75 毫秒；所有页面、扫描与 Agent 共享调度器，不会瞬时突发。</div>
              <div>· 超时、访问过快、服务端临时异常或防护拦截后会自动放慢请求，间隔范围为 100 毫秒至 30 秒；成功后逐步恢复。</div>
              <div>· 单请求 8 秒超时；主机池切换间隔 300 毫秒；连续 3 次瞬态失败触发熔断。</div>
              <div>· 熔断冷却从 10 分钟指数增长到 1 小时；实时/K线/搜索/全市场/宽度缓存分别为 2/15/15/60/120 秒。</div>
              <div>· 财经快讯最多 3 路并发、瞬时失败重试 1 次，并按来源的 2-30 分钟更新周期缓存；失败会保留最后成功副本。</div>
              <div>· 聚宽研究调用全局串行且至少间隔 2 秒，只执行日线、估值、指数成分和宏观数据固定模板。</div>
            </div>
          </div>
        </div>
        <div className="rounded border border-blue-200 bg-blue-50 px-2.5 py-2 text-xs text-blue-700 dark:border-blue-900/60 dark:bg-blue-950/30 dark:text-blue-300">
          数据策略采用“字段级合并 + 质量校验 + 主备降级”：会利用已配置的数据源补齐名称、价格、换手、基本面与事件字段，但不会为同一已命中缓存的字段无意义地同时轰击所有上游。
        </div>
      </Section>

      {/* 数据源凭证(可选 provider;状态只回布尔,凭证本体绝不回显) */}
      <Section title="数据源凭证">
        {provErr && <ErrorBox message={provErr} onRetry={loadProviders} />}
        <div className="muted text-xs">
          可选数据源的凭证与代理,保存后立即写入进程环境变量。已存值绝不回显;某行留空并保存 = 清除该项。
        </div>
        <div className="space-y-2">
          {CRED_FIELDS.map((f) => (
            <div key={f.key} className="flex flex-wrap items-center gap-2">
              <span className="w-24 shrink-0 text-xs">{f.label}</span>
              {provStatus &&
                (provStatus[f.key] ? (
                  <span className="text-xs text-down">已配置</span>
                ) : (
                  <span className="text-xs text-amber-500">未配置</span>
                ))}
              <input
                className="input w-72"
                type={f.secret ? "password" : "text"}
                placeholder={
                  provStatus?.[f.key] ? "已配置(输入新值覆盖,留空保存=清除)" : f.hint
                }
                value={creds[f.key]}
                onChange={(e) => setCreds((c) => ({ ...c, [f.key]: e.target.value }))}
                autoComplete="off"
              />
            </div>
          ))}
        </div>
        <div className="flex items-center gap-3">
          <button className="btn-primary" onClick={saveProviders} disabled={provSaving}>
            {provSaving ? "保存中…" : "保存"}
          </button>
          {provMsg && <span className="text-xs text-down">{provMsg}</span>}
        </div>
      </Section>

      {/* 数据目录与缓存 */}
      <Section title="数据与缓存">
        <div className="text-xs">
          <span className="muted">数据目录:</span>
          <span className="num break-all">{dataDir ?? "加载中…"}</span>
        </div>
        {cacheErr && <ErrorBox message={cacheErr} onRetry={loadCache} />}
        {cache && (
          <div className="grid grid-cols-2 gap-x-6 gap-y-1 text-xs sm:grid-cols-3">
            <span>
              K线缓存:<span className="num">{fmtBytes(cache.kline_bytes)}</span>
            </span>
            <span>
              数据库:<span className="num">{fmtBytes(cache.sqlite_bytes)}</span>
            </span>
            <span>
              工具缓存:<span className="num">{fmtBytes(cache.tool_cache_bytes)}</span>
            </span>
            <span>
              对话记录:<span className="num">{fmtBytes(cache.chat_bytes)}</span>
            </span>
            <span className="font-medium">
              合计:<span className="num">{fmtBytes(cache.total_bytes)}</span>
            </span>
            {cache.disk_free_bytes != null && (
              <span>
                磁盘可用:<span className="num">{fmtBytes(cache.disk_free_bytes)}</span>
              </span>
            )}
          </div>
        )}
        <div className="flex items-center gap-3">
          <button className="btn" onClick={cleanup} disabled={cleaning}>
            {cleaning ? "清理中…" : "清理缓存(保留最近 512MB)"}
          </button>
          {cacheMsg && <span className="text-xs text-down">{cacheMsg}</span>}
        </div>
      </Section>

      {/* 外观与模式 */}
      <Section title="外观与模式">
        <div className="flex flex-wrap items-center gap-6">
          <div className="flex items-center gap-2 text-sm">
            <span className="muted text-xs">主题</span>
            <div className="seg">
              <button
                className={"seg-item" + (theme === "dark" ? " seg-item-active" : "")}
                onClick={() => changeTheme("dark")}
              >
                深色
              </button>
              <button
                className={"seg-item" + (theme === "light" ? " seg-item-active" : "")}
                onClick={() => changeTheme("light")}
              >
                浅色
              </button>
            </div>
          </div>
          <div className="flex items-center gap-2 text-sm">
            <span className="muted text-xs">
              <Term label="显示模式" tip="专业模式显示全部分析模块;小白模式只看大白话总结、操作建议与风险提示" />
            </span>
            <div className="seg">
              <button
                className={"seg-item" + (mode === "pro" ? " seg-item-active" : "")}
                onClick={() => changeMode("pro")}
              >
                专业
              </button>
              <button
                className={"seg-item" + (mode === "simple" ? " seg-item-active" : "")}
                onClick={() => changeMode("simple")}
              >
                小白
              </button>
            </div>
          </div>
        </div>
      </Section>
      </div>
    </div>
  );
}
