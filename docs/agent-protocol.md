# Agent 协议、恢复与发布边界

本文描述 v6 当前实现。历史 Tauri Channel、旧 Rust Agent 和旧聊天消息格式只属于迁移基线，不是可调用的生产接口。

## 进程与身份

React Renderer 只通过声明过的 Proton typed bridge 调用桌面 Host。MoonBit Agent Worker 负责纯状态转移、任务编排、动态澄清、Provider 调用和报告结构校验；Rust Engine 负责确定性金融计算、数据访问、持久化和凭据。Agent 不读取数据库路径，也不直接调用 Rust crate。

`conversation_id` 是稳定会话身份，`task_id` 是一次研究任务身份。请求使用协议 v1 envelope，并携带 `request_id`、`deadline_ms` 和 `cancellation_id`。Worker 标准输出只允许 4 字节 little-endian 长度加 UTF-8 JSON 的协议帧，单帧上限 8 MiB；结构化日志写入标准错误。

`protocol/schema/agent.schema.json` 同时定义并生成跨语言稳定模型：`TaskSpec`、`AgentQuestion`（与单个动态澄清问题同构）、`ConversationSummary`、`TaskCheckpoint`、`ToolActivity`、`EvidenceRef`、`VerificationFinding` 和 `ProviderQuota`。Rust、MoonBit 与 TypeScript 生成物必须由 `protocol-codegen --check` 保持一致；Renderer 不再为这些公开模型维护另一套手写形状。

Renderer 使用稳定的 Agent 服务外观 `task.create/list/get/branch/resume/cancel/answer`。这七个方法也由 schema 生成白名单，但为保持协议 v1 兼容，它们不会扩张底层 Worker wire：状态变更映射到 Host 持久化的 `agent.start/event/research.workflow`，历史列表、任务读取和消息分支映射到 Engine 的有界只读/分支服务。外观不能调用任务创建日志、事件追加、Effect 或 checkpoint 写原语。

## 持久化真相与 Effect

任务状态由 `reduce(state, event) -> (state, effects)` 推进。网络、时间、工具、存储和日志都在 reducer 外执行。桌面 Host 在执行工具前，先把用户输入、任务事件、检查点和 Effect 意图写入 Engine；工具结果带幂等键持久化后，才作为新事件继续 reducer。恢复时根据持久化事件和检查点重放，重复、过期、跳号或乱序事件会被拒绝。

Host 是通用 Effect runner，不包含金融算法。当前生产路径只允许 Agent 发出目标为 Engine 的声明式工具请求；Renderer 不能绕过 Host 获取进程、文件、数据库或凭据能力。单次 continuation 循环有硬上限，超过上限会保留检查点并显式失败。

## 生命周期、取消与恢复

稳定阶段包括 `Idle`、`Preparing`、`WaitingForUser`、`Reasoning`、`AwaitingTools`、`Reviewing`、`Synthesizing`、`Verifying`、`Suspended`、`Completed`、`VerificationFailed`、`Cancelled` 和 `HardFailed`。任务事件序列单调；终态吸收；取消和 Effect 结果写入具有幂等语义。

页面切换不会卸载 Agent 主视图。会话、任务、事件、Effect、检查点和报告保存在 Engine 数据库中，React 状态只用于当前视图；会话中的任务快照只能展示，不能作为可执行恢复状态。打开历史或重试前，Renderer 必须读取 `agent.task.load`，核对 `task_id` 与 `accepted_seq` 后才允许继续，读取失败时按钮保持关闭。会话支持列表、服务端搜索、新建、重命名、软删除、恢复，以及从指定用户消息或检查点分支为新研究。搜索最多返回协议页大小范围内的结果，不会把全部历史搬入 Renderer。

Provider 额度不足、已知的额度暂停和可恢复断流进入 `Suspended` 并保留检查点；用户可在额度恢复后继续。无法恢复的协议错误、损坏状态或校验失败会进入显式失败状态，不伪装成完成。

Agent Worker 使用单一有序双向管道，因此显式停止不是在同一管道后面排队的普通消息。Host 收到持久化 `event_kind=cancel` 时会先终止正在执行的 Agent Worker，使活动帧读取失败并把未完成操作记为失败；随后重新启动、完成版本/能力握手、恢复最后一个已提交检查点，再把取消事件写入日志并转换到 `Cancelled`。任何请求超时也会废弃该 Worker 通道，下一次调用必须重启握手，禁止把迟到响应误认作下一请求。取消不会发布未校验报告，已持久化工具结果仍可供审计。

## 动态澄清

任务类型声明必填业务字段。预算、数据截止时间、研究期限或风险边界等缺失时，reducer 进入 `WaitingForUser`，不会静默采用业务假设。MoonBit Worker 请求 MiniMax 根据当前 `TaskSpec` 生成一至三个问题；每题包含二至六个模型生成选项、自由输入能力、目标字段以及“由 Agent 选择最优方案”选项。

