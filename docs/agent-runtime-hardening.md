# Agent Runtime 加固

本文描述 `astock-terminal` Agent 在模型流、工具执行、缓存并发和进程中断方面的运行时约束。目标不是承诺上游永不失败，而是保证失败有边界、有状态、可诊断且可恢复，不再出现无限等待或无终态退出。

## 1. 模型流提交边界

MiniMax Provider 位于 MoonBit Agent Worker。结构化规划与审查使用非流式 JSON，最多进行三次有界尝试；每次结果必须完整解析为声明类型，私有思考文本和不完整 JSON 都不会进入任务历史。

最终纯文本报告使用 SSE，但 Worker 在内存中收到并校验完整响应后才一次性交给 reducer：

- 必须观察到 `[DONE]`；提前 EOF、无效 UTF-8、损坏 JSON 或缺失结束标记都视为未完成；
- 传输总量最多 2 MiB、单行最多 256 KiB、最多 32768 个事件、可见正文最多 120000 字节；
- 每次完整报告尝试最多 180 秒，最多三次；失败尝试的部分文本被整体丢弃，后一次从同一持久化输入重新生成；
- `<think>`、`reasoning_details` 和只包含思考内容的响应不会展示或持久化。

因此 Renderer 不会看到“半段答案”，Provider 也不会把一次失败流和后续恢复流拼接为伪完整结论。

## 2. 持久化 Runtime Supervisor

MoonBit Agent 的纯 reducer 只处理事件并产生 Effect；Rust Engine 保存任务事件、检查点、Effect 意图和结果；Proton Host 监督两个 Worker 并编排最多四轮 `host_effects + continuation`：

1. Host 先持久化 Agent 检查点，再读取 Effect 历史。
2. 只接受 `target=engine`，且 kind 必须属于 `research.agent_prepare_context`、`research.agent_security_context`、`research.agent_report_verify` 三项闭集。
3. Host 持久化 Effect 意图后才调用 Engine，Engine 结果持久化成功后才传回 Agent。
4. 已成功的幂等键直接复用；崩溃留下的 pending 记录只允许上述三个可重放研究聚合以 `:retry:N` 重新执行。
5. Agent/Engine 连续丢失三次 2 秒心跳后由 Job Object 监督器重启并重新握手；Provider 暂停保留检查点，不发布未完成报告。

### 自动恢复分类

鉴权失败、无效密钥、额度暂停、协议损坏、SQLite 错误、证据校验失败和用户取消不会被无限重试。MiniMax 不可用时任务进入 `Suspended` 并保留证券计划；确定性 Engine Effect 失败则安全终止当前执行，不把缺失结果改写为零或继续发布。

## 3. 工具权限、幂等与缓存

Renderer 只提交一个 `agent.research.workflow`。模型不能产生任意 Engine kind，只能在以下高级模块闭集中选择子集：`earnings_driver`、`industry_graph`、`relationship`、`market_regime`、`historical_backtest`。`market/evidence/full` 策略在模型规划后由程序强制覆盖，`auto` 也必须通过闭集校验；交易、凭据和存储修改永远不在集合中。

Effect 幂等键由任务、工具 kind 和完整 JSON payload 构成，因而证券、研究区间、数据截止时间、工具策略与高级模块都会参与身份。Host 串行化同一 Worker 通道、读取持久化 Effect 历史并复用成功结果；Rust 数据层继续按来源版本和参数管理读缓存。缓存命中只复用不可变成功结果，失败、跳过、过期和冲突状态仍显式返回。

## 4. 工具防死锁预算

预算是最终安全边界，不是预计完成时间。工具内部的来源切换、退避、熔断和进度事件仍先正常运行。

| Effect | 上限 |
|---|---:|
| 市场/宏观/新闻/候选准备聚合 | 300 秒 |
| 证券证据与高级分析聚合 | 600 秒 |
| 独立报告校验 | 120 秒 |
| 单次完整 MiniMax 报告流 | 180 秒 |
| Renderer 到 Agent 整体请求 | 最高 900 秒 |

任何超时都形成结构化失败或暂停状态；不得以历史旧值、零值或模型猜测填补本轮缺失。

## 5. 运行时不变量

- 每个 reducer 调用和 Effect 结果保持唯一、单调、可重放。
- 不完整模型流不得写入 SQLite；完整流恢复不得拼接前一次草稿。
- 任何 TaskStream 必须产生终态，或由监督器转换为恢复/挂起状态。
- 高级模块失败与跳过必须显示在 `tool_activities`，失败不得伪装为成功证据。
- Host 与浏览器验收 Bridge 的 Agent Effect 白名单必须精确相同。
- 独立报告验证未通过时只能进入 `VerificationFailed`，不能发布报告。
- 自动恢复、Provider 尝试和 Host continuation 都有明确上限。

## 6. 故障测试矩阵

| 场景 | 预期结果 |
|---|---|
| SSE 在 `[DONE]` 前断流 | 丢弃全部部分正文，有界地从头生成 |
| SSE 多字节字符跨 TCP chunk | 按字节重组后再 UTF-8 解码，不产生乱码 |
| 模型选择未知/交易工具 | 规划立即失败，Engine 不执行 |
| Agent 首次请求凭据/存储 kind | Host 与浏览器 Bridge 都在持久化 Effect 前拒绝 |
| 相同成功 Effect 重放 | 从持久化结果命中，不重复执行 |
| 可重放研究 Effect 在 checkpoint 后崩溃 | 使用有界 retry 幂等键恢复 |
| Worker 连续三次心跳失败 | Job Object 监督器重启并重新握手 |
| 鉴权/协议/存储错误 | 立即失败，不自动循环 |
| Token Plan 耗尽 | 挂起并保留检查点，用户恢复后继续 |
| 报告引用或数字无法复现 | `VerificationFailed`，不发布 |

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
