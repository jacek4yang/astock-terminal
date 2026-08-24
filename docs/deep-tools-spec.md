# 深度分析 Agent / Engine 合同说明（v6 已接线）

目标:让 Agent 从"读报价"升级为"做研究"。所有数字来自引擎,LLM 只做组织与解释。

## MoonBit Agent 使用的 Engine 工具

1. `get_fundamentals(symbol)` → 概况/最新指标(ROE/毛利/FCF/收现比)/成长序列摘要/F-Z-M 评分/异常预警;full 进缓存。
   数据源:astock-fundamental `FundamentalClient::bundle` + metrics/scores/anomaly。
2. `run_valuation(symbol, growth?, wacc?)` → 当前倍数+历史分位(方法标注)+DCF 三情景区间+敏感性摘要。
3. `get_industry_chain(symbol)` → 该公司在图谱中的位置:上游供应商/下游客户/竞争对手/所属行业(belongs_to),带来源与置信度。
   数据源:astock-graph `GraphStore::{neighbors, subgraph}`。
4. `run_supply_chain_shock(subject, direction, magnitude_pct)` → 事件传导报告:一级受益/一级受损/二级/潜在映射,每条含完整逻辑链、预期滞后、量级估计(标注粗略)、置信度;例:subject="铜", direction="up", magnitude_pct=10。
   数据源:astock-graph `engine::propagate`。
5. `build_relationship_graph(symbols[], window_days=250)` → 快速两两 Pearson + lead-lag 网络图。正式推断使用 `run_quant_research`：可配置完整研究口径，输出 Bootstrap/置换区间与 p 值、默认 FDR、多维稳健性、有效 N、性能预算和不可变快照；严格区分相关、预测领先、Granger 预测因果和结构因果。
   数据源:market-data 取历史 K 线 → astock-quant correlation/leadlag。
6. `run_backtest(symbol, strategy, params?, years?)` → MaCross/Turtle/BuyHold 回测:CAGR/Sharpe/最大回撤/胜率/交易次数+过拟合警告(若跑网格);强调历史不代表未来由 UI 固定提示,不在文本重复。
   数据源:astock-backtest,K线来自缓存/市场层。
7. `get_market_regime()` → 宽度、指数 MA20/60 状态、涨跌停家数(若可得)、成交趋势 → 风险偏好分档(进攻/中性/防守),全部附数据。

## 进程边界
MoonBit Agent 只通过版本化协议请求 Engine 工具。Rust Engine 负责
market、storage、graph、fundamental 等确定性能力；Renderer 不持有
Rust `ToolContext` 或数据库连接。

## Agent playbook
深度研究流程:全面分析 = 行情资金 → 技术结构 → 基本面(get_fundamentals)→ 估值(run_valuation)→ 产业链位置(get_industry_chain)→ 同类对比(compare_stocks)→ 市场状态(get_market_regime)→ 综合:结论/证据/不确定性/失效条件。
事件类问题 = run_supply_chain_shock → 个股验证(get_quote/get_fundamentals)→ 已 price-in 判断。
关系类问题 = build_relationship_graph → 解读稳定性与风险。

## UI
- 供应链:产业链地图页(图谱可视化,ECharts graph 布局,冲击传播高亮)。
- 关系网络:网络图页(边粗细=相关强度,红正绿负)。
- 回测:策略回测页(参数表单+权益曲线+回撤区+交易明细+过拟合警告徽标)。
命令层：相应能力由 `protocol/schema/engine.schema.json` 的粗粒度请求
暴露，并通过 Proton typed bridge 调用。
