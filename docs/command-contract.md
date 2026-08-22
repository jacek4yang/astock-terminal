# Tauri 命令层契约(M3 实现依据,UI 与后端共用)

所有命令均为 Tauri command,参数/返回为 JSON(snake_case)。错误统一 `{ "error": string, "kind": string }`。

## 行情数据(astock-market-data)
- `get_quote(symbol: string) -> Quote` — {symbol,name,price,pct,change,high,low,open,pre_close,volume,amount,turnover,timestamp}
- `get_kline(symbol, period: "day"|"week"|"month", adjust: "qfq"|"hfq"|"none", count: number) -> { bars: Bar[], source: string }`
  - Bar: {date,open,close,high,low,volume,amount,pct,turnover}
  - **持久读穿缓存**:按 `{symbol}/{period}/{adjust}.parquet` 落盘。缓存最新 bar 覆盖最近交易日(周/月线按周首/月首判定)时直接读 parquet,`source: "cache"`,零网络;过期则从市场上行拉取并按日期增量合并后返回,`source` 为实际上行(tencent/sina/eastmoney)。交易时段内当日 bar 视为未完结,每个 key 至多 60s 刷新一次;15:00 后当日完结,直接走缓存。刷新失败但缓存非空时降级返回旧缓存。
- `get_stock_bundle(symbol, period, adjust, count) -> { quote, kline: {bars, source} | null, fund_flow_30d: FundFlow[] | null, analysis: SignalJson | null, chanlun_daily: ChanlunDailyJson | null, missing: string[] }`
  - 股票页一次取数:K 线只拉一次(走上面的持久缓存),`analysis` 与 `chanlun_daily` 由同一组 bars 推导;资金流走自带 15s TTL 通道。除 `quote` 失败为硬错误外,各分区独立降级:null + `missing` 记录分区名(`kline` 缺失时 `analysis`/`chanlun_daily` 一并缺失)。
