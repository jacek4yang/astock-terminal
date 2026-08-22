# tdxrs 上游审计与本地差异

- 上游：<https://github.com/jiangtaovan/tdxrs>
- 审计提交：`b39ea12ea8e85fd3c83f1f9f646d3cf9501ba3f5`
- License：MIT，Copyright (c) 2026 Chiang Tao / tdxrs Contributors

项目未直接依赖 tdxrs crate，以避免其 Python/pyo3 打包成本。`astock-tdx` 参考并重新实现了公开 TDX 协议中的帧、握手、证券列表、K线、五档、分钟以及服务器探测/切换思路；实现适配 tokio、项目错误体系和现有 provider 架构。

本地差异：

- 只保留桌面研究终端已验证并接入产品的数据能力；
- 连接池、心跳、黑名单和熔断与 `astock-market-data` 组合；
- 价格/成交量单位在 adapter 边界统一为核心领域类型；
- TDX 不提供的名称/换手等字段由 Security Master 和其他 provider 合并；
- qfq/hfq 由项目统一调整语义处理，不把协议保留位当作复权结果；
- 北交所能力不做 TDX 支持承诺。

更完整的协议审计见 [data-source-tdx.md](data-source-tdx.md)。
