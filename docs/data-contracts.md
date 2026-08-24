# 数据契约

## Security Master

`SecurityRecord` 是代码、交易所、板块、资产类型和中文名称的权威应用内身份。实时 quote 不能覆盖其不具备的名称能力。SQLite v6 迁移持久化主数据，启动时合并内置回归身份、缓存记录、TDX 完整列表和东方财富列表。

## 行情与来源

可缺失数值使用 `Option`。`FieldProvenance` 至少描述：

```text
source / as_of / fetched_at / stale / quality / missing_reason
```

价格、名称、换手可以来自不同来源。前端按 nullable 类型渲染，禁止用真假判断把合法 0 和缺失混为一谈。

## Agent IPC

历史消息在 Rust 侧先规范化为：

```text
id, role, content:string, tool_calls[], tool_call_id?, created_at, malformed
```

流事件统一包裹为：

```text
run_id, conversation_id, seq, event
```

工具开始/结束事件携带 `call_id`，并行同名工具不会错配。结束事件包含成功状态、耗时、来源、抓取时间、缓存键和安全错误信息。

## 时间单位

业务时间使用带时区 ISO 字符串；任务列表为 Unix 秒；MiniMax Token Plan 原始窗口及 `fetched_at` 为 Unix 毫秒。前端的额度格式器兼容秒/毫秒，但新契约固定输出毫秒。

## 兼容策略

外部 schema 使用容忍式 optional 解析，稳定 Engine 协议响应优先强类型。
Renderer 只使用生成的 Proton typed bridge，不保留 Tauri/WebView2 兼容层。
旧 Agent 记录若不是合法 provider message，则作为纯文本返回并标记
`malformed`，不能让历史页崩溃。