- `get_provider_health() -> [{name, state: "closed"|"open"|"half_open", cooldown_remaining_secs: number|null}]` — 各数据源熔断器快照,供设置页健康面板。
- `get_news_provider_health() -> [NewsProviderHealth]` — 返回每一路财经资讯来源的可信层级、采集方式、许可、刷新/频率上限、延迟、成功时间、失败率、陈旧状态、持久游标和熔断状态；只读本地快照，不访问上游。
- `set_news_provider_enabled(provider_id, enabled) -> void` — 持久启停单一财经资讯来源；停用后该来源的最后成功副本也不会参与研究。
- `get_news_archive_recent(limit) -> [ArchivedNewsRevision]` — 查询重启后仍存在的最新资讯档案，不返回受限原始快照。
- `get_news_archive_revisions(document_id) -> [ArchivedNewsRevision]` — 查询一篇来源文档的不可变修订链。
- `check_news_archive_integrity() -> string` — 运行本地 SQLite 快速完整性诊断，正常返回 `ok`。
- `get_news_ingest_observations(provider_id?, limit) -> [NewsIngestObservation]` — 查询来源最近的抓取/解析诊断；返回错误与证据哈希，但不返回原始正文。
- `get_news_event_clusters(limit) -> [EventCluster]` — 查询事件簇摘要、独立来源数、证据多样性、时间、最新修订和冲突字段。
- `get_news_event_cluster_detail(cluster_id) -> EventClusterDetail` — 查询事件全部文档修订、关系、旧闻标记、合并/分离解释及字段级冲突。
- `merge_news_event_clusters(from_cluster_id, to_cluster_id, reason) -> EventClusterDetail` — 将事件人工合并；理由不能为空，操作追加写入审计记录。
- `split_news_event_revision(revision_id, reason) -> EventClusterDetail` — 将单一修订人工拆为独立事件；理由不能为空，历史成员记录不覆盖。
- `get_pending_news_evidence_reviews(limit) -> [AgentConclusionReview]` — 查询因更正或撤回而进入待复核状态的 Agent 结论。
- `resolve_news_evidence_review(task_id, conclusion_key, triggering_revision) -> void` — 显式标记一项结论已经人工复核。
- `get_news_entity_links(revision_ids) -> [DocumentEntityLink]` — 批量查询资讯原文 span、候选实体、消歧理由、关系链、规则版本、置信度和 Agent 可用状态。
- `get_entity_link_reviews(limit) -> [EntityLinkReview]` — 查询低置信、歧义或模型提议的待审核实体映射。
- `resolve_entity_link_review(link_id, entity_id?, accept, reason) -> bool` — 在候选列表中人工确认或拒绝映射；理由不能为空，接受时仍要求精确修订证据。
- `fetch_source_document(url) -> SourceDocumentDetail` — 受控读取 HTML/JSON/PDF/正式附件，保存不可变来源版本，并返回原值、原单位、页码/段落/span；访问失败仍持久化 `unverified` 诊断。
- `get_source_documents(limit) -> [SourceDocumentSummary]` — 查询最近来源、来源层级、一级来源标记、读取状态与失败原因，不发起网络请求。
- `get_source_document(source_version_id) -> SourceDocumentDetail` — 按 `srcver:...` 读取不可变版本、原文分段和字段证据。
- `compare_source_evidence(source_version_ids) -> [EvidenceConflict]` — 对 2–10 个版本逐字段展示冲突值及精确证据，不自动裁决。
- `get_data_quality_slo(window_secs) -> [DatasetSlo]` — 从本地真实观测计算分数据集/来源的错误率、P50/P95、最近成功、连续陈旧、缺失和冲突；不访问上游。
- `get_data_quality_observations(dataset?, provider?, limit) -> [QualityObservation]` — 查询成功、失败、缓存命中、延迟与质量门禁明细。
- `get_field_lineage(dataset?, entity_key?, limit) -> [FieldLineageRecord]` — 查询字段的来源、时点、解析/结构版本、许可、单位、币种、复权和财务口径。
- `get_data_reconciliations(dataset?, entity_key?, limit) -> [ReconciliationAudit]` — 查询双方原值、偏差、容差、口径与阻断状态。
- `reconcile_quote_sources(symbol) -> QuoteReconciliationReport` — 通达信与东方财富并发取数并逐字段校验；不足两个成功来源时明确阻断高置信。
- `reconcile_valuation_sources(symbol) -> ValuationReconciliationReport` — 东方财富与已配置的聚宽/Tushare 并发对账；单位换算显式完成并保留原来源。
- `get_data_health_report(window_secs) -> DataHealthReport` — 生成可复制的真实连续健康报告；样本覆盖不足时不补造历史。
- `get_minute(symbol) -> { points: {time,price,avg_price,volume}[], pre_close, name }`
- `search_stocks(keyword) -> {code,name,classify}[]`
- `get_market_breadth() -> {up,down,flat,total,breadth_ratio}`
- `get_all_a_shares() -> {code,name,price,pct,amount}[]`
- `get_fund_flow(symbol, days) -> {date,main_net,super_large_net,large_net,medium_net,small_net,main_pct}[]`
- `get_realtime_flow(symbol) -> {points:{time,main_net,...}[], summary:{...}}`
- `get_index_kline(secid: string, count) -> Bar[]`

