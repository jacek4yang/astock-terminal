# 架构

## 分层

1. `crates/core`：证券、行情、来源、缺失与质量等稳定领域类型。
2. `crates/tdx`、`crates/market-data`、`crates/fundamental`：协议、数据源适配、字段级聚合、缓存、质量校验和熔断。
3. `crates/technical`、`crates/chanlun`、`crates/quant`、`crates/backtest`、`crates/graph`：确定性研究引擎。
4. `crates/minimax`、`crates/agent`：模型协议、额度调度、工具注册、持久任务与证据报告。
5. `src-tauri`：强类型桌面命令、后台作业和应用共享状态。
6. `ui`：React 工作台；数据经 Tauri v2 IPC 进入，不直接持有上游凭证。

## 核心数据流

证券代码先由 `SecurityMaster` 解析身份；行情层按字段能力组合 TDX 的快速价格/五档、主数据名称和其他源的换手等字段。每个关键字段携带 `source/as_of/fetched_at/quality/missing_reason`。K 线进入确定性质量过滤后才交给分析、图谱、交易计划或回测。

Agent 只调用确定性工具。模型决定研究路径并综合证据，不能制造价格或财务数字。完整工具结果写入缓存，模型收到摘要、`cache_key` 和稳定字段证据编号，需要时再下钻。最终文本必须先通过独立的引用、数字、单位、时效、来源与冲突校验器；校验失败会触发有限修订，仍不合格则阻止草稿发布。结构化结论、反证、失效条件、校验结果及工具/数据版本一并保存。

## 后台作业

- Agent：持久化 conversation/run/message/task，使用 Tauri Channel 先建通道再运行。
- 全市场扫描：后端快照、增量事件、轮询兜底和协作式取消。
- 回测：后端 job 快照、页面重进恢复、结果保留和取消令牌。

页面卸载不会终止这些作业。Agent 的 UI 会话和输入草稿由持久 Zustand store 保存；只有“新对话”清空。

## 可靠性边界

上游访问共享主机级调度器；健康基线 75ms，失败惩罚最高 30s。K 线 provider 具有三次瞬态失败熔断和 10 分钟至 1 小时冷却。不同数据类别按 2–120 秒 TTL 缓存。模型推理流最多四路并发，建流错误按全抖动指数退避。
