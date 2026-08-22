# Agent 协议与恢复

## 身份与通道

`conversation_id` 表示稳定会话，`run_id/task_id` 表示一次用户轮次。`agent_ask` 接收由前端预先创建的 Tauri v2 Channel，因此最早事件不会落在订阅之前。每个 envelope 有单调 `seq`，前端拒绝同一 run 的重复/倒序事件。

## MiniMax M3 工具协议

一次 assistant `tool_calls` 后，历史中必须为每个 call id 保留恰好一个 `role=tool` 结果。完整 assistant provider content（包括 OpenAI 兼容模式中的 `<think>`）原样持久化并回传给模型；UI 在流式和历史渲染时剥离私有推理。

应用退出可能发生在 assistant 已持久化、部分工具尚未返回之间。恢复前 `reconcile_tool_history` 会：

1. 规范化缺失或重复 call id；
2. 保留已完成且匹配的结果；
3. 为未完成调用插入明确的 `interrupted=true` 工具结果；
4. 丢弃孤立、重复或不匹配的工具结果。

这避免 MiniMax `2013 invalid params: tool call and result not match`，同时不会把未完成调用伪装成成功数据。

## 生命周期与进度

运行事件包含 preparing、reasoning、tools、synthesizing 阶段、模型轮次安全上限和可确定的批次完成量。每个工具展示参数、批次位置、耗时、来源、时间、成功/失败和证据键。

任务在额度错误 2056 时持久化为 suspended；应用重启后，存储中 status=running 但没有 live handle 的任务在 UI 显示为 interrupted。两者均可继续。

## 上下文压缩

超过字符预算时，不调用额外模型，而是确定性生成“工作状态快照”：目标、已完成工具及参数、缓存键、证据来源、当前轮次和继续指令。系统提示与最近消息原样保留，工具调用/结果对不拆分，重复压缩不嵌套。

## 安全边界

Agent 只能产出研究结论和人工方案，不具备券商登录、订单提交或自动执行工具。重要数字必须来自确定性工具，最终报告保留证据来源和时间。
