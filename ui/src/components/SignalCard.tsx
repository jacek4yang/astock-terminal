import type { SignalJson } from "../lib/api";
import { fmtNum, pctClass } from "../lib/format";
import { StatCell, Term } from "./ui";
import { useAppStore } from "../store";

const ACTION_STYLE: Record<string, string> = {
  强烈买入: "bg-up text-white",
  买入: "bg-up/80 text-white",
  谨慎买入: "bg-amber-500 text-white",
  观望: "bg-slate-500 text-white",
  卖出: "bg-down/80 text-white",
  强烈卖出: "bg-down text-white",
};

function actionStyle(action: string): string {
  for (const k of Object.keys(ACTION_STYLE)) if (action.includes(k)) return ACTION_STYLE[k];
  return "bg-slate-500 text-white";
}

function riskClass(level: string): string {
  if (level.includes("高")) return "text-up";
  if (level.includes("低")) return "text-down";
  return "text-amber-500";
}

export default function SignalCard({ signal }: { signal: SignalJson }) {
  const mode = useAppStore((s) => s.mode);
  const s = signal;
  const tp = s.trade_plan;

  return (
    <div className="card brand-bar-top">
      <div className="card-title">
        信号卡
        <span className="muted text-xs font-normal">{s.description}</span>
      </div>
      <div className="p-3">
        {/* 综合操作大色块 + 概要 */}
        <div className="flex flex-wrap items-stretch gap-3">
          <div
            className={
              "flex min-w-[120px] flex-col items-center justify-center rounded px-5 py-3 " +
              actionStyle(s.action)
            }
          >
            <div className="text-lg font-bold">{s.action}</div>
            <div className="mt-0.5 text-xs opacity-90">信号强度 {s.signal_strength}</div>
          </div>
          {/* 1px 分隔统计带:micro-label + 大数字 */}
          <div className="stat-band min-w-0 flex-1 grid-cols-2 content-start sm:grid-cols-4">
            <StatCell
              label={<Term label="综合评分" tip="五大模块(趋势/形态/量价/突破/CANSLIM)加权得分,满分100" />}
              value={s.score}
            />
            <StatCell
              label={<Term label="置信度" tip="信号可靠程度(0-100),越高代表各模块结论越一致" />}
              value={s.confidence}
            />
            <StatCell
              label="风险等级"
              value={<span className={riskClass(s.risk_level)}>{s.risk_level}</span>}
            />
            <StatCell
              label={<Term label="风报比" tip="预期盈利空间 ÷ 止损亏损空间,大于2通常才值得入场" />}
              value={s.risk_reward != null ? fmtNum(s.risk_reward, 1) : "--"}
            />
          </div>
        </div>

        {/* 大白话总结 */}
        <div className="mt-3 rounded bg-slate-100 px-3 py-2 text-sm leading-relaxed dark:bg-slate-800/60">
          {s.plain_summary}
        </div>

        {mode === "simple" ? (
          /* 小白模式:只看总结 + 操作 + 风险 */
          <div className="mt-3 space-y-1.5">
            {s.risk_warnings.map((w, i) => (
              <div key={i} className="text-sm text-amber-600 dark:text-amber-400">
                风险提示:{w}
              </div>
            ))}
            <div className="muted text-xs">切换到专业模式可查看模块评分、交易计划与关键价位</div>
          </div>
        ) : (
          <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-3">
            {/* 五模块评分 */}
            <div>
              <div className="micro-label mb-1.5">五模块评分</div>
              <div className="space-y-1.5">
                {Object.entries(s.module_scores).map(([name, score]) => (
                  <div key={name} className="flex items-center gap-2 text-xs">
                    <span className="w-14 shrink-0">{name}</span>
                    <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
                      <div
                        className={
                          "h-full rounded-full " +
                          (score >= 70 ? "bg-up" : score >= 45 ? "bg-amber-500" : "bg-down")
                        }
                        style={{ width: `${Math.min(100, score)}%` }}
                      />
                    </div>
                    <span className="num w-8 text-right">{score}</span>
                  </div>
                ))}
              </div>
              {/* 买卖信号 */}
              <div className="micro-label mb-1 mt-3">买卖信号</div>
              <div className="max-h-40 space-y-1 overflow-auto text-xs">
                {s.buy_signals.map((t, i) => (
                  <div key={"b" + i} className="text-up">
                    买 · {t}
                  </div>
                ))}
                {s.sell_signals.map((t, i) => (
                  <div key={"s" + i} className="text-down">
                    卖 · {t}
                  </div>
                ))}
                {s.buy_signals.length === 0 && s.sell_signals.length === 0 && (
                  <div className="muted">暂无明确信号</div>
                )}
              </div>
            </div>

            {/* 交易计划 */}
            <div>
              <div className="micro-label mb-1.5">
                <Term label="交易计划" tip="系统给出的参考入场/止损/目标价位与仓位建议,非投资建议" />
              </div>
              <table className="w-full text-xs">
                <tbody className="[&_td]:py-1">
                  <tr>
                    <td className="muted w-16">方向</td>
                    <td className={pctClass(tp.action.includes("买") ? 1 : tp.action.includes("卖") || tp.action.includes("空") ? -1 : 0)}>
                      {tp.action}
                    </td>
                  </tr>
                  <tr>
                    <td className="muted">入场价</td>
                    <td className="num">{fmtNum(tp.entry_price)}</td>
                  </tr>
                  <tr>
                    <td className="muted">
                      <Term label="止损价" tip="跌破该价位说明判断错误,应离场控制亏损" />
                    </td>
                    <td className="num text-down">{fmtNum(tp.stop_loss)}</td>
                  </tr>
                  <tr>
                    <td className="muted">目标价</td>
                    <td className="num text-up">{fmtNum(tp.target_price)}</td>
                  </tr>
                  <tr>
                    <td className="muted">盈亏比</td>
                    <td className="num">{fmtNum(tp.risk_reward_ratio, 1)}</td>
                  </tr>
                  <tr>
                    <td className="muted">持有周期</td>
                    <td>{tp.holding_period}</td>
                  </tr>
                </tbody>
              </table>
              <div className="mt-2 rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-950/40 dark:text-amber-300">
                仓位建议:{tp.position_size || s.position_advice}
              </div>
              {tp.notes && <div className="muted mt-2 text-xs leading-relaxed">{tp.notes}</div>}
            </div>

            {/* 关键价位 + 风险 */}
            <div>
              <div className="micro-label mb-1.5">关键价位</div>
              <div className="max-h-32 space-y-1 overflow-auto text-xs">
                {Object.entries(s.key_levels).map(([k, v]) => (
                  <div key={k} className="flex justify-between">
                    <span className="muted">{k}</span>
                    <span className="num">{v.toFixed(2)}</span>
                  </div>
                ))}
                {Object.keys(s.key_levels).length === 0 && <div className="muted">无</div>}
              </div>
              {(s.risk_warnings.length > 0 || s.risk_notes.length > 0) && (
                <>
                  <div className="micro-label mb-1 mt-3">风险提示</div>
                  <div className="space-y-1 text-xs">
                    {s.risk_warnings.map((w, i) => (
                      <div key={i} className="text-amber-600 dark:text-amber-400">
                        · {w}
                      </div>
                    ))}
                    {s.risk_notes.map((w, i) => (
                      <div key={i} className="muted">
                        · {w}
                      </div>
                    ))}
                  </div>
                </>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
