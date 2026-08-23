# 正式披露数据面

## 目标与信任边界

正式披露中心把上交所、深交所、北交所、巨潮资讯、证监会和上市公司投资者关系网站视为一级入口。东方财富公告大全等聚合页面只用于发现和补漏，数据库字段固定为 `mirror_discovery`；它们不能被 UI 或 Agent 提升为“正式原文已核验”。

一条记录只有同时满足以下条件才是 `primary_verified=true`：

1. 至少一个入口的 authority 是交易所、监管机构或发行人；
2. 已通过受控抓取保存不可变 `source_version_id`；
3. 页面/PDF 解析没有进入访问墙、动态空壳或 OCR 待复核状态。

公开入口以各机构当前网站为准：

- 上交所：`https://www.sse.com.cn/disclosure/listedinfo/announcement/`
- 深交所：`https://www.szse.cn/disclosure/listed/notice/`
- 北交所：`https://www.bse.cn/disclosure/announcement.html`
- 巨潮资讯：`https://www.cninfo.com.cn/new/index`
- 证监会：`https://www.csrc.gov.cn/csrc/c100028/common_list.shtml`
- 公司 IR：只读取证券主数据中显式配置、允许自动访问的入口。

各入口有独立访问上限、目标发现延迟、增量游标、连续失败计数和指数退避时间。上游失败不会阻断已经成功的入口，也不会删除已入库记录。界面的“来源频率与健康状态”会显示这些运行状态。

## 数据模型

- `disclosures`：规范公告、类别、状态、发布时间精度、首次发现、解析状态和修订/撤回关系。
- `disclosure_securities`：稳定代码关联；公告时简称作为历史名称保存，支持公司更名和一份公告关联多个证券。
- `disclosure_sources`：每个入口单独保存 authority、原始 URL、上游编号、发现时间和延迟。跨入口只合并规范公告，不覆盖入口。
- `disclosure_attachments`：原文和附件的父子层级、MIME、哈希、页数、解析器版本、源文档版本和复核原因。
- `disclosure_events`：合同、业绩预告、回购、持股变动、限售解禁、停复牌、处罚、诉讼、担保、质押、产销与资本开支等确定性事件。
- `disclosure_provider_state`：每个入口的游标、SLA、失败、重试和最近成功状态。

去重键由规范标题、发布日期和排序后的证券代码组成。更正、修订、补充、勘误、取消、撤回、作废等标题会建立版本关系；取消记录会反向写入旧版 `cancelled_by`，避免 Agent 继续使用失效数字。

## PDF、表格和扫描件

源文档解析器版本为 `source-evidence-v2`。字段证据保留字符 span、PDF 页码；可确定时还保留页面坐标和表格的表号/行号/列号。无法证明的坐标保持空值，绝不猜测。

没有可靠文本层的 PDF 标记为 `ocr_review_required`，不会产生结构化事件。后续 OCR 必须作为新的解析版本保存，并在人工复核后才能用于事实级结论。这样可以防止扫描错误静默污染合同金额、业绩数字等关键字段。

## 后台任务与诊断

`disclosure_sync_start` 只创建后台任务，不限制执行时长。切换页面后任务继续运行。`disclosure_sync_status` 返回当前来源、当前公告、发现/规范化/新增/去重/正式核验/待复核/失败数量、进度、非强制预计剩余时间和有界日志；`disclosure_sync_cancel` 协作取消并保留已完成增量。

同步日志和错误可在界面逐层展开并复制，不记录凭据。预估时间仅用于展示，不是超时或自动降级条件。

## Agent 纪律

事件类研究先调用 `research_disclosures`，再用 `research_news` 补充媒体报道。只有 `primary_verified=true` 且 `source_version_id` 非空时，公告正文才可标注为【事实】；只有镜像入口时必须写“原文未核验”，不得增加仓位建议或结论置信度。修订和撤回记录必须沿版本链解释，不能混用旧版数字。

## 回归覆盖

自动测试覆盖跨入口去重、镜像不冒充正式来源、修订/撤回、公司更名、多证券关联、分类筛选、分页、指数退避、HTML 表格单元格、PDF 页码/单位和扫描件 OCR 待复核。正式入口的网络可用性属于运行时健康状态，不用固定测试假装永远成功。
