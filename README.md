# 趋势智研 · AStock Terminal

面向个人投资者的 A 股研究与人工决策支持终端。应用以 Rust 确定性行情、技术分析、基本面、图谱、量化和回测引擎为底座，由 MiniMax Agent 负责选择工具、交叉核验和解释证据。

本项目不是自动交易系统：不登录券商、不路由订单、不自动买卖，也不承诺收益。交易计划只提供带成立条件、失效条件和风险预算的人工执行参考。

## 主要能力

- Security Master 统一证券身份，覆盖沪深、科创、创业、北交所及研究范围内的基金品种。
- TDX、腾讯、新浪、东方财富及可选数据源的字段级合并、来源标记、质量校验、缓存、限频和熔断。
- 行情、K 线、五档、技术结构、缠论、资金流、基本面、估值、产业链图谱和市场状态工作台。
- 年报、招股书、调研、招投标、合同、专利与产能材料的供应链关系后台抽取、原文证据校验、人工审核、幂等发布和可审计撤回。
- 双时间关系图谱：不可变修订、业务/系统知悉时间快照、证据过期复核、实体合并回放、时间滑块与事件回测防穿越（见 [设计文档](docs/bitemporal-graph.md)）。
- 盈利驱动树：按金融/地产/资源/制造/消费/软件适配经营公式，参数与证据逐行追溯，支持三情景、敏感性、Monte Carlo、现价隐含假设及供应链冲击到收入/毛利/EPS/现金流桥接（见 [设计文档](docs/earnings-driver-tree.md)）。
- 可复现量化研究工作台：统一界面/Agent 配置与数值，提供 Bootstrap/置换推断、FDR、多维稳健性、O(n²) 预算、无硬超时后台任务和不可变研究快照（见 [设计文档](docs/quant-lab.md)）。
- 全市场分页筛选表、后台扫描、可恢复后台回测和持久化可调整桌面布局。
- 独立专业资讯中心，支持多源增量更新、修订追踪、事件折叠、丰富筛选、分页与十万条虚拟浏览。
- 资讯按统一 A 股日历和 15:00 边界归入盘前/盘中/下一交易日，实时 Agent 与事件回测共享 Point-in-Time 口径。
- MiniMax Plus 工具调用、Token Plan 额度查看、动态澄清、自动上下文压缩、中断恢复、证据清单和多轮反方复核。
- Agent Runtime 提供 SSE 空闲看门狗、pre-commit 安全重建、持久化检查点恢复、工具 single-flight、缓存参数规范化和防死锁预算（见 [运行时加固](docs/agent-runtime-hardening.md)）。
- 基于 ATR、结构位、交易规则与时段配置生成的 `ManualTradingPlan`。

## 开发与构建

首版仅支持 Windows x64，需要 MSVC Build Tools、Rust 1.88+、Node.js
20+、MoonBit、Proton 0.2.1 和固定 CEF 147 runtime。构建脚本强制把
Cargo、MoonBit、CEF、Vite、npm 与打包中间产物写入
`D:\astock-build\astock-terminal`；D 盘不可用或剩余空间少于 60 GiB
时不会回退到 C 盘。

```powershell
.\scripts\bootstrap.ps1
.\scripts\dev.ps1
.\scripts\test.ps1
.\scripts\package.ps1
```

生产发布必须从干净、与 `origin/main` 完全一致的提交执行：

```powershell
$env:ASTOCK_SIGNING_CERT_THUMBPRINT = '<CurrentUser\\My certificate thumbprint>'
.\scripts\release-gate.ps1
```

门禁在 D 盘生成 JSON/HTML 报告；能力迁移、数据、Agent、恢复、形式
验证、浏览器、桌面 40 场景、迁移、性能、真实 Provider、凭据轮换、
签名或安装任一项缺失都会失败。GitHub Actions 因计费限制未执行时，
Release 必须明确写明 `GitHub Actions: NOT VERIFIED — billing/spending
restriction; release gates executed locally`。

## 配置与凭证

MiniMax Key 保存在 Windows 凭据管理器，不写入数据库或日志。Tushare、问财、聚宽及 SOCKS5 为可选配置；未配置时相应数据源会标记为不可用，核心页面继续降级运行。

架构与方法详见 [架构](docs/architecture.md)、[数据源](docs/data-sources.md)、[专业资讯中心](docs/news-center.md)、[数据契约](docs/data-contracts.md)、[Agent 协议](docs/agent-protocol.md)、[Agent Runtime](docs/agent-runtime-hardening.md)、[量化方法](docs/quant-methodology.md) 和 [可复现量化实验室](docs/quant-lab.md)。

## 数据与风险

公开金融上游可能超时、限流、变更结构或返回错误数据。终端会展示来源、时间、缺失原因和降级状态，但这些机制不能消除全部数据风险。历史回测不代表未来表现。