## 分析引擎(astock-technical / astock-chanlun)
- `analyze(symbol, period) -> SignalJson` — 与旧版 signal_to_dict + 优化后处理完全同形(见 fixtures/golden/*/outputs/signal)
  - **短缓存**:结果写入 `tool_cache`,交易时段 TTL 60s、收盘后 4h;缓存键含全部参数与 K 线缓存最后 bar 日期,新 bar 出现即自然失效。
- `chanlun_daily(symbol, period, count) -> ChanlunDailyJson` — daily_result_to_dict 同形;缓存策略同 `analyze`(键含 count)。
- `chanlun_minute(symbol) -> ChanlunMinuteJson`

## 基本面分析(astock-fundamental)
- `get_fundamentals(symbol) -> {symbol, profile?, period?, metrics?, growth_series, scores?, red_flags, dividends, missing}` — 分区独立降级
- `get_valuation(symbol) -> {symbol, multiples?, percentile?, dcf?, history_series, missing}` — DCF 恒为区间(bear/central/bull + 敏感性表)

## 深度分析引擎(astock-graph / astock-agent)
- `graph_subgraph(symbol_or_node, hops?: 1-3, 默认 2) -> {nodes, edges}` — 产业链子图(代码/节点 id/精确名称)
- `supply_chain_shock(subject, direction: "up"|"down"(支持涨/跌), magnitude_pct?) -> ShockJson` — 事件沿供应链传导:一二级受益/受损 + 逻辑链/滞后/置信度
- `relationship_graph(symbols: string[2-12], window_days?: 60-500, 默认 250) -> GraphJson` — 日收益 Pearson + lead-lag 关系网络
- `run_backtest(symbol?, strategy?, params?, pool?, fast?, slow?, entry_n?, exit_n?, bars?: 60-2000, 默认 750) -> BacktestJson` — 日线回测(A 股交易规则),返回绩效/净值曲线/最近 50 笔交易
  - 内置策略 `ma_cross`(默认)/ `turtle` / `buy_hold`:标量参数 fast/slow/entry_n/exit_n 直接传,或经 `params` JSON 对象传(显式标量优先);`symbol` 必填。
  - 注册表单标的策略(如 `zscore_mean_reversion`):`symbol` 必填,参数走 `params` JSON(如 `{"ma_window":20,"z_window":60,"entry_z":-2.0,"exit_z":1.0}`);未知参数键报 `invalid_param`。返回同 BacktestJson 形状并带 `kind: "single"`。
  - 注册表轮动策略(`min_corr_etf_rotation`):`pool: string[2-20]`(去重)必填,`symbol` 忽略;参数 `{"lookback":60,"hold_n":4}`。返回 `kind: "rotation"` + `pool`,绩效由净值曲线计算,`trades_tail` 含 `symbol` 字段。
- `list_strategies() -> [{name, kind: "single"|"rotation", description, params: [{name, ty: "int"|"number", default, description}]}]` — 回测策略注册表+参数元数据,UI 按此渲染策略表单;`run_backtest` 的 `strategy`/`params` 取值以此为准。
- `get_market_regime() -> {regime: 进攻|中性|防守, ...breadth/index 依据, source, fetched_at}`

## 东财数据中心(astock-market-data EmDataCenter)
统一返回 `{rows: [...], count, source, fetched_at}`;行字段与 `crates/market-data/src/providers/em_datacenter.rs` 行结构一致(日期 `YYYY-MM-DD`;金额元、比率 %;两融余额类字段单位亿元、以 `_yi` 结尾;解禁数量已由万股→股、市值万元→元)。所有命令带 60s 进程内缓存(按命令+参数为键),底层报表另有 600s TTL 缓存。
- `get_zt_pool(date?: "YYYY-MM-DD") -> ZtPoolRow[]` — 涨停股池,默认最近交易日;含连板数/封板时间/封单资金/炸板次数/涨停统计。
- `get_billboard(days?: 1-90, 默认 7) -> BillboardRow[]` — 龙虎榜详情,近 N 个自然日(按最近交易日向前取窗)。
- `get_margin_daily() -> MarginDailyRow[]` — 两融账户统计,最近约 1000 个交易日,按日期倒序。
- `get_org_survey(days?: 1-365, 默认 30) -> OrgSurveyRow[]` — 机构调研统计(含最新价/涨跌幅注入)。
- `get_holder_num(code?) -> HolderNumRow[]` — 股东户数最新披露;传 `code` 只返回该股(未披露则 rows 为空)。
- `get_earnings_predict(report_date?: "YYYY-MM-DD") -> EarningsPredictRow[]` — 业绩预告;`report_date` 为报告期,默认今天之前最近的季度末(03-31/06-30/09-30/12-31)。
- `get_lift_stage(start, end) -> LiftStageRow[]` — 限售解禁明细,解禁窗口 [start,end],最长 366 天。
- `get_suspensions(date?: "YYYY-MM-DD") -> SuspendRow[]` — 停复牌,默认最近交易日;当日无停复牌 rows 为空属正常。
- `get_notices(code, days?: 1-730, 默认 90) -> NoticeRow[]` — 个股公告(全部类型),最多最近 300 条,含详情页 `url`。
- `get_boards(kind: "industry"|"concept") -> BoardRow[]` — 行业/概念板块列表(含领涨股)。
- `get_board_cons(bk_code) -> BoardConsRow[]` — 板块成分股,`bk_code` 为 get_boards 返回的板块代码(`BK`+4 位数字)。

## 扫描
- `scan_start() -> {started: true}`;`scan_status() -> {running, done, total, current_symbol, results: [{symbol,name,score,action,confidence}]}`
- 进度事件:`scan-progress` {done,total,current_symbol};`scan-result` 单条结果

## 自选股(astock-storage)
- `watchlist_list() -> {group_name,code,name?,added_at,pinned}[]`
- `watchlist_add(code, group)`, `watchlist_remove(code, group)`, `watchlist_pin(code, group, pinned)`

## 设置与 MiniKey(M4 接 Agent,此处先有状态面板)
- `minimax_set_key(key: string) -> ServiceInfo`(存 Windows 凭据管理器,永不回显)
- `minimax_status() -> { has_key, region?, api_host?, model?, quota?: QuotaStatus }`
- `minimax_quota() -> QuotaStatus`

### 数据源凭证与代理(可选 provider)
- `settings_set_provider_credentials({tushare_token?, iwencai_key?, jq_user?, jq_pwd?, socks5?}) -> {status: ProviderStatus, message: string}`
  - 每个字段都是 `Option<string>`:**不传/传 null 或空串 = 清除该项**;非空 = 覆盖保存。
  - 保存到 storage kv 表(key 前缀 `provider.`),同时 `set_var`/`remove_var` 写入进程环境变量(`TUSHARE_TOKEN` / `IWENCAI_KEY` / `JQ_USER` / `JQ_PWD` / `ASTOCK_SOCKS5`),即时生效;返回的 `message` 提示"部分 provider 需重启后重新建连"(已构造的 provider 实例持有旧配置)。
  - 敏感项(tushare_token / iwencai_key / jq_pwd)在 kv 里用 **base64 包一层,仅防 shoulder-surfing,不是加密**;任何能读 meta.db 的人都能还原。凭证本体绝不写日志、绝不回传前端。
- `settings_get_provider_status() -> ProviderStatus`
  - `ProviderStatus = {tushare_token: bool, iwencai_key: bool, jq_user: bool, jq_pwd: bool, socks5: bool}`,只回报"是否已配置",绝不回传 token 本体。
- 启动时(`AppState::init`,构造 MarketData 之前)先从 kv 读出各项并注入进程环境变量,market-data / joinquant 现有的 env 读取路径零改动即可生效。

## 缓存维护
- `cache_stats() -> {kline_bytes, sqlite_bytes, tool_cache_bytes, chat_bytes, total_bytes, disk_free_bytes?}`
- `cache_cleanup(target_mb) -> {freed_bytes, removed_files}`
- `get_data_dir() -> string`,`set_data_dir(path)`

## 约定
- 所有耗时命令异步(tokio),不阻塞 UI;扫描等长任务发事件流。
- UI 轮询行情 2s;扫描轮询 scan_status 或订阅事件。
