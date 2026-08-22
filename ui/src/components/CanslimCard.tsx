import type { CanslimInfo } from "../lib/api";
import { fmtNum } from "../lib/format";
import { Term } from "./ui";

const ITEMS: { key: keyof Pick<CanslimInfo, "c_score" | "a_score" | "n_score" | "s_score" | "l_score" | "i_score" | "m_score">; label: string; tip: string }[] = [
  { key: "c_score", label: "C 近期动量", tip: "当季业绩/股价动量是否强劲加速" },
  { key: "a_score", label: "A 年度增长", tip: "年度收益是否持续增长" },
  { key: "n_score", label: "N 新高形态", tip: "是否创阶段新高或出现新催化、新形态" },
  { key: "s_score", label: "S 供需筹码", tip: "股本供给与筹码结构,流通盘大小与回购" },
  { key: "l_score", label: "L 龙头地位", tip: "是否为行业领涨龙头而非跟风股" },
  { key: "i_score", label: "I 机构认同", tip: "机构资金是否认可并持续流入" },
  { key: "m_score", label: "M 市场环境", tip: "大盘趋势方向,顺势而为" },
];

function scoreColor(v: number): string {
  if (v >= 70) return "text-up";
  if (v >= 45) return "text-amber-500";
  return "text-down";
}

export default function CanslimCard({ data }: { data: CanslimInfo }) {
  const ch = data.cup_handle;
  return (
    <div className="card">
      <div className="card-title justify-between">
        <span>
          <Term
            label="CANSLIM"
            tip="欧奈尔成长股选股模型:C当季动量、A年度增长、N新高、S供需、L龙头、I机构、M大盘"
          />
        </span>
        <span className="text-xs font-normal">
          <span className={"num font-semibold " + scoreColor(data.total)}>{data.total}</span>
          <span className="muted"> 分 / 评级 </span>
          <span className={"num font-semibold " + scoreColor(data.total)}>{data.grade}</span>
        </span>
      </div>
      <div className="p-3">
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-4 lg:grid-cols-7">
          {ITEMS.map((it) => {
            const v = data[it.key];
            return (
              <div
                key={it.key}
                className="rounded border border-slate-200 px-2 py-2 text-center dark:border-slate-800"
                title={it.tip}
              >
                <div className="muted cursor-help text-xs underline decoration-dotted underline-offset-2">
                  {it.label}
                </div>
                <div className={"num mt-1 text-lg font-semibold " + scoreColor(v)}>{v}</div>
              </div>
            );
          })}
        </div>
        {ch && (
          <div className="mt-3 rounded bg-slate-100 px-3 py-2 text-xs dark:bg-slate-800/60">
            <Term label="杯柄形态" tip="经典看涨中继形态:圆弧底(杯)+小幅回调整理(柄),放量突破柄部上沿为买点" />
            <span className="muted ml-2">
              杯高 {fmtNum(ch.cup_high)} / 杯低 {fmtNum(ch.cup_low)} · 杯深 {fmtNum(ch.cup_depth, 1)}% ·
              柄区 [{fmtNum(ch.handle_low)} ~ {fmtNum(ch.handle_high)}]
            </span>
            <span className={"ml-2 font-medium " + (ch.breakout ? "text-up" : "muted")}>
              {ch.breakout ? "已突破" : "未突破"}
            </span>
            {ch.buy_point != null && (
              <span className="muted ml-2">
                买点 <span className="num text-up">{fmtNum(ch.buy_point)}</span>
                {ch.target != null && (
                  <>
                    {" "}目标 <span className="num">{fmtNum(ch.target)}</span>
                  </>
                )}
              </span>
            )}
          </div>
        )}
        {data.signals.length > 0 && (
          <div className="mt-2 space-y-1 text-xs">
            {data.signals.map((s, i) => (
              <div key={i} className="muted">
                · {s}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
