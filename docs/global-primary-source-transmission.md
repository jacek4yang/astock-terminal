# 海外一级来源与 A 股传导研究

## 目标与边界

“全球传导”把海外监管披露、宏观指标、贸易政策、能源与商品数据保存为可追溯的一级来源记录，再通过带证据的实体和供应链关系映射到 A 股。系统不把媒体转载、搜索摘要或模型常识写成确定性关系，也不执行海外交易。

每条可用于研究结论的记录必须保留：来源机构、原始 URL、原始发布时间与 IANA 时区、北京时间、抓取时间、来源版本、原始语言、原始计量单位/币种、中文翻译状态、许可说明和原文归档校验值。缺少任一关键证据时，界面和 Agent 都必须明确显示“来源缺口”。

## 官方来源目录

内置目录目前覆盖 21 个官方入口。目录只描述可用能力；未配置凭据或暂未实现适配器的入口不会被假装成已抓取。

| 区域 | 官方入口 | 主要用途 | 凭据与频率约束 |
| --- | --- | --- | --- |
| 美国 | SEC EDGAR | 上市公司申报、XBRL facts | 无 API key；必须声明 User-Agent；不超过 10 次/秒 |
| 中国香港 | HKEXnews | 公告、财报 | 官方网页/API，遵守站点许可与节流 |
| 日本 | EDINET | 有价证券报告、XBRL | 需要 API key；官方建议轮询不高于每分钟一次 |
| 韩国 | OpenDART | 公司披露、财务指标 | 需要 crtfc_key |
| 中国台湾 | MOPS | 公司公告、财报 | 官方公开入口，保守节流 |
| 美国 | Federal Reserve、BLS、BEA、USTR、EIA、CFTC | 利率、就业、国民经济、贸易政策、能源、持仓 | 各自独立限流与失败退避 |
| 欧洲 | ECB、Eurostat、EU Trade | 汇率、宏观、贸易与政策 | 按官方接口许可与频率 |
| 国际组织 | IMF、World Bank、BIS、UN Comtrade、WTO | 宏观、跨境金融、贸易 | World Bank v2 无需密钥；其他按官方要求配置 |
| 能源 | OPEC、IEA | 原油与能源供需 | 许可不明确或需订阅时只标记目录，不抓取受限正文 |

实现优先参考官方文档：

- SEC EDGAR API：https://www.sec.gov/search-filings/edgar-application-programming-interfaces
- SEC 开发者与公平访问：https://www.sec.gov/about/developer-resources
- World Bank Indicators API：https://datahelpdesk.worldbank.org/knowledgebase/articles/889392
- World Bank 查询结构：https://datahelpdesk.worldbank.org/knowledgebase/articles/898581-api-basic-call-structures
- EDINET API：https://disclosure2dl.edinet-fsa.go.jp/guide/static/disclosure/WEEK0060.html
- OpenDART：https://opendart.fss.or.kr/guide/detail.do?apiGrpCd=DE005&apiId=AE00055
- TWSE MOPS 说明：https://www.twse.com.tw/en/about/company/guide.html
- EIA API v2：https://www.eia.gov/opendata/documentation/APIv2.1.0.pdf

## 数据与证据契约

数据分为六层：来源健康状态、原始文档、法律实体、带证据关系、可修订观测值、汇率版本。原始文档先归档，关系和观测值才能入库；关系必须引用归档版本、原文位置、证据摘录、观察时间和置信度。

证据路径采用有向图查询，避免循环并逐边合成置信度。只有同时包含“海外一级原文”和“境内公司一级原文”的路径，才能激活到具体 A 股的确定性结论。四条黄金链路模板为：

1. 半导体：海外政策/客户 → 受限产品与工艺 → 国产替代或需求冲击 → A 股设备/材料公司。
2. 消费电子：海外品牌财报 → 产品需求与库存 → 零部件/组装环节 → A 股消费电子公司。
3. 新能源：海外需求/能源政策 → 电池与能源成本 → 材料/设备订单 → A 股新能源公司。
4. 资源品：全球供需/库存/持仓 → 商品价格冲击 → 收入/成本暴露 → A 股资源品或下游公司。

## 时间、翻译、币种与修订

- 海外发布时间先以来源所在地 IANA 时区解析，再保存 UTC 和北京时间。夏令时歧义必须明确选择较早/较晚实例；不存在的本地时间直接拒绝。
- 海外盘后或 A 股休市时发布的信息映射到下一个 A 股交易日，不把自然日当交易日。
- 翻译前保护证券代码、数字、百分比、货币金额、法律实体名称和关键术语；保护标记丢失时翻译失败，不静默改写数值。
- 币种换算使用定点整数和带版本的汇率，不使用浮点近似；原币和原单位始终保留。
- 宏观数据按 `period + vintage` 保存修订版本，回测按当时可见版本读取，避免未来函数。

## 后台任务、限流与失败隔离

同步任务没有总超时；页面切换后仍继续运行，并显示阶段、百分比、预计剩余时间、来源总数、可访问数、配置缺口、发现文档、原文归档、保存观测、证据路径和失败数。用户可取消任务或复制完整诊断信息。

每个来源拥有独立的节流、重试和健康状态。瞬时错误使用指数退避；认证、许可和参数错误不会高频重试。单个来源失败不阻断其他来源，结果中保留具体缺口。SEC 只有在配置 `ASTOCK_SEC_USER_AGENT` 且用户提供 CIK 时才访问；World Bank 可直接同步官方指标。

## Agent 使用规则

Agent 工具 `research_global_transmission` 只读取已归档文档、证据路径、来源缺口和黄金链路契约。回答海外申报、制裁、贸易政策或商品变化影响哪些 A 股时，必须逐条给出证据、时间、来源版本和置信度；无法闭合的边必须作为待验证假设，不允许用常识补全。

## 验证

主要自动化覆盖：夏令时歧义与空档、美国盘后映射下一 A 股交易日、翻译保护、定点币种换算、点时修订、四条黄金链路、未归档关系拒绝、图路径循环防护、SEC 列式申报解析、World Bank 指标解析、来源一级性识别、前端后台进度与详情展示。

建议发布前执行：

```text
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
npm test -- --run
npm run build
```
