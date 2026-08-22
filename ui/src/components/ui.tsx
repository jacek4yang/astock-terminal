import { useEffect, useRef, useState, type ReactNode } from "react";
import logo from "../assets/logo.svg";

/** 让加载态至少展示 minMs,避免一闪而过 */
export function useMinLoading(loading: boolean, minMs = 450): boolean {
  const [visible, setVisible] = useState(loading);
  const shownAt = useRef<number | null>(null);

  useEffect(() => {
    if (loading) {
      shownAt.current = Date.now();
      setVisible(true);
      return;
    }
    const shown = shownAt.current;
    if (shown == null) {
      setVisible(false);
      return;
    }
    const remain = minMs - (Date.now() - shown);
    if (remain <= 0) {
      setVisible(false);
      return;
    }
    const t = setTimeout(() => setVisible(false), remain);
    return () => clearTimeout(t);
  }, [loading, minMs]);

  return visible;
}

/** 滑动段不确定进度条(2px 轨道 + 40% 主色段) */
export function LoadBar({ className = "" }: { className?: string }) {
  return (
    <div className={"load-track " + className}>
      <div className="load-segment" />
    </div>
  );
}

export function Loading({ text = "加载中…" }: { text?: string }) {
  return (
    <div className="anim-fade-in flex flex-col items-center justify-center gap-3 px-8 py-6">
      <LoadBar className="w-40" />
      <span className="muted text-xs">{text}</span>
    </div>
  );
}

/** 品牌加载屏:logo 呼吸 + 滑动段条(应用启动 / 大模块加载) */
export function BrandLoader({ text = "正在加载…" }: { text?: string }) {
  return (
    <div className="anim-fade-in flex h-full w-full flex-col items-center justify-center gap-5 bg-white dark:bg-slate-950">
      <img src={logo} alt="趋势智研" className="anim-logo-pulse h-14 w-14 rounded-2xl" />
      <div className="text-sm font-semibold tracking-wide text-slate-700 dark:text-slate-200">
        趋势智研
      </div>
      <LoadBar className="w-44" />
      <span className="muted text-xs">{text}</span>
    </div>
  );
}

export function ErrorBox({ message, onRetry }: { message: string; onRetry?: () => void }) {
  return (
    <div className="mx-3 my-3 rounded border border-red-300 bg-red-50 px-3 py-2 text-sm text-red-700 dark:border-red-900 dark:bg-red-950/40 dark:text-red-300">
      <div className="flex items-center justify-between gap-3">
        <span>{message}</span>
        {onRetry && (
          <button className="btn shrink-0" onClick={onRetry}>
            重试
          </button>
        )}
      </div>
    </div>
  );
}

/** 专业术语:悬停显示白话解释 */
export function Term({ label, tip }: { label: string; tip: string }) {
  return (
    <span className="term" title={tip}>
      {label}
    </span>
  );
}

/** 顶部信息栏的单项指标 */
export function Stat({
  label,
  value,
  tip,
  className = "",
}: {
  label: ReactNode;
  value: ReactNode;
  tip?: string;
  className?: string;
}) {
  return (
    <div className={"min-w-[64px] " + className} title={tip}>
      <div className="micro-label">{label}</div>
      <div className="num mt-0.5 text-sm">{value}</div>
    </div>
  );
}

/** 1px 分隔统计带单元格(micro-label + 大数字) */
export function StatCell({
  label,
  value,
  tip,
  className = "",
}: {
  label: ReactNode;
  value: ReactNode;
  tip?: string;
  className?: string;
}) {
  return (
    <div className={"stat-cell " + className} title={tip}>
      <div className="micro-label">{label}</div>
      <div className="num mt-1 text-lg font-semibold leading-tight">{value}</div>
    </div>
  );
}

export function EmptyBox({ text }: { text: string }) {
  return <div className="muted py-6 text-center text-sm">{text}</div>;
}
