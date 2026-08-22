import { useEffect, useState } from "react";
import { getProviderHealth, errMsg, type ProviderHealthItem } from "../lib/api";
import { Term } from "./ui";

const STATE_META: Record<string, { label: string; dot: string; text: string }> = {
  closed: { label: "可用", dot: "bg-down", text: "text-down" },
  open: { label: "熔断中", dot: "bg-up", text: "text-up" },
  half_open: { label: "试探恢复", dot: "bg-amber-500", text: "text-amber-500" },
};

function metaOf(state: string) {
  return STATE_META[state] ?? { label: state, dot: "bg-slate-400", text: "muted" };
}

/** 数据源健康面板:各 provider 熔断器状态,5s 轮询 */
export default function ProviderHealth() {
  const [items, setItems] = useState<ProviderHealthItem[] | null>(null);
  const [err, setErr] = useState<string | null>(null);

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

  return (
    <div className="card">
      <div className="card-title">
        <Term
          label="数据源健康"
          tip="各行情数据源的熔断器状态:连续失败会熔断(open)并跳过该源,冷却结束后放行一个试探请求(half_open),成功后恢复可用(closed)"
        />
      </div>
      <div className="space-y-1.5 p-4">
        {err ? (
          <div className="muted text-xs">{err}(5s 后自动重试)</div>
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
                {!it.available && <span className="muted">(未配置凭证)</span>}
                {it.cooldown_remaining_secs != null && it.cooldown_remaining_secs > 0 && (
                  <span className="num muted">冷却 {it.cooldown_remaining_secs}s</span>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
