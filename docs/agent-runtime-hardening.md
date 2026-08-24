# Agent Runtime 加固

本文描述 `astock-terminal` Agent 在模型流、工具执行、缓存并发和进程中断方面的运行时约束。目标不是承诺上游永不失败，而是保证失败有边界、有状态、可诊断且可恢复，不再出现无限等待或无终态退出。

## 1. 模型流提交边界

MiniMax SSE 被划分为两个阶段：

- **pre-commit**：尚未向上层交付用户可见文本、工具调用或 `finish_reason`。仅有 `reasoning_content` / `reasoning_details` 的 chunk 暂存在内存中。
- **committed**：第一段可见文本、第一段工具调用或结束原因已经出现。此前缓冲的 reasoning chunk 按原顺序交付，之后不允许在 provider 层重放整次请求。

默认策略：

| 参数 | 默认值 | 作用 |
|---|---:|---|
| 首 chunk 等待 | 90 秒 | 永久无首包时终止本次连接 |
| chunk 空闲 | 120 秒 | 流建立后长期无任何数据时终止连接 |
| pre-commit 重建 | 2 次 | reasoning 阶段断流时安全重建请求 |
| reasoning 缓冲上限 | 2 MiB | 防止私有推理缓冲无限增长 |

只有 pre-commit 阶段允许 provider 重建。committed 后断流直接返回瞬时错误，由上层从最近完整持久化轮次恢复，避免重复文本、重复调用工具或破坏 `tool_call_id → tool result` 一一对应关系。

正常完成必须观察到 `finish_reason`。TCP/SSE 在结束标记前关闭不会被误判成完整回答。

## 2. 持久化 Runtime Supervisor

`AgentEngine` 外层监督原有持久化执行内核：

1. 正常事件原样转发。
2. 收到可重试 `Failed`，或事件流在没有 `Completed` / `Suspended` / `Failed` 的情况下结束时，先发送 `TextReset`，清除 UI 中尚未持久化的截断草稿。
3. 将任务状态从瞬时 `failed` 修复为 `running`，清除 `last_error`，从 SQLite 会话和 `agent_tasks.state_json` 恢复。
4. 采用 1、2、4 秒退避，单次运行最多自动恢复 3 次。
5. 达到上限后写入 `status=suspended` 与最后错误，等待用户手动继续，而不是保持伪运行状态。

### 自动恢复分类

自动恢复仅用于保守识别的瞬时错误：

- 网络断开、连接重置、EOF、SSE/stream 中断；
- 429 / rate limit；
- timeout、service unavailable、502/503/504；
- Worker 没有发送任何终态就退出。

以下错误不会循环重试：

- 鉴权、无效密钥；
- API 业务错误和响应解析错误；
- 工具调用历史协议损坏；
- SQLite/存储错误；
- 超过模型最大轮数；
- 证据校验失败；
- 用户取消或任务状态不允许恢复。

MiniMax Token Plan 耗尽仍沿用原有 `QuotaExhausted` 挂起/重置窗口恢复路径，不进入通用瞬时错误重试。

## 3. 工具 single-flight 与缓存规范化

缓存键仍由稳定的 `(tool, canonical_args)` 构成。规范化只处理已有工具契约中语义等价的字段：

- 删除对象中的 `null` 可选字段；
- 证券代码字段裁剪空白、去内部空白并转大写；
- `daily/d`、`weekly/w`、`min60` 等周期别名归一；
- `raw` 复权别名归一为 `none`；
- 数组顺序保持不变，避免改变候选、比较或来源优先级语义；
- 公式、URL、搜索词、来源文本及其他不透明字符串保持原样，不为了命中缓存而改写用户输入。

同一进程内，相同缓存键共享一个 Tokio `OnceCell`：

- leader 执行实际缓存查询或上游工作；
- 同一 flight 的等待者获得同一个不可变成功结果或同一个失败结果的克隆，不再并发击穿上游；
- 成功结果仍由原有 read-through 层写入 SQLite；该 flight 清理后的后续调用正常从 SQLite 命中；
- 失败只在当前并发 flight 内共享，flight 清理后允许新的有界尝试，不形成长期错误缓存。

模型工具 schema 和注册顺序不变，因此不会因并发控制而破坏提示词前缀稳定性。

## 4. 工具防死锁预算

预算是最终安全边界，不是预计完成时间。工具内部的来源切换、退避、熔断和进度事件仍先正常运行。

| 类别 | 上限 |
|---|---:|
| 实时报价、搜索、自选、缓存详情 | 90 秒 |
| K 线、指标、资金流、市场宽度 | 180 秒 |
| 个股分析、基本面、估值、比较、聚宽模板 | 360 秒 |
| 新闻、黄金、公告、网页和原文核验 | 600 秒 |
| 全市场扫描、图谱、量化、回测和策略迭代 | 1200 秒 |
| 未分类工具 | 300 秒 |

超过预算时仅返回该工具的结构化失败。编排器继续使用同批及历史成功证据，并要求最终回答明确标注缺失项。用户取消仍通过丢弃 Future 立即传播，不等待预算耗尽。

## 5. 运行时不变量

- 一次 assistant 工具调用只允许恰好一个同 ID 的 tool result。
- pre-commit 重建不得把部分消息写入 SQLite。
- committed 流不得在 provider 层整轮重放。
- 任何 TaskStream 必须产生终态，或由监督器转换为恢复/挂起状态。
- 单个工具超时、403 或来源失败不能阻塞其他工具和最终综合。
- single-flight 不改变工具权限、安全域、schema、来源时间和证据编号。
- 自动恢复有明确上限，不允许无限重试。

## 6. 故障测试矩阵

| 场景 | 预期结果 |
|---|---|
| 首包永久静默 | 90 秒后错误；pre-commit 有界重建 |
| reasoning-only 后断流 | 丢弃未提交缓冲，安全重建 |
| 可见文本后断流 | 不在 provider 层重放；Supervisor 清稿并从检查点恢复 |
| 流无 `finish_reason` 关闭 | 视为不完整，不发布报告 |
| 16 路相同成功工具调用 | 实际执行一次，其余共享同一成功结果 |
| 16 路相同失败工具调用 | 实际执行一次，同一 flight 共享失败；后续 flight 可重试 |
| 工具永久不返回 | 到达分类预算后只失败该工具 |
| Worker 无终态退出 | Supervisor 转为恢复；达到上限后持久化挂起 |
| 鉴权/协议/存储错误 | 立即失败，不自动循环 |
| Token Plan 耗尽 | 按额度重置时间挂起和恢复 |

## 7. 验证限制

当前仓库的 GitHub Actions 因账户 billing/spending-limit 限制，在执行任何 step 前即失败且没有 job 日志。因此，本次 PR 不能把 Actions 状态声明为通过。恢复 CI 额度后必须补跑：

```powershell
npm --prefix ui ci
npm --prefix ui test -- --run
npm --prefix ui run build
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo check --locked --workspace --all-targets --all-features
node protocol/codegen.mjs --check
node scripts/capability-parity-check.mjs --release
Push-Location app-moon
moon fmt --check --target-dir D:\astock-build\astock-terminal\moon-target\agent
moon check --target native --target-dir D:\astock-build\astock-terminal\moon-target\agent
moon test --target native --target-dir D:\astock-build\astock-terminal\moon-target\agent
Pop-Location
```

上述列表是普通质量回归，不构成生产发布证明。双求解器 MoonBit/Why3、
TLA+/TLC、故障注入、浏览器、打包桌面、迁移、真实性能、外部服务和
Authenticode 证据必须由不可变提交上的 `scripts/release-gate.ps1` 生成。
