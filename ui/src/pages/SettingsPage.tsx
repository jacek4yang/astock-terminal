import { useCallback, useEffect, useState } from "react";
import {
  minimaxSetKey,
  minimaxStatus,
  cacheStats,
  cacheCleanup,
  getDataDir,
  settingsGetProviderStatus,
  settingsSetProviderCredentials,
  errMsg,
  type MinimaxStatus,
  type CacheStats,
  type ProviderStatus,
} from "../lib/api";
import { fmtBytes } from "../lib/format";
import { ErrorBox, Term } from "../components/ui";
import ProviderHealth from "../components/ProviderHealth";
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
  { key: "tushare_token", label: "Tushare Token", secret: true, hint: "输入 Tushare Pro Token" },
  { key: "iwencai_key", label: "问财 Key", secret: true, hint: "输入同花顺问财 OpenAPI Key" },
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

function QuotaBar({ quota }: { quota: Record<string, unknown> }) {
  const used = typeof quota.used === "number" ? quota.used : null;
  const total = typeof quota.total === "number" ? quota.total : null;
  if (used == null || total == null || total <= 0) {
    return (
      <div className="muted text-xs">
        配额:{Object.entries(quota)
          .filter(([, v]) => typeof v === "number" || typeof v === "string")
          .map(([k, v]) => `${k}=${String(v)}`)
          .join(" · ") || "无数据"}
      </div>
    );
  }
  const pct = Math.min(100, Math.round((used / total) * 100));
  return (
    <div>
      <div className="mb-1 flex justify-between text-xs">
        <span className="muted">配额用量</span>
        <span className="num">
          {used} / {total}({pct}%)
        </span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
        <div
          className={"h-full rounded-full " + (pct >= 90 ? "bg-up" : pct >= 70 ? "bg-amber-500" : "bg-blue-600")}
          style={{ width: `${pct}%` }}
        />
      </div>
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

  useEffect(() => {
    loadMinimax();
    loadCache();
    loadProviders();
  }, [loadMinimax, loadCache, loadProviders]);

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
      setMmMsg("Key 已安全保存(存入 Windows 凭据管理器,不会回显)");
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

      {/* MiniMax Key */}
      <Section title="MiniMax API Key">
        {mmErr && <ErrorBox message={mmErr} onRetry={loadMinimax} />}
        <div className="flex flex-wrap items-center gap-2">
          <input
            className="input w-80"
            type="password"
            placeholder={mmStatus?.has_key ? "已保存(脱敏,输入新 Key 覆盖)" : "输入 API Key"}
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
            </div>
            {mmStatus.quota && <QuotaBar quota={mmStatus.quota} />}
          </div>
        )}
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
