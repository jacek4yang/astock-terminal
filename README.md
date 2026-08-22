# 趋势智研 · AStock Terminal

面向个人投资者的 A 股研究与人工决策支持终端。应用以 Rust 确定性行情、技术分析、基本面、图谱、量化和回测引擎为底座，由 MiniMax Agent 负责选择工具、交叉核验和解释证据。

本项目不是自动交易系统：不登录券商、不路由订单、不自动买卖，也不承诺收益。交易计划只提供带成立条件、失效条件和风险预算的人工执行参考。

## 主要能力

- Security Master 统一证券身份，覆盖沪深、科创、创业、北交所及研究范围内的基金品种。
- TDX、腾讯、新浪、东方财富及可选数据源的字段级合并、来源标记、质量校验、缓存、限频和熔断。
- 行情、K 线、五档、技术结构、缠论、资金流、基本面、估值、产业链图谱和市场状态工作台。
- 全市场分页筛选表、后台扫描、可恢复后台回测和持久化可调整桌面布局。
- 独立专业资讯中心，支持多源增量更新、修订追踪、事件折叠、丰富筛选、分页与十万条虚拟浏览。
- MiniMax M3 工具调用、Token Plan 额度查看、自动上下文压缩、中断恢复、证据清单和策略迭代。
- 基于 ATR、结构位、交易规则与时段配置生成的 `ManualTradingPlan`。

## 开发环境

- Windows 10/11、Rust 1.88+、Node.js 20+、WebView2
- Tauri v2 所需的 MSVC Build Tools

```powershell
npm --prefix ui ci
npm --prefix ui run build
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
npm --prefix ui test
.\ui\node_modules\.bin\tauri.cmd dev
```

生产包：

```powershell
.\ui\node_modules\.bin\tauri.cmd build
```

## 配置与凭证

MiniMax Key 保存在 Windows 凭据管理器，不写入数据库或日志。Tushare、问财、聚宽及 SOCKS5 为可选配置；未配置时相应数据源会标记为不可用，核心页面继续降级运行。

架构与方法详见 [架构](docs/architecture.md)、[数据源](docs/data-sources.md)、[专业资讯中心](docs/news-center.md)、[数据契约](docs/data-contracts.md)、[Agent 协议](docs/agent-protocol.md) 和 [量化方法](docs/quant-methodology.md)。

## 数据与风险

公开金融上游可能超时、限流、变更结构或返回错误数据。终端会展示来源、时间、缺失原因和降级状态，但这些机制不能消除全部数据风险。历史回测不代表未来表现。
