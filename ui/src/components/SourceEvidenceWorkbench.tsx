import { useCallback, useEffect, useState } from "react";
import {
  compareSourceEvidence,
  errMsg,
  fetchSourceDocument,
  getSourceDocument,
  getSourceDocuments,
  type EvidenceConflict,
  type FactEvidence,
  type SourceDocumentDetail,
  type SourceDocumentSummary,
  type SourceSegment,
} from "../lib/api";

function dateTime(seconds: number | null): string {
  if (seconds == null) return "时间未知";
  return new Date(seconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

function pct(value: number): string {
  return `${(value * 100).toFixed(0)}%`;
}

function factLocation(fact: FactEvidence): string {
  const page = fact.page_number == null ? "网页" : `第 ${fact.page_number} 页`;
  return `${page} · 第 ${fact.paragraph_index + 1} 段 · 原文位置 ${fact.span_start}–${fact.span_end}`;
}

function EvidenceText({ fact, segment }: { fact: FactEvidence; segment?: SourceSegment }) {
  if (!segment) return <span className="muted">原段落未找到</span>;
  const needle = `${fact.raw_value}${fact.original_unit ?? ""}`;
  let start = segment.text.indexOf(needle);
  let length = needle.length;
  if (start < 0) {
    start = segment.text.indexOf(fact.raw_value);
    length = fact.raw_value.length;
  }
  if (start < 0) return <span>{segment.text}</span>;
  return (
    <span>
      {segment.text.slice(0, start)}
      <mark className="rounded bg-amber-200 px-0.5 dark:bg-amber-800/70">
        {segment.text.slice(start, start + length)}
      </mark>
      {segment.text.slice(start + length)}
    </span>
  );
}

export default function SourceEvidenceWorkbench() {
  const [documents, setDocuments] = useState<SourceDocumentSummary[]>([]);
  const [details, setDetails] = useState<Record<string, SourceDocumentDetail>>({});
  const [selected, setSelected] = useState<string[]>([]);
  const [conflicts, setConflicts] = useState<EvidenceConflict[] | null>(null);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setDocuments(await getSourceDocuments(100));
      setError(null);
    } catch (loadError) {
      setError(errMsg(loadError));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const fetchUrl = async () => {
    const requested = url.trim();
    if (!requested) return;
    setBusy("fetch");
    try {
      const detail = await fetchSourceDocument(requested);
      if (detail.version) {
        setDetails((current) => ({ ...current, [detail.version!.source_version_id]: detail }));
      }
      setUrl("");
      setError(null);
      await load();
    } catch (fetchError) {
      setError(errMsg(fetchError));
    } finally {
      setBusy(null);
    }
  };

  const loadDetail = async (versionId: string) => {
    if (details[versionId]) return;
    setBusy(`detail:${versionId}`);
    try {
      const detail = await getSourceDocument(versionId);
      setDetails((current) => ({ ...current, [versionId]: detail }));
      setError(null);
    } catch (detailError) {
      setError(errMsg(detailError));
    } finally {
      setBusy(null);
    }
  };

  const toggleSelected = (versionId: string, checked: boolean) => {
    setSelected((current) => checked
      ? [...new Set([...current, versionId])]
      : current.filter((item) => item !== versionId));
    setConflicts(null);
  };

  const compare = async () => {
    if (selected.length < 2) return;
    setBusy("compare");
    try {
      setConflicts(await compareSourceEvidence(selected));
      setError(null);
    } catch (compareError) {
      setError(errMsg(compareError));
    } finally {
      setBusy(null);
    }
  };

  const copy = async (key: string, value: unknown) => {
    try {
      await navigator.clipboard.writeText(typeof value === "string" ? value : JSON.stringify(value, null, 2));
      setCopied(key);
      window.setTimeout(() => setCopied((current) => current === key ? null : current), 1500);
    } catch (copyError) {
      setError(`复制失败：${String(copyError)}`);
    }
  };

  return (
    <div className="card">
      <div className="card-title flex items-center justify-between gap-2">
        <span>原始来源核验与字段级证据</span>
        <button className="btn !px-2 !py-0.5 text-xs" onClick={load}>刷新</button>
      </div>
      <div className="space-y-3 p-4 text-xs">
        <div className="muted leading-relaxed">
          搜索摘要只用于发现网址。重大结论必须读取原网页、接口数据或 PDF；无法访问时明确标为“原文未核验”。来源评分仅帮助排序，不能代替原文证据。
        </div>
        <div className="flex gap-2">
          <input
            className="input min-w-0 flex-1"
            value={url}
            onChange={(event) => setUrl(event.target.value)}
            onKeyDown={(event) => event.key === "Enter" && fetchUrl()}
            placeholder="输入公告、监管页面、新闻原文、JSON 或 PDF 网址"
          />
          <button className="btn-primary" disabled={busy === "fetch" || !url.trim()} onClick={fetchUrl}>
            {busy === "fetch" ? "正在安全读取…" : "读取并核验"}
          </button>
        </div>
        {error && (
          <div className="flex items-start gap-2 rounded bg-red-50 p-2 text-up dark:bg-red-950/30">
            <span className="min-w-0 flex-1 break-all">{error}</span>
            <button className="btn !px-2 !py-0.5" onClick={() => copy("error", error)}>
              {copied === "error" ? "已复制" : "复制诊断信息"}
            </button>
          </div>
        )}

        {documents.some((document) => document.current_version_id) && (
          <div className="flex flex-wrap items-center gap-2 rounded bg-slate-50 p-2 dark:bg-slate-900/60">
            <span>已选择 {selected.length} 个版本</span>
            <button className="btn" disabled={selected.length < 2 || busy === "compare"} onClick={compare}>
              {busy === "compare" ? "正在逐字段核对…" : "比较所选来源"}
            </button>
            <span className="muted">冲突会并列展示，不会自动挑选更有利的数字。</span>
          </div>
        )}

        {conflicts && (
          <div className="space-y-2 rounded border border-amber-300 bg-amber-50 p-3 dark:border-amber-800 dark:bg-amber-950/30">
            <div className="font-medium">字段比较结果：{conflicts.length} 项冲突</div>
            {conflicts.length === 0 && <div className="muted">已抽取字段中没有发现不同取值。</div>}
            {conflicts.map((conflict) => (
              <details key={conflict.field_name} className="rounded border border-amber-200 p-2 dark:border-amber-900">
                <summary className="cursor-pointer font-medium">{conflict.field_name} · {conflict.values.length} 个不同来源值</summary>
                <div className="mt-2 space-y-1">
                  {conflict.values.map((fact) => (
                    <div key={fact.fact_id} className="rounded bg-white/60 p-2 dark:bg-slate-950/30">
                      <span className="font-medium">原值 {fact.raw_value}{fact.original_unit}</span>
                      <span className="num muted ml-2 break-all">{fact.source_version_id}</span>
                      <div className="muted">{factLocation(fact)}</div>
                    </div>
                  ))}
                  <div className="muted">{conflict.note}</div>
                </div>
              </details>
            ))}
          </div>
        )}

        {documents.length === 0 ? <div className="muted">尚无来源记录。Agent 核验外部事实或手动读取网址后会显示在这里。</div> : (
          <div className="space-y-2">
            {documents.map((document) => {
              const versionId = document.current_version_id;
              const detail = versionId ? details[versionId] : undefined;
              return (
                <details
                  key={document.source_document_id}
                  className="rounded-lg border border-slate-200 dark:border-slate-800"
                  onToggle={(event) => event.currentTarget.open && versionId && loadDetail(versionId)}
                >
                  <summary className="cursor-pointer list-none p-2.5">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className={document.access_status === "verified" ? "text-emerald-600" : "text-up"}>
                        {document.access_status === "verified" ? "已读取原文" : "原文未核验"}
                      </span>
                      <span className="font-medium">{document.authority_name}</span>
                      {document.is_primary_source && <span className="rounded bg-blue-50 px-1.5 py-0.5 text-blue-700 dark:bg-blue-950/40 dark:text-blue-300">一级来源</span>}
                      <span className="muted ml-auto">{dateTime(document.last_fetched_at)}</span>
                    </div>
                    <div className="mt-1 truncate text-[11px] text-blue-600 dark:text-blue-300">{document.canonical_url}</div>
                  </summary>
                  <div className="space-y-3 border-t border-slate-200 p-3 dark:border-slate-800">
                    {document.access_status !== "verified" && (
                      <div className="space-y-2 rounded bg-red-50 p-2 text-up dark:bg-red-950/30">
                        <div>{document.failure_message ?? "页面未返回可核验正文"}</div>
                        <div className="num">失败类型：{document.failure_kind ?? "未知"}</div>
                        {versionId && <div className="muted">下方仍可查看此前最后一次成功读取的历史版本，但它不能证明当前页面状态。</div>}
                        <button className="btn" onClick={() => copy(`failure:${document.source_document_id}`, document)}>
                          {copied === `failure:${document.source_document_id}` ? "已复制" : "复制完整诊断记录"}
                        </button>
                      </div>
                    )}
                    {!versionId ? (
                      <div className="muted">没有可供引用的历史原文版本。</div>
                    ) : !detail ? (
                      <div className="muted">{busy === `detail:${versionId}` ? "正在读取原文分段与证据…" : "展开后读取详情"}</div>
                    ) : (
                      <>
                        <div className="grid gap-2 sm:grid-cols-2">
                          <div><span className="muted">标题：</span>{detail.version?.title ?? "未识别"}</div>
                          <div><span className="muted">内容格式：</span><span className="num">{detail.version?.media_type}</span></div>
                          <div><span className="muted">读取时间：</span><span className="num">{dateTime(detail.version?.fetched_at ?? null)}</span></div>
                          <div><span className="muted">发布时间：</span><span className="num">{dateTime(detail.version?.published_at ?? null)}</span></div>
                        </div>
                        <div className="rounded bg-slate-50 p-2 dark:bg-slate-900/60">{detail.verification_note}</div>
                        {detail.version?.prompt_injection_detected && (
                          <div className="rounded bg-red-50 p-2 text-up dark:bg-red-950/30">原文含疑似提示词注入内容，已按不可信外部数据隔离。</div>
                        )}
                        {detail.version && (
                          <details className="rounded border border-slate-200 p-2 dark:border-slate-800">
                            <summary className="cursor-pointer font-medium">来源排序评分与版本信息</summary>
                            <div className="mt-2 grid gap-2 sm:grid-cols-3">
                              <div>可靠性 <span className="num">{pct(detail.version.scores.reliability)}</span></div>
                              <div>独立性 <span className="num">{pct(detail.version.scores.independence)}</span></div>
                              <div>新鲜度 <span className="num">{pct(detail.version.scores.freshness)}</span></div>
                            </div>
                            <div className="muted mt-1">{detail.version.scores.note}</div>
                            <div className="num mt-2 break-all">版本：{detail.version.source_version_id}</div>
                            <div className="num break-all">内容指纹：{detail.version.content_hash}</div>
                            {detail.version.supersedes_version_id && <div className="num break-all">替代旧版本：{detail.version.supersedes_version_id}</div>}
                          </details>
                        )}
                        <div className="flex flex-wrap items-center gap-2">
                          <label className="flex cursor-pointer items-center gap-1.5">
                            <input type="checkbox" checked={selected.includes(versionId)} onChange={(event) => toggleSelected(versionId, event.target.checked)} />
                            加入字段比较
                          </label>
                          <button className="btn" onClick={() => copy(`detail:${versionId}`, detail)}>
                            {copied === `detail:${versionId}` ? "已复制" : "复制完整证据包"}
                          </button>
                          <span className="muted">抽取 {detail.facts.length} 项字段 · 保留 {detail.segments.length} 个原文段落</span>
                        </div>
                        <div className="space-y-2">
                          {detail.facts.length === 0 && <div className="muted">未自动识别金额、比例、产能或日期字段；仍可展开原文段落人工核对。</div>}
                          {detail.facts.map((fact) => {
                            const segment = detail.segments.find((item) => item.segment_id === fact.segment_id);
                            return (
                              <details key={fact.fact_id} className="rounded border border-slate-200 p-2 dark:border-slate-800">
                                <summary className="cursor-pointer">
                                  <span className="font-medium">{fact.field_name}：{fact.raw_value}{fact.original_unit}</span>
                                  <span className="muted ml-2">{factLocation(fact)}</span>
                                </summary>
                                <div className="mt-2 space-y-2">
                                  <div className="leading-relaxed"><EvidenceText fact={fact} segment={segment} /></div>
                                  <div className="num break-all">事实编号：{fact.fact_id}</div>
                                  <div className="num break-all">段落编号：{fact.segment_id}</div>
                                  {fact.normalized_value != null && <div className="muted">标准化值：<span className="num">{fact.normalized_value} {fact.normalized_unit}</span>（原单位始终保留）</div>}
                                  <button className="btn" onClick={() => copy(`fact:${fact.fact_id}`, { fact, segment })}>
                                    {copied === `fact:${fact.fact_id}` ? "已复制" : "复制该条证据"}
                                  </button>
                                </div>
                              </details>
                            );
                          })}
                        </div>
                        <details className="rounded border border-slate-200 p-2 dark:border-slate-800">
                          <summary className="cursor-pointer font-medium">查看全部原文分段（{detail.segments.length}）</summary>
                          <div className="mt-2 max-h-80 space-y-2 overflow-y-auto">
                            {detail.segments.map((segment) => (
                              <div key={segment.segment_id} className="rounded bg-slate-50 p-2 dark:bg-slate-900/60">
                                <div className="muted mb-1">{segment.page_number == null ? "网页" : `第 ${segment.page_number} 页`} · 第 {segment.paragraph_index + 1} 段 · {segment.span_start}–{segment.span_end}</div>
                                <div>{segment.text}</div>
                              </div>
                            ))}
                          </div>
                        </details>
                      </>
                    )}
                  </div>
                </details>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
