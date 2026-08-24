import type { SignalJson } from "../lib/api";
import { EMPTY_DISPLAY, fmtNum, pctClass } from "../lib/format";
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
  const mp = s.manual_plan;

  return (
    <div className="signal-card card brand-bar-top">
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
              value={s.risk_reward != null ? fmtNum(s.risk_reward, 1) : EMPTY_DISPLAY}
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
            {mp && (
              <div className="grid grid-cols-2 gap-2 rounded border border-blue-200 bg-blue-50 p-2.5 text-xs dark:border-blue-900/60 dark:bg-blue-950/30 sm:grid-cols-4">
                <div><span className="muted">候选入场区</span><div className="num font-medium">{fmtNum(mp.entry_zone_low)}–{fmtNum(mp.entry_zone_high)}</div></div>
                <div><span className="muted">失效止损</span><div className="num font-medium text-down">{fmtNum(mp.stop_loss)}</div></div>
                <div><span className="muted">复核目标</span><div className="num font-medium text-up">{fmtNum(mp.target_price)}</div></div>
                <div><span className="muted">账户风险预算</span><div className="num font-medium">≤ {fmtNum(mp.risk_budget_pct, 2)}%</div></div>
              </div>
            )}
            {s.risk_warnings.map((w, i) => (
              <div key={i} className="text-sm text-amber-600 dark:text-amber-400">
                风险提示:{w}
              </div>
            ))}
            <div className="muted text-xs">切换到专业模式可查看模块评分、交易计划与关键价位</div>
          </div>
        ) : (
          <div className="signal-detail-grid mt-4">
            {/* 五模块评分 */}
            <section className="signal-score-section">
              <div className="micro-label mb-1.5">五模块评分</div>
              <div className="space-y-1.5">
                {Object.entries(s.module_scores).map(([name, score]) => (
                  <div key={name} className="signal-module-row text-xs">
                    <span className="signal-module-name">{name}</span>
                    <div className="signal-module-track h-1.5 overflow-hidden rounded-full bg-slate-200 dark:bg-slate-800">
                      <div
                        className={
                          "h-full rounded-full " +
                          (score >= 70 ? "bg-up" : score >= 45 ? "bg-amber-500" : "bg-down")
                        }
                        style={{ width: `${Math.min(100, score)}%` }}
                      />
                    </div>
                    <span className="num signal-module-score">{score}</span>
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
            </section>

            {/* 交易计划 */}
            <section className="signal-plan-section">
              <div className="micro-label mb-1.5">
                <Term label="交易计划" tip="系统给出的参考入场/止损/目标价位与仓位建议,非投资建议" />
              </div>
              <table className="signal-plan-table w-full text-xs">
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
            </section>

            {/* 关键价位 + 风险 */}
            <section className="signal-levels-section">
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
            </section>

            {mp && (
              <details className="rounded-lg border border-slate-200 bg-slate-50/60 p-3 dark:border-slate-800 dark:bg-slate-900/50 lg:col-span-3" open>
                <summary className="cursor-pointer list-none text-sm font-semibold">
                  人工条件化交易计划
                  <span className="num muted ml-2 text-xs font-normal">{mp.plan_id} · 数据截至 {mp.data_as_of}</span>
                </summary>
                <div className="mt-3 space-y-3">
                  <div className="grid gap-2 text-xs sm:grid-cols-2 lg:grid-cols-5">
                    <StatCell label="候选入场区" value={<span className="num">{fmtNum(mp.entry_zone_low)}–{fmtNum(mp.entry_zone_high)}</span>} />
                    <StatCell label="结构失效位" value={<span className="num text-down">{fmtNum(mp.stop_loss)}</span>} />
                    <StatCell label="复核目标" value={<span className="num text-up">{fmtNum(mp.target_price)}</span>} />
                    <StatCell label="风险收益比" value={<span className="num">{fmtNum(mp.risk_reward_ratio, 1)}R</span>} />
                    <StatCell label="账户风险预算" value={<span className="num">≤ {fmtNum(mp.risk_budget_pct, 2)}%</span>} />
                  </div>
                  <div className="grid gap-2 text-xs lg:grid-cols-2">
                    <div className="rounded border border-emerald-200 bg-emerald-50 px-2.5 py-2 text-emerald-800 dark:border-emerald-900/60 dark:bg-emerald-950/30 dark:text-emerald-300">
                      <div className="micro-label mb-1">成立逻辑 · {mp.market_regime}</div>{mp.thesis}
                    </div>
                    <div className="rounded border border-red-200 bg-red-50 px-2.5 py-2 text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
                      <div className="micro-label mb-1">反方与失效条件</div>{mp.counter_thesis}
                    </div>
                  </div>
                  <div>
                    <div className="micro-label mb-1.5">情景响应</div>
                    <div className="grid gap-2 lg:grid-cols-3">
                      {mp.scenarios.map((scenario) => (
                        <div key={scenario.name} className="rounded border border-slate-200 bg-white p-2 text-xs dark:border-slate-800 dark:bg-slate-900">
                          <div className="font-medium">{scenario.name}</div>
                          <div className="muted mt-1">条件：{scenario.condition}</div>
                          <div className="mt-1 text-blue-600 dark:text-blue-400">响应：{scenario.response}</div>
                          <div className="mt-1 text-down">失效：{scenario.invalidation}</div>
                        </div>
                      ))}
                    </div>
                  </div>
                  <div>
                    <div className="micro-label mb-1.5">盘中检查点</div>
                    <div className="overflow-x-auto">
                      <table className="w-full min-w-[760px] text-xs">
                        <thead><tr><th className="th">阶段</th><th className="th">时间</th><th className="th">观察与确认</th><th className="th">确认后</th><th className="th">失败后</th></tr></thead>
                        <tbody>
                          {mp.checkpoints.map((checkpoint) => (
                            <tr key={checkpoint.phase} className="border-t border-slate-200 align-top dark:border-slate-800">
                              <td className="td font-medium">{checkpoint.phase}</td>
                              <td className="td num whitespace-nowrap">{checkpoint.time_window}</td>
                              <td className="td"><div>{checkpoint.observe.join("；")}</div><div className="muted mt-1">需满足：{checkpoint.required_conditions.join("；")}</div></td>
                              <td className="td text-blue-600 dark:text-blue-400">{checkpoint.action_if_confirmed}</td>
                              <td className="td text-down">{checkpoint.action_if_failed}</td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  </div>
                  <div className="grid gap-2 text-xs lg:grid-cols-2">
                    <div><div className="micro-label mb-1">价位依据</div><div className="muted">止损：{mp.stop_basis}</div><div className="muted mt-1">目标：{mp.target_basis}</div></div>
                    <div><div className="micro-label mb-1">执行约束</div>{mp.constraints.map((item) => <div key={item} className="muted">· {item}</div>)}</div>
                  </div>
                  <div className="rounded border border-amber-200 bg-amber-50 px-2.5 py-2 text-xs text-amber-800 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-300">
                    {mp.position_guidance}。{mp.disclaimer}
                  </div>
                </div>
              </details>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
