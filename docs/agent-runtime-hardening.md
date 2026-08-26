# Agent Runtime 加固

本文描述 `astock-agent-runtime` 在模型流、工具执行、证据校验和任务中断方面的
运行时约束。目标不是承诺上游永不失败，而是保证失败**有边界、有状态、可诊断、
可恢复**，不出现无限等待，也不出现没有终态的退出。

> 历史说明：本文早期版本描述的是 v6 Proton/CEF 宿主中的 MoonBit Agent Worker。
> 生产实现已经收敛到 Rust `astock-agent-runtime`，因此下述约束以当前 Rust 运行时
> 为准。MoonBit 模型与 `formal/` TLA+ 规格仍作为规格与模型检验资产保留。
> 表中数值取自 `RuntimeConfig::default()`，改代码时必须同步改本文。

## 1. 模型流的提交边界

MiniMax SSE 被划分为两个阶段：

- **pre-commit**：尚未向上层交付任何用户可见文本、工具调用或结束原因。仅含
  `reasoning_content` / `reasoning_details` 的 chunk 只存在于内存中。
- **committed**：第一段可见文本、第一个工具调用或结束原因已经出现。此后
  **不允许在 provider 层重放整次请求**。

已提交后断流直接返回瞬时错误，由上层从最近一次完整持久化的轮次恢复。这条边界
的作用是防止重复文本、重复执行工具，以及破坏 `tool_call_id → tool result` 的
一一对应关系。

私有推理永不外泄：`<think>` 段、`reasoning_details` 以及只包含思考内容的响应
都不会进入可见输出、任务历史或durable事件。该性质由
`fragmented_private_reasoning_never_becomes_visible` 覆盖，包括推理文本被切成
多个 chunk 的情况。

默认边界：

| 参数 | 默认值 | 作用 |
|---|---:|---|
| provider 连接超时 | 90 秒 | 永久无首包时终止本次连接 |
| chunk 空闲超时 | 120 秒 | 流建立后长期无数据时终止连接 |
| 每轮最大 chunk 数 | 10 000 | 防止无终止流耗尽资源 |
| 每轮可见正文上限 | 120 000 字符 | 越界按失败处理并关闭 effect |
| 每轮最大工具调用 | 32 | 限制单轮扇出 |
| 工具参数上限 | 256 000 字符 | 防止参数无限增长 |
| 工具结果上限 | 2 MiB | 越界作为**有界失败**回传模型 |
| 最大模型轮数 | 16 | 防止工具循环不收敛 |
| 并行工具上限 | 4 | 仅只读工具可并行 |

正常完成必须观察到结束原因。SSE 在结束标记前关闭不会被误判为完整回答。

## 2. 失败分类与恢复

自动恢复只用于保守识别的**瞬时**错误：网络中断、连接重置、EOF、SSE 中断、
429/限流、超时、502/503/504。

以下错误**不进入**重试循环，而是直接成为终态或挂起：

- 鉴权失败与无效凭据（`provider_authentication_failure_is_terminal_failed`）；
- API 业务错误与响应解析错误；
- 工具调用历史协议损坏；
- 存储错误；
- 超过最大模型轮数；
- 证据校验失败；
- 用户取消。

限流发生在部分流之后时，任务进入 `Suspended` 而不是 `Completed`，避免把截断的
回答当成成品交付（`provider_rate_limit_after_partial_stream_is_suspended_not_completed`）。
空闲超时同样是有界挂起（`provider_idle_timeout_is_bounded_and_suspended`）。

## 3. 工具执行

工具注册表是封闭的：模型只能调用注册过的类型化工具。未知工具**在到达 Engine
之前**失败（`unknown_model_tool_fails_closed_and_never_reaches_engine`），因此
模型无法通过编造工具名扩大权限。运行时不暴露任意文件系统访问、进程执行或模型
生成代码；计算语言是有燃料计量的求值器，没有任意执行。

分片到达的工具参数会被重建成单个 JSON 对象，且保留原始不透明字符串
（`fragmented_tool_arguments_reconstruct_one_json_object`）。超限的工具结果不会
静默截断成"成功"，而是作为有界失败对模型可见
（`oversized_tool_result_is_visible_to_model_as_a_bounded_failure`）。

## 4. 取消

取消是协作式的，覆盖两个位置：等待中的 provider 流
（`cancellation_interrupts_a_pending_provider_stream`）和正在执行的 Engine 工具
（`cancellation_reaches_a_cooperative_engine_tool`）。取消后任务进入 `Cancelled`
终态，不留伪运行状态。

`/cancel` 与"先停一下"解析为同一个 `UserIntent::Cancel`，因此两条路径不会分叉。

## 5. 持久化与恢复

意图和 effect intent 在执行副作用**之前**持久化；重复、过期与重放输入按幂等
处理。任务投影完整落盘
（`embedded_engine_persists_the_complete_headless_task_projection`），可见计划
（`Plan`）随任务状态持久化，因此中断后恢复时用户看到的工作分解不会丢失。

可选字段被省略而不是编码成 `null`
（`task_optional_fields_are_omitted_not_encoded_as_null`），旧版本会话仍可读取
（`completed_v6_conversation_without_rust_version_remains_readable`）。

## 6. 上下文边界

长会话保留全部原始消息，同时对模型上下文设上限：最近 40 条用户/Agent 消息且
不超过 120 000 字符（`compaction_keeps_full_history_and_bounds_model_context`、
`long_session_uses_summary_plus_bounded_recent_history`）。摘要是**历史上下文**，
不得静默变成当前市场证据。

## 7. 发布闸门

存在阻塞性校验发现时不得作为成功报告发布。上游部分失败呈现为**降级覆盖**，
而不是静默成功或补零。计划中的 `Degraded` 与 `Done` 是不同状态，正是为了让降级
在最终报告里保持可见。

## 尚未验证的部分

- 真实 provider 行为仍是受信边界，需显式选择加入的实时测试；
- 精确 effect 检查点重放与语义压缩尚未完成，属于迁移中的工作；
- 上述性质由确定性集成测试覆盖，不等于实时数据正确性。
