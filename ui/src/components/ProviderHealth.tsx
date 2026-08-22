import { useCallback, useEffect, useState } from "react";
import {
  getNewsIngestObservations,
  getNewsProviderHealth,
  getProviderHealth,
  setNewsProviderEnabled,
  errMsg,
  type NewsDeliveryMode,
  type NewsIngestObservation,
  type NewsProviderHealthItem,
  type ProviderHealthItem,
} from "../lib/api";
import { Term } from "./ui";

const STATE_META: Record<string, { label: string; dot: string; text: string }> = {
  closed: { label: "可用", dot: "bg-down", text: "text-down" },
  open: { label: "熔断中", dot: "bg-up", text: "text-up" },
  half_open: { label: "试探恢复", dot: "bg-amber-500", text: "text-amber-500" },
};

function metaOf(state: string) {
  return STATE_META[state] ?? { label: state, dot: "bg-slate-400", text: "muted" };
}

const MODE_LABELS: Record<NewsDeliveryMode, string> = {
  push_stream: "推送/流式",
  scheduled_index: "定时索引",
  published_incremental: "按发布时间增量",
};

const ERROR_LABELS: Record<string, string> = {
  configuration: "配置错误",
  authentication: "身份验证失败",
  rate_limited: "访问过快",
  timeout: "连接超时",
  network: "网络异常",
  parse: "内容解析失败",
  empty: "没有返回内容",
  circuit_open: "熔断冷却中",
  disabled: "用户已停用",
  storage: "状态保存失败",
};

