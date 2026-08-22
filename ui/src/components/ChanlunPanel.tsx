import type { ChanlunDailyJson } from "../lib/api";
import { Term } from "./ui";

interface Props {
  data: ChanlunDailyJson;
  showOverlay: boolean;
  onToggleOverlay: (v: boolean) => void;
}

/** 缠论面板:概要 + 买卖点列表(K线叠加由 KlineChart 负责) */
export default function ChanlunPanel({ data, showOverlay, onToggleOverlay }: Props) {
  const isBuy = (t: string) => t.includes("buy") || t.includes("买");
  return (
    <div className="card">
      <div className="card-title justify-between">
        <span>
          缠论分析
          <span className="muted ml-2 text-xs font-normal">
            {data.fractal_count} 分型 · {data.stroke_count} 笔 · {data.zhongshu_count}{" "}
            <Term label="中枢" tip="连续三笔价格重叠区间,代表多空反复争夺的平衡区域" />
          </span>
        </span>
        <label className="flex cursor-pointer items-center gap-1.5 text-xs font-normal">
          <input
            type="checkbox"
            checked={showOverlay}
            onChange={(e) => onToggleOverlay(e.target.checked)}
          />
          在K线图上叠加
        </label>
      </div>
      <div className="space-y-3 p-3">
        <div className="rounded bg-slate-100 px-3 py-2 text-xs leading-relaxed dark:bg-slate-800/60">
          {data.description}
        </div>
        <div>
          <div className="muted mb-1.5 text-xs font-medium">
            买卖点(
            <Term label="一类" tip="趋势背驰产生的转折点,最早但最难把握" />/
            <Term label="二类" tip="一类买卖点之后的回抽确认点,成功率更高" />/
            <Term label="三类" tip="突破中枢后的回踩/反抽确认点" />)
          </div>
          {data.signals.length === 0 ? (
            <div className="muted text-xs">近期无买卖点信号</div>
          ) : (
            <table className="w-full text-xs">
              <thead>
                <tr className="muted border-b border-slate-200 text-left dark:border-slate-800">
                  <th className="py-1 font-normal">日期</th>
                  <th className="py-1 font-normal">类型</th>
                  <th className="py-1 font-normal">价位</th>
                  <th className="py-1 font-normal">
                    <Term label="置信度" tip="信号可靠程度(0-100),依据背驰力度与中枢位置综合评估" />
                  </th>
                  <th className="py-1 font-normal">说明</th>
                </tr>
              </thead>
              <tbody>
                {[...data.signals].reverse().map((sig, i) => (
                  <tr key={i} className="border-b border-slate-100 dark:border-slate-800/50">
                    <td className="num muted py-1.5">{sig.date}</td>
                    <td
                      className={
                        "py-1.5 font-medium " + (isBuy(sig.type + sig.type_name) ? "text-up" : "text-down")
                      }
                    >
                      {sig.type_name}
                    </td>
                    <td className="num py-1.5">{sig.price.toFixed(2)}</td>
                    <td className="num py-1.5">{sig.confidence}</td>
                    <td className="muted max-w-[280px] truncate py-1.5" title={sig.description}>
                      {sig.description}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
        <div className="muted text-xs">当前状态:{data.current_state}</div>
      </div>
    </div>
  );
}
