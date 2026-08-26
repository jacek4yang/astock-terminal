use crate::runtime::RuntimeTask;

const SYSTEM_PROMPT: &str = r#"你是 AStock 的高级金融分析 Agent。你负责规划、选择已注册工具、核验证据并形成可审计的中文研究结论；确定性数值计算由 Rust Engine 完成。

必须遵守：
1. 不得编造行情、财务、新闻、估值或回测数字。缺少证据时明确写“证据不足/无法验证”。
2. 当前、最新、近期问题必须调用合适的实时或最新披露工具，并在报告中写数据时间。
3. 独立只读工具可在同一轮并行调用。只调用注册工具，不生成 shell 或任意文件/进程操作。
4. 每个关键事实和数值使用 Engine 返回的 evidence_registry 中的标识，格式为【E:evf_xxx】；保留来源冲突和失败源。
5. 区分【事实】【计算】【推断】【假设】【未知】。主动寻找反方证据，写结论失效条件和不确定性。
6. 工具局部失败时使用成功证据继续并明确降级；所有来源失败时不得给出确定性投资结论。
7. 只输出公开的研究计划、工具动作、证据判断和最终结论，不展示私有思维链。
8. 禁止自动交易、订单路由或声称替用户执行买卖；计划只能是需人工操作的研究产物。
9. 复杂计算使用受限金融计算 AST 工具，优先用 bindings 拆分中间变量并只输出必要的 tail/归约结果。不得请求 Python、shell、eval 或远端任意代码执行；聚宽计算必须使用 Engine 注入的受保护序列。
10. 产业链/图谱结论必须说明覆盖范围。当前图谱由种子数据加行业富集构成，并非全市场完整覆盖；传播强度、滞后与置信度是有文档记录的启发式，不是实测营收暴露。必须区分“未采集到关系”与“不存在关系”，不得把缺失覆盖当作无影响，也不得把边权当作营收占比。
11. 回测结果只能作为研究性证据。当前回测未通过严格 Point-in-Time、幸存者偏差与可成交性审计，因此不得作为策略盈利能力的证明，也不得与严格样本外结果混同；引用时必须写明该限制与成本、可成交性假设。

实质性报告通常包含：研究结论、关键驱动、市场状态、基本面/估值、技术或交易结构、催化剂、反方证据、风险、失效条件、数据质量、证据来源与置信度。不要为简单问题机械套用完整模板。"#;

pub fn system_prompt(task: &RuntimeTask) -> String {
    format!(
        "{SYSTEM_PROMPT}\n\n本轮运行约束：depth={}；tool_policy={}；language={}；symbol={}。",
        task.depth,
        task.tool_policy,
        task.language,
        task.symbol.as_deref().unwrap_or("未预先指定")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> String {
        system_prompt(&RuntimeTask::ask("分析紫金矿业"))
    }

    /// The Agent must not present partial graph coverage as comprehensive.
    ///
    /// The graph is seed data plus industry enrichment, and the propagation
    /// engine documents its magnitude, lag and confidence outputs as heuristics.
    /// Without this instruction the model can present an edge weight as a revenue
    /// exposure percentage, which is the specific overclaim the graph roadmap
    /// calls out.
    #[test]
    fn the_prompt_constrains_graph_coverage_claims() {
        let rendered = prompt();
        assert!(
            rendered.contains("并非全市场完整覆盖"),
            "the prompt must state that graph coverage is not the whole market"
        );
        assert!(
            rendered.contains("启发式"),
            "the prompt must label propagation numbers as heuristics"
        );
        assert!(
            rendered.contains("未采集到关系"),
            "the prompt must distinguish an uncollected relation from an absent one"
        );
        assert!(
            rendered.contains("不得把边权当作营收占比"),
            "the prompt must forbid reading an edge weight as revenue exposure"
        );
    }

    /// Backtests stay research-only until the point-in-time audit is complete.
    #[test]
    fn the_prompt_forbids_presenting_backtests_as_profitability_proof() {
        let rendered = prompt();
        assert!(
            rendered.contains("Point-in-Time"),
            "the prompt must name the missing point-in-time guarantee"
        );
        assert!(
            rendered.contains("不得作为策略盈利能力的证明"),
            "the prompt must forbid presenting a backtest as proof of profitability"
        );
    }

    /// The manual-trading boundary and the no-arbitrary-execution rule are
    /// product invariants, not stylistic preferences.
    #[test]
    fn the_prompt_keeps_the_safety_boundaries() {
        let rendered = prompt();
        assert!(
            rendered.contains("禁止自动交易"),
            "the prompt must forbid automatic trading"
        );
        assert!(
            rendered.contains("不展示私有思维链"),
            "the prompt must forbid exposing private reasoning"
        );
        assert!(
            rendered.contains("eval"),
            "the prompt must forbid arbitrary code execution"
        );
    }

    #[test]
    fn run_constraints_are_appended_for_the_current_task() {
        let mut task = RuntimeTask::ask("分析紫金矿业");
        task.symbol = Some("601899".into());
        let rendered = system_prompt(&task);
        assert!(rendered.contains("symbol=601899"));
        assert!(rendered.contains("depth="));
    }
}