Worker 校验问题数量、选项数量、字段覆盖和目标字段合法性。所有缺失字段都被有效问题覆盖后，选择卡片才会显示。回答作为同一会话的新持久化用户事件继续任务；未回答时不执行候选发现、工具或报告合成。旧 `astock-questions` 围栏只用于读取历史兼容内容，不是 v6 Worker 的公共传输格式。

## 研究工作流

默认人工计划按以下有界阶段执行：

1. 校验 `TaskSpec`，必要时动态澄清；
2. 获取市场环境和有界候选池；
3. 模型只从候选池中选择一至五个证券进入深度取证；
4. Engine 聚合行情、K 线、资金、基本面、估值、新闻、公告、来源时间和数据质量；
5. MiniMax 顺序执行证据审查、独立反方审查和最终综合三个阶段；
6. MoonBit 运行确定性的结构与安全边界校验；
7. 通过后保存报告和检查点，否则进入 `VerificationFailed` 或暂停补证。

三个模型阶段当前是顺序执行，不是并行专家系统。`multi_agent_enabled` 和 `max_parallel_agents` 是协议 v1 的兼容保留字段，v6.0.0 不把它们展示为可用能力，也不根据它们改变生产工作流。配置页只允许选择实际存在于 MiniMax 模型目录的主分析、快速结构化、深度综合和反方复核模型；不可用模型不会静默回退。

候选发现不等同于推荐。最终报告可以淘汰所有候选并保留现金。人工计划必须检查预算、一手成本、费用和滑点、现金备用金、单一证券与组合风险上限、触发条件、失效条件和复核时间，并明确只能由用户人工执行。

## Provider 与隐私

MiniMax Plus 凭据只存放在 Windows Credential Manager，由 Agent Worker 在本进程读取；不得进入命令行、SQLite、日志或 React/Zustand 状态。模型目录探测和额度查询使用当前凭据的官方接口。HTML、空响应和非 JSON 网关响应会转换为有界的可操作错误，不把响应正文或密钥显示给用户。

结构化响应最多有三次有界尝试，使用已验证的快速、主分析和复核模型接力。最终研究报告由 MoonBit Worker 直接读取 MiniMax OpenAI 兼容 SSE：按字节跨 TCP 分片组帧，仅接收 `delta.content`，忽略 `reasoning_details`，并且只有收到 `[DONE]` 才允许进入后续校验。断流、畸形事件、超大行、超大响应或超时会丢弃该轮全部半成品，并从同一持久化输入重新生成完整报告；不会把两次生成拼接成结论。该恢复最多三次、单次 180 秒、传输 2 MiB、可见正文 120 kB，仍失败时保留检查点并进入可恢复失败。MiniMax 返回的 `<think>` 私有推理同样不会写入用户会话、工具活动或最终报告。工具活动只公开输入摘要、状态、耗时、缓存命中和证据，不展示私有推理链。

## 缓存、证据与数据质量

工具缓存按工具版本、规范化参数、证券、数据截止时间和 `source_version_id` 寻址，并由 Engine 负责 single-flight、过期策略和重放幂等。缓存命中仍按当前时间重新计算新鲜度。缺失、单源、过期、冲突或口径不兼容的数据必须保留真实质量状态，不能转换为零或确定性结论。

研究上下文包含有界 `evidence_inventory`、来源、抓取时间和数据版本。模型被要求让关键判断紧邻说明证据类别，并列出反方证据、不确定性和未覆盖能力。当前 MoonBit 发布门禁实际检查：

- 最低报告长度；
- 私有推理不泄露；
- 必需章节完整；
- 人工执行和不自动下单边界；
- 人工计划所需的资金、现金、一手、触发、失效和复核内容；
- 上下文包含证据清单与抓取时间。

字段级 Engine 门禁在独立、已持久化的 `research.agent_report_verify` Effect 中执行。行情聚合时为有界标量生成内容寻址的 `evf_*` 编号、JSON Pointer、原值、来源、观察时间、来源版本和质量阻断状态。最终报告每个含数字、百分比、日期、证券代码或价格的事实行都必须引用真实编号；未知编号、缺少时间/版本、质量阻断证据、无引用数字，以及无法从同一行引用原值按单位换算复现的数字都会阻止发布。用户预算和一手规则也使用明确的任务/规则证据编号，不享受文字豁免。

该门禁验证“报告没有超出输入证据”，但不独立证明外部 Provider 原始事实必然正确，也不判断未来投资收益。上游真实性仍依赖跨源对账、一级来源版本和外部服务可信边界。当前实现已通过 Rust 单元测试、MoonBit reducer pending-Effect 测试和桌面 Host 测试；真实 MiniMax 完整流程在凭据轮换和浏览器验收完成前仍标记为 `NOT VERIFIED`。

## 安全边界

Agent 只能生成研究结论、回测结果和人工交易计划。产品没有券商登录、订单提交、自动下单或无人值守交易能力。模型审查的一致性不等于事实成立，更不构成收益保证；最终使用者必须检查来源、时间、数据质量、风险条件和可成交性后人工决定。