function dateTime(seconds: number | null): string {
  if (seconds == null) return "尚无成功记录";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

function pct(value: number): string {
  return `${(Math.max(0, Math.min(1, value)) * 100).toFixed(1)}%`;
}

/** 数据源健康面板:各 provider 熔断器状态,5s 轮询 */
export default function ProviderHealth() {
  const [items, setItems] = useState<ProviderHealthItem[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [news, setNews] = useState<NewsProviderHealthItem[] | null>(null);
  const [newsErr, setNewsErr] = useState<string | null>(null);
  const [changing, setChanging] = useState<string | null>(null);
  const [observations, setObservations] = useState<Record<string, NewsIngestObservation[]>>({});
  const [observationLoading, setObservationLoading] = useState<string | null>(null);

  const loadNews = useCallback(() => {
    getNewsProviderHealth()
      .then((rows) => {
        setNews(rows);
        setNewsErr(null);
      })
      .catch((error) => setNewsErr(errMsg(error)));
  }, []);

  useEffect(() => {
    let alive = true;
    const load = () =>
      getProviderHealth()
        .then((h) => {
          if (!alive) return;
          setItems(h);
          setErr(null);
        })
        .catch((e) => alive && setErr(errMsg(e)));
    load();
    const t = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, []);

  useEffect(() => {
    loadNews();
    const timer = setInterval(loadNews, 15_000);
    return () => clearInterval(timer);
  }, [loadNews]);

  const toggleNews = async (item: NewsProviderHealthItem) => {
    setChanging(item.provider_id);
    try {
      await setNewsProviderEnabled(item.provider_id, !item.enabled);
      loadNews();
    } catch (error) {
      setNewsErr(errMsg(error));
    } finally {
      setChanging(null);
    }
  };

  const loadObservations = async (providerId: string) => {
    setObservationLoading(providerId);
    try {
      const rows = await getNewsIngestObservations(providerId, 10);
      setObservations((current) => ({ ...current, [providerId]: rows }));
    } catch (error) {
      setNewsErr(errMsg(error));
    } finally {
      setObservationLoading(null);
    }
  };

  return (
    <div className="space-y-3">
      <div className="card">
        <div className="card-title">
          <Term
            label="行情数据源健康"
            tip="各行情数据源的熔断器状态：连续失败会暂停该来源，冷却结束后自动试探恢复。"
          />
        </div>
        <div className="space-y-1.5 p-4">
          {err ? (
            <div className="muted text-xs">{err}（5 秒后自动重试）</div>
          ) : !items ? (
            <div className="muted text-xs">加载中…</div>
          ) : items.length === 0 ? (
            <div className="muted text-xs">暂无数据源</div>
          ) : (
            items.map((it) => {
              const meta = metaOf(it.state);
              return (
                <div key={it.name} className="flex items-center gap-2 text-xs">
                  <span className={"h-2 w-2 shrink-0 rounded-full " + meta.dot} />
                  <span className="num">{it.name}</span>
                  <span className={meta.text}>{meta.label}</span>
                  {!it.available && <span className="muted">（未配置凭证）</span>}
                  {it.cooldown_remaining_secs != null && it.cooldown_remaining_secs > 0 && (
                    <span className="num muted">冷却 {it.cooldown_remaining_secs} 秒</span>
                  )}
                </div>
              );
            })
          )}
        </div>
      </div>

      <div className="card">
        <div className="card-title flex items-center justify-between gap-2">
          <Term
            label="财经资讯来源"
            tip="每一路资讯拥有独立的访问频率、重试、熔断、增量游标和最后成功副本。公共快讯只作为发现线索，一手披露优先作为证据。"
          />
          <button className="btn !px-2 !py-0.5 text-xs" onClick={loadNews}>刷新状态</button>
        </div>
        <div className="space-y-2 p-4">
          <div className="muted text-[11px]">状态每 15 秒自动更新；查看状态不会访问任何上游。点击某一行可展开全部运行信息。</div>
          {newsErr && <div className="text-xs text-up">{newsErr}</div>}
          {!news ? (
            <div className="muted text-xs">加载中…</div>
          ) : news.length === 0 ? (
            <div className="muted text-xs">尚未配置资讯来源</div>
          ) : (
            news.map((item) => {
              const usable = item.enabled && item.circuit_state === "closed";
              return (
                <details key={item.provider_id} className="rounded-lg border border-slate-200 dark:border-slate-800">
                  <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-2 gap-y-1 p-2.5 text-xs">
                    <span className={`h-2 w-2 shrink-0 rounded-full ${usable ? "bg-down" : "bg-up"}`} />
                    <span className="font-medium">{item.display_name}</span>
                    <span className="rounded bg-slate-100 px-1.5 py-0.5 dark:bg-slate-800">{item.trust_tier_name}</span>
                    <span className={usable ? "text-down" : "text-up"}>
                      {!item.enabled ? "已停用" : item.circuit_state === "open" ? "暂停请求" : item.stale ? "数据可能陈旧" : "可用"}
                    </span>
                    <span className="num muted">{item.last_latency_ms == null ? "尚无耗时" : `${item.last_latency_ms} 毫秒`}</span>
                    <span className="muted ml-auto">展开详情</span>
                  </summary>
                  <div className="space-y-3 border-t border-slate-200 p-3 text-xs dark:border-slate-800">
                    <div className="grid gap-2 sm:grid-cols-2">
                      <div><span className="muted">内部编号：</span><span className="num break-all">{item.provider_id}</span></div>
                      <div><span className="muted">采集方式：</span>{item.modes.map((mode) => MODE_LABELS[mode]).join("、")}</div>
                      <div><span className="muted">最低刷新间隔：</span><span className="num">{item.min_refresh_secs} 秒</span></div>
                      <div><span className="muted">频率上限：</span><span className="num">{item.rate_limit_per_minute} 次/分钟</span></div>
                      <div><span className="muted">最近成功：</span><span className="num">{dateTime(item.last_success_at)}</span></div>
                      <div><span className="muted">请求/失败：</span><span className="num">{item.attempts} / {item.failures}（{pct(item.failure_rate)}）</span></div>
                      <div><span className="muted">增量游标：</span>{item.cursor_present ? "已持久保存" : "尚未建立"}</div>
                      <div><span className="muted">最近错误：</span>{item.last_error_kind ? (ERROR_LABELS[item.last_error_kind] ?? item.last_error_kind) : "无"}</div>
                      <div><span className="muted">持久档案：</span><span className="num">{item.archived_documents} 篇文档 / {item.archived_revisions} 个修订</span></div>
                      <div><span className="muted">陈旧年龄：</span><span className="num">{item.stale_age_secs == null ? "尚无成功记录" : `${item.stale_age_secs} 秒`}</span></div>
                      <div className="sm:col-span-2"><span className="muted">许可策略：</span>{item.license}</div>
                      <div className="sm:col-span-2 break-all"><span className="muted">访问端点：</span><span className="num">{item.endpoint}</span></div>
                    </div>
                    {item.cooldown_remaining_secs != null && item.cooldown_remaining_secs > 0 && (
                      <div className="rounded bg-red-50 px-2 py-1.5 text-up dark:bg-red-950/30">还需冷却 {item.cooldown_remaining_secs} 秒，期间会自动使用其他来源或最后成功副本。</div>
                    )}
                    <div className="rounded border border-slate-200 p-2 dark:border-slate-800">
                      <div className="flex items-center justify-between gap-2">
                        <span className="font-medium">最近抓取记录</span>
                        <button
                          className="btn !px-2 !py-0.5"
                          disabled={observationLoading === item.provider_id}
                          onClick={() => loadObservations(item.provider_id)}
                        >
                          {observationLoading === item.provider_id ? "读取中…" : "查看最近 10 条"}
                        </button>
                      </div>
                      {observations[item.provider_id] && (
                        <div className="mt-2 space-y-1.5">
                          {observations[item.provider_id].length === 0 ? (
                            <div className="muted">尚无抓取记录</div>
                          ) : observations[item.provider_id].map((row) => (
                            <div key={row.observation_id} className="rounded bg-slate-50 p-2 dark:bg-slate-900/70">
                              <div className="flex flex-wrap gap-x-3 gap-y-1">
                                <span className="num">{dateTime(row.fetched_at)}</span>
                                <span className={row.parse_status === "ok" ? "text-down" : "text-up"}>
                                  {row.parse_status === "ok" ? "解析成功" : "抓取/解析异常"}
                                </span>
                                <span className="num muted">HTTP {row.http_status ?? "未知"}</span>
                                <span className="num muted">{row.latency_ms == null ? "耗时未知" : `${row.latency_ms} 毫秒`}</span>
                                {row.revision_id && <span className="num break-all">修订 {row.revision_id}</span>}
                              </div>
                              {row.parse_error && (
                                <div className="mt-1 flex items-start gap-2 rounded bg-red-50 px-2 py-1 text-up dark:bg-red-950/30">
                                  <span className="min-w-0 flex-1 break-all">{row.parse_error}</span>
                                  <button className="btn !px-1.5 !py-0.5" onClick={() => navigator.clipboard.writeText(row.parse_error ?? "")}>复制错误</button>
                                </div>
                              )}
                              {row.raw_evidence_present && (
                                <div className="muted mt-1">已保留受限原始证据，校验值：<span className="num break-all">{row.raw_evidence_hash}</span></div>
                              )}
                            </div>
                          ))}
                        </div>
                      )}
                    </div>
                    <div className="flex items-center justify-between gap-3">
                      <span className="muted">停用后，该来源及其旧缓存都不会参与 Agent 研究。</span>
                      <button
                        className={item.enabled ? "btn-danger" : "btn-primary"}
                        disabled={changing === item.provider_id}
                        onClick={() => toggleNews(item)}
                      >
                        {changing === item.provider_id ? "保存中…" : item.enabled ? "停用此来源" : "启用此来源"}
                      </button>
                    </div>
                  </div>
                </details>
              );
            })
          )}
        </div>
      </div>
    </div>
  );
}
