# NiuOne (kunkundi/niuone) 项目调研报告

调研日期：2026-08-22。调研方式：浅克隆 GitHub 仓库至 `../research-tmp/niuone`(commit `441a547`,2026-08-21),通读 README、`docs/APP_ARCHITECTURE.md` 及 `app/` 全部关键模块源码(287 个 Python 文件)。项目为 Apache-2.0、Python 3.11+ 本地优先的 A 股/美股市场研究 + 自动化监控 + 模拟交易系统。本报告目标：吸收其数据源、算法逻辑、agent 模型与快速 AI 调用能力，**剔除飞书等 IM 接入**,评估向 astock-terminal(Rust 核心 + Tauri v2)的移植价值。

## 结论速览

- **数据源层价值最高且全部可纯 Rust 重写**:niuone 几乎不用重型库(requirements 只有 akshare/mini-racer/PyYAML/fastapi/uvicorn),核心行情全部走腾讯/东财/新浪裸 HTTP,与我们已接入的源高度重合，其字段解析、降级链、缓存 TTL 设计可直接借鉴。问财走的是**官方 OpenAPI(Bearer 认证，无验证码无 JS 挑战)**,与我们在 `data-source-ths-xueqiu.md` 中实测的网页版 iwencai(弹验证码、不可行)完全是两条路——这是本次调研最重要的数据源发现。
- **算法层最有价值的是"风控与退出"而非"入场形态"**:R 倍分段止盈/结构止损/动态风险定仓/市场状态机与硬停止，全部是确定性纯函数，适合搬进 `trading-rules`/`quant`/`backtest`。15 个内置策略(Z哥、板块潮汐、牛牛战法等)阈值迭代极快(协议已到 v40),建议只借鉴框架语义，不照搬阈值。
- **AI 层是"单端点多角色 + 严格 JSON + 本地复核",没有多 agent 框架**:三层 LLM 客户端(请求值对象 / reasoning 参数能力表 / 配置解析)、Chat+Responses 双协议容错降级链、"LLM 编译、本地执行"的文字策略 DSL(sha256 冻结 + 确定性回测)是最值得移植的设计,`minimax`/`agent` crate 直接受益。
- **飞书/钉钉/企微/Telegram 通知集中在 `app/messaging/`(约 1300 行，边界清晰),外部触点仅 4 处，整体剔除即可，不影响其余模块的吸收。**
- **建议优先级**:① 问财 OpenAPI 接入(填补龙虎榜/消息面)> ② 风控退出规则栈 + 回测撮合假设 > ③ LLM 客户端三层结构与 reasoning 能力表 > ④ 文字策略 DSL > ⑤ 市场状态机 > ⑥ 提示词模板 > ⑦ 其余策略形态按需摘取。

---

## 1. 项目架构与核心能力

### 1.1 运行时拓扑

单仓多进程，依赖方向：入口/组合层 → 领域包(dashboard、market_data、screening、trading、strategies、reports、storage)→ 标准库与外部行情源。`app/entrypoints/` 全是薄启动器。

| 进程 | 职责 |
|---|---|
| Dashboard(FastAPI 单端口 8787) | 唯一 HTTP 服务，内嵌全部后台线程：b1 选股/决策槽位调度、盘前 K 线预热、市场宽度 30s 采样、行业资金流采样、公开投影 15s 刷新；挂载 Vue3 静态产物 |
| Cron Scheduler(10s 轮询自实现 cron) | 按 cron 表达式用子进程拉起任务脚本：08:00 美股总结、09:25 竞价总结、09:37/14:45 模拟盘自动离场、11:40 午盘、15:10 收盘总结、15:15 净值快照、15:20 前向评估、18:00 问财龙虎榜快照、06:00 美股评级 |
| Practice Trader(非常驻) | 由 b1 槽位调度或 cron 触发：候选扫描 → 本地退出检查 → LLM 决策 → 硬复核 → 确定性模拟成交落盘 |
| Minute Refresh(隔离子进程，180s 硬超时) | CPU 密集的题材强度分钟级重算，只写缓存，主进程始终可返回上一份有效读模型 |
| Backtest Worker(独立低优先级子进程) | 离线回测，结果原子写盘，与模拟账户完全隔离 |
| NewsNow sidecar(Docker 容器) | 财经快讯聚合(财联社/金十/华尔街见闻等 60+ 源白名单),60s 刷新 |

存储：SQLite(WAL)+ 原子 JSON 文件。模拟盘库 `niuniu.db`(trades/decisions/daily_equity/position_snapshots,只追加不可变版本历史),报告库、全市场腾讯日 K 缓存库各一。

**对我们的启示**:与我们 Tauri 单进程架构不同，niuone 用"主进程 + 短寿命隔离子进程 + 原子 JSON 读模型"保证 UI 永不阻塞。Rust 侧对应物是 tokio task + `tokio::process` + 原子写文件，其"重计算写缓存、读路径只读上一份有效产物"的模式值得借鉴。

### 1.2 定时任务体系(可借鉴的交易时段编排)

选股与决策不在 cron，而由 Dashboard 内置槽位调度按 `09:25,10:00,10:30,11:00,11:20,13:00,13:30,14:00,14:30,14:50` 驱动：先全市场扫描(超时 480s),再决策交易。槽位终态落盘，重复执行幂等。退出检查固定在 09:37(开盘)与 14:45(尾盘)各跑一次，盘中每个决策槽也先跑退出检查。

---

## 2. 数据源清单(接口、字段、反爬、Rust 可行性)

### 2.1 总览

| 源 | 用途 | 认证/反爬 | Rust 可行性 |
|---|---|---|---|
| 腾讯 `qt.gtimg.cn` | 全市场实时快照(市场宽度)、个股批量报价(模拟交易主通道) | 无，GBK 文本 | ✅ 极易 |
| 腾讯 `ifzq.gtimg.cn` | 日 K(qfq)/1 分钟 K/分时 | 无，JSON | ✅ 极易 |
| 东财 `push2*/clist/get` | 行业/概念归属、板块排行、行业主力净额、竞价快照 | 无(JS token `ut=` 硬编码公开值);**对非常规 TLS 客户端断连，Python 侧靠 curl 子进程绕过** | ⚠️ 需实测 reqwest 指纹，必要时 `std::process::Command` 调 curl 保底 |
| 东财 `push2his/kline/get` | 指数/个股历史 K(回测主源) | 同上 | ⚠️ 同上 |
| 新浪 `hq.sinajs.cn` | A 股第三级兜底报价、期货/黄金/美股(`gb_`/`hf_` 前缀) | **必须 `Referer: finance.sina.com.cn`**,GBK | ✅ 易 |
| **问财官方 OpenAPI `openapi.iwencai.com`** | 龙虎榜、消息面(公告/新闻/事件)、板块归属兜底 | `Authorization: Bearer {API_KEY}`,**无验证码无 JS 挑战** | ✅ 极易(纯 JSON POST) |
| FMP `financialmodelingprep.com/stable` | 美股机构评级、目标价、批量报价 | API Key 放 `apikey` **请求头**(不进 URL) | ✅ 极易 |
| Yahoo `query1.finance.yahoo.com/v8/chart` | 美股指数分钟线、ETF 日行情 | 无 | ✅ 易 |
| NASDAQ `api.nasdaq.com` | 美股公司行业归属 | 无(带 Origin/Referer) | ✅ 易 |
| NewsNow `/api/s` | 财经快讯聚合(60+ 源) | 公共端点有 Cloudflare 但仅按 UA 过滤 | ✅ 易；可自建 sidecar 或用公共实例 |
| akshare(库) | 涨停/跌停池、融资融券、大宗交易、交易日历、代码池、同花顺行业资金流 | 同花顺资金流内部要 py_mini_racer 跑 JS 解密 | ⚠️ 逐项重写工作量大；其中资金流 niuone 自己已用东财 clist 直替，可照此路线 |

### 2.2 问财 OpenAPI(本次最重要发现)

我们在 `data-source-ths-xueqiu.md` 中实测网页版 iwencai **纯 HTTP 不可行(直接弹图形验证码)**。niuone 证明存在第二条路：**同花顺官方 SkillHub 网关**,与网页版反爬体系完全无关。

- 客户端:`app/market_data/iwencai_client.py`。端点:
  - `POST /v1/query2data` — 自然语言查询，默认 skill `hithink-market-query`;payload `{query, page, limit, is_cache, expand_index}`,响应取 `datas` + `code_count`。
  - `POST /v1/comprehensive/search` — skill `announcement-search` / `news-search`。
  - `query2data` 带 `skill_id="hithink-event-query"` 查结构化事件。
- 使用场景:
  1. **龙虎榜**(`app/dashboard/apis/iwencai_service.py:1370`):三条查询——主榜(上榜原因/买卖额/净买入/连板天数/涨停原因)+ 行业归属 + 营业部席位(最多 10 页),按交易日归档，席位数据跨刷新保留，连续上榜自动标记。
  2. **消息面预检**(`app/market_data/news_precheck.py`):每只候选并发调公告/新闻/事件 3 个 skill，证据截断后交 LLM 判利好/利空，缓存 300s。
  3. **板块归属兜底**:全量分页查询作为东财板块快照的备用源。
- 治理：默认关闭(`IWENCAI_ENABLED=0`)、超时 20s、仅 429/5xx 重试 1 次、并发上限 2(信号量)、响应 5MB 上限、指数退避。
- **前提：需要用户申请 API Key**。适合做成我们 `wencai` crate 的可选 provider(设置页填 key),与聚宽"可选 provider"路线一致。

### 2.3 关键字段映射(niuone 已踩平的坑)

- 腾讯 `~` 分隔文本(`parse_tencent_quote_body`):f[3]=现价、f[4]=昨收、f[6]=量、f[30]=时间戳、f[32]=涨跌幅、f[37]=成交额(**万**)、f[47/48]=涨/跌停价。K 线行 `[date, open, close, high, low, volume]`(注意 close 在第 3 位)。
- 东财 clist 资金流:`f62`→主力净额(÷1e8 得亿元)、`f184`→净占比、`f66/72/78/84`→超大/大/中/小单净额、`f204/205`→领涨股；板块字段 `f100`=行业、`f102`=地区、`f103`=概念、`f104/105/106`=涨/跌/平家数、`f128/140/141`=领涨股。
- 市场宽度：枚举约 9000 个 sh/sz 代码批量打腾讯快照，本地算红绿平/涨停/跌停/炸板/全市场成交额，**≥5000 只才认定有效**,否则报错不污染读模型。
- 全天成交额估算：东财指数分钟线近 20 日 5 分钟分布模型 + 开盘 5 分钟用竞价额平方根收缩模型——日频终端可简化为"分钟分布比例外推"。

### 2.4 降级链与缓存(值得整套借鉴)

- 个股报价三级链：腾讯 → 东财(curl)→ 新浪。回测历史 K:东财 → 腾讯 → 新浪。美股分时：腾讯 → 东财 → Yahoo → 新浪，四级。
- 缓存模式:`TTL 内读 JSON → 重算并原子写 → 失败回退 stale(标记 stale_cache=true)→ 否则 empty+error`,进程内 stale-while-revalidate 单飞刷新、按 key 锁。
- TTL 参考：板块快照 6h、概念板块 60s(stale 10min)、行业资金流 60s、热股 75s、指数 45s、NewsNow 60s、龙虎榜按交易日永久归档。
- 文件缓存键:`(dev, ino, size, mtime_ns)` 四元组 + LRU + 路径级锁，零重复解析——Rust 对应 `std::fs::Metadata` 的 `modified()+len()` 即可近似。
- 盘前 K 线预热：全市场 120 根前复权日 K 入 SQLite(WAL),`prewarm_runs` 跟踪覆盖率 ≥90% 才算 ready，失败记 attempts 不覆盖有效历史。

---

## 3. 策略与算法清单

### 3.1 策略体系(15 个策略、6 个互斥套件)

注册表 `app/strategies/registry.py`,一次只激活一个套件；历史持仓按 `strategy_mark` 继续走原策略退出。全部技术指标**纯 Python 自实现，无 pandas/numpy/talib**。

| 套件 | 策略 | 一句话逻辑 |
|---|---|---|
| 基础 | 突破确认 / 趋势回踩 | 30 日平台放量突破回踩不破;BBI 三日向上 + 缩量回踩 BBI |
| Z哥 | 少妇B1 / B2确认 / B3中继 / 超级B1 | KDJ-J 极值 + N 型低点抬高 + 缩量企稳的左侧抄底序列 |
| 李大霄 | 底部蓝筹 | 120 日回撤 -12%~-45% 底部区 + 高流动性蓝筹代理 |
| 板块潮汐 | 主线领航 / 轮动初升 / 冰点修复 | 市场状态机 × 行业潮位分位 × 个股行业内分位 |
| 牛牛战法 | 领涨 / 转强 / 启动 / 试仓 | 题材生命周期(酝酿→主升→高潮→分歧→退幕)× 龙头梯队 × V 型检测 |
| 文字策略 | preset_text | 自然语言→LLM 一次性编译为 JSON DSL→本地确定性执行(见 §4.4) |

**规则 vs LLM 划分**:14 个 scorer、市场状态机、行业/题材聚合、风险预算、卖出规则栈全部确定性;LLM 只负责(a) 内置套件下从 top-10 候选里输出 BUY/SELL/HOLD+股数(执行层硬复核),(b) 文字策略的编译期翻译。

### 3.2 退出/风控规则栈(全场最有价值的算法资产)

通用栈(按优先级):≥8% 减半、≥12% 清仓；峰值回撤止盈(盈利≥5% 激活，回撤≥max(3%, min(6.5%, 峰值×0.45)));ATR 吊灯(最高价-3×ATR20);移动止损保本;Donchian 10 日低点;时间效率退出(12 日亏>3%、25 日强平);日内权益 -3% 暂停新开仓。

R 倍体系(潮汐/牛牛):入场时冻结结构止损(突破位-0.5ATR 或近 4 日结构低点);首段止盈 = 成本+R×风险(1R 减 45%/试仓 0.75R 减 50%);首段后止损抬到成本线，余仓 峰值-2×ATR14 跟踪；各策略独立时间窗退出(B3 次日不涨走、主线 5 日未创新高、试仓 3 日未延续等)。

动态风险定仓(`practice_trader.py:2238 dynamic_risk_order_ceiling`):单票上限/总仓/主题敞口/单笔风险/组合风险/主题风险/现金储备 **七项约束取最小整手**;有效损失距离 = 结构止损 + max(近 60 日向下跳空 P95, 0.5×ATR) + 0.20% 执行缓冲。**这是全套系统里最工程化、最与市场观点无关的部分。**

市场状态机:广度 30% + 中位涨幅 20% + 涨跌停结构 15% + 指数 20% + 20 日趋势 15%,≥65 且广度≥55 = offensive,<40 = defensive，另有 recovery/rotation;硬停止 = 指数破位 + 广度破位 + 跌停扩散同时成立，进入/退出各需 2 次确认。

### 3.3 选股流水线

代码池(主板/创业板/科创板/ST 可选)→ 腾讯批量快照 → 流动性预筛(成交额≥8 亿取前 500,牛牛战法不预筛)→ 本地 SQLite K 线(盘前预热)→ 上下文构建(板块潮汐/牛牛题材 + 资金流/龙虎榜/隔夜美股/消息面限幅加分)→ 逐股多策略评分(`decision_score = score + priority/100 - 1.5×blocker数`,取"可执行优先、分数次之"的最优策略)→ 前 10 送决策层、前 16 展示(每策略族保底 1 席)。

### 3.4 回测与评估

- 撮合：收盘出信号 → **次日开盘价 + 5bp 滑点**;开盘涨停拒单;T+1 按买入批次独立执行;100 股整手;费用 = 万一免五佣金 + 卖出 0.05% 印花税 + 双向 0.001% 过户费。日 K 无法还原盘中顺序：止损用最低价、目标用最高价，**同根 K 线同触时保守地先执行止损**。
- 三种模式：固定持有期事件研究 / 交易生命周期回放 / 组合级资金回放。指标：胜率、盈亏比、利润因子、最大回撤(带峰谷日期)、年化、夏普、索提诺、卡玛、敞口、换手。
- 严格前向评估(`evaluate_niuone_forward.py`):从零持仓基线重建持仓生命周期，统计门含 **Wilson 95% 胜率下界 > 50%**、按入场日期×题材聚类的簇等权胜率、组合门(回撤≤6%、收益/回撤≥1);协议版本化(当前 v40),规则变更 bump 版本、旧证据失效——**防止规则漂移污染样本**的设计值得借鉴。

### 3.5 技术指标实现要点(移植时注意与 TA-Lib 的差异)

- BBI = (MA3+MA6+MA12+MA24)/4;KDJ(9,3,3)用中式 SMA 递推、初始 K=D=50;**ATR14 用 TR 简单均值而非 Wilder 平滑**;Z 白线 = EMA10 的 EMA10,Z 黄线 = (MA14+MA28+MA57+MA114)/4。
- N 型摆动低点检测(`niuone.py:242` 附近)与缠论分型/笔识别最接近，可考虑并入 `chanlun` crate 复用摆动点检测。

---

## 4. AI agent 组织与提示词方案

### 4.1 LLM 客户端三层结构(最值得移植的架构)

| 层 | niuone 位置 | 职责 |
|---|---|---|
| 传输/协议层 | `app/core/model_api.py` | Chat Completions + Responses 双协议构造/解析;`build_model_request()` 返回不可变 `ModelRequest{endpoint, payload, api_mode}` 值对象，鉴权与发送分离，HTTP 层可注入 mock |
| 推理参数适配层 | `app/core/model_reasoning.py` | 20 条 `ReasoningEffortCapability` 能力表：模型正则 + accepted_efforts + 兼容值映射 + 参数形态(qwen `enable_thinking` 布尔、GLM `thinking.type`、MiniMax-M3 `thinking.type=adaptive`、默认 `reasoning_effort`);已知模型配非法值直接报错，未知别名透传 |
| 配置解析层 | `app/core/shared_model_config.py` | 单一共享端点键族 `DASHBOARD_DECISION_{MODEL,BASE_URL,API_KEY,STREAM_MODE,REASONING_EFFORT,...}` + 旧键显式迁移函数 |

容错降级链(踩坑后的兼容性资产):非流式被 400/409/422 拒且错误体匹配"必须流式"正则 → 自动改 SSE 重试;Responses 的 `max_output_tokens` 被拒 → 剥参重试一次;`finish_reason=length` → 自动翻倍 max_tokens;瞬态错误(408/429/5xx/超时/SSL)才重试，单请求超时与总 deadline 分层(`min(请求超时, deadline剩余-2)`)。

**"快速 AI 调用"没有独立轻量模型路由**,快在:① 决策情报 75s TTL 缓存(一个决策周期内不重复抓数据);② 版本化文件缓存零重复解析;③ 消息预检结果 300s 缓存;④ 连通性快探针(256 max_tokens、超时夹紧 5~30s);⑤ JSON 解析失败才重试并加大 max_tokens。模型调用刻意低并发(上限 1~2),并发只用于数据证据抓取。

### 4.2 Agent 组织:单端点、多角色提示词

无 agent 编排框架、无工具调用循环。每个角色 = 一个 system prompt 构造函数 + 专用 JSON 解析器 + 失败兜底:

| 角色 | 输入 | 输出 |
|---|---|---|
| 盘面监控分析师(竞价/午盘/盘后) | 本地规则生成的盘面快照全文 | tone + summary + guidance/focus/risk |
| 隔夜美股分析师 | 指数/期货/黄金/原油 + ETF→A股映射 | 同上 schema |
| 日内复盘助手 | 多份资料(美股总结/当日总结/实时快照，单份截断 ≤18000 字、总预算 60000) | 对比复盘 JSON，禁止操作建议 |
| 消息面判断器 | 问财抓取的公告/新闻证据(≤8 条，压缩 JSON) | tone_label/event/impact/reason |
| 文字策略编译助手 | 本地特征注册表全量 + 用户原始文字 | strategy_spec JSON(冻结) |
| 交易决策器 | 纪律文本 + 策略规则 + 盘面 + 账户 + 情报包 + 候选单行摘要 | actions[BUY/SELL/HOLD, shares, intent] |
| 二次风控审稿人 | 超限 BUY 的原始决策 + 风险摘要 | keep/drop 清单 |

两个值得借鉴的组装模式:**所有输入先由本地规则预处理成"中性事实"**(截断、压缩、单行摘要),提示词反复强调"不得编造输入以外的数据";**提示词按当前持仓的 strategy_mark 动态拼装策略纪律段**,不是静态模板。

### 4.3 提示词方案

统一模式:system 定义角色 + 禁令(不编造、严格 JSON、无 Markdown),user 给数据 + schema 示例 + 条数/风格约束;五档情绪枚举 `offensive|balanced|neutral|cautious|defensive`;A 股术语(竞价承接、回封、右侧确认、炸板、T+1)。代表性骨架:

```
你是牛牛1号的A股盘面监控策略分析师。你会收到一份由本地规则生成的A股盘面快照…
不要编造未给出的新闻、政策、公司事件、资金数据或实时行情;如果数据不足,必须明确保守处理。
必须输出严格JSON,不要Markdown,不要代码块,不要URL。
```

JSON 输出落地手段：剥 ```json fence + 正则找首个 `{` + `raw_decode` 容错;解析后逐字段白名单清洗 + 截断 + 枚举回退。**Rust 侧用 serde 强类型 + `#[serde(default)]` + schemars 生成 schema 示例进提示词，比 Python 的 dict 清洗更自然**(我们 workspace 已有 schemars)。

### 4.4 "LLM 编译、本地执行"的文字策略 DSL(全场最有价值的 AI 设计)

1. **编译(创建时一次 LLM)**:用户自然语言 → 受限 JSON DSL(selection/entry/exit 三阶段规则树),只能用本地特征注册表(带版本号:EMA v1/v2、cn-kdj-v1/v2、RSI Wilder、MACD、BOLL、ATR、量比、区间高低点等，每个特征声明 min_bars 与 offset_bars 0–499 历史偏移);禁止生成代码/公式字符串/eval;遗漏条件确定性补全;冻结并算 `plan_sha256`。
2. **执行(运行期零 LLM)**:本地三值(true/false/unknown)规则引擎按规则树反推所需特征与历史深度，物化事实快照，产出 action_intent + 可审计 audit;失败即关闭。
3. **回测**:先验 sha256 指纹 + 引擎版本 + 数据契约(仅 1d/closed K 线),同一冻结计划确定性因果回放，完全可复现。

计划指纹贯穿信号/决策/审计元数据——这套"自然语言 → 受限 DSL → 指纹冻结 → 确定性执行/回测"的闭环是我们 `agent` + `quant` + `backtest` crate 做"用户自定义策略"功能的理想蓝本。

---

## 5. 逐项移植评估

| niuone 能力 | 移植价值 | 移植方式 | 目标 crate |
|---|---|---|---|
| 问财 OpenAPI 客户端(龙虎榜/消息面/板块兜底) | ★★★★★ | **重写为 Rust**(reqwest JSON POST,Bearer);做成可选 provider,用户填 key | `wencai` / `market-data` |
| R 倍退出体系 + 通用卖出规则栈 | ★★★★★ | **借鉴算法**(纯函数照搬语义，阈值做成配置);以 `tests/test_sell_strategy_rules.py` 断言为参考测试向量 | `trading-rules` |
| 动态风险定仓(七约束取最小整手、有效损失距离) | ★★★★★ | **借鉴算法**,`rust_decimal` 复刻 | `quant` / `trading-rules` |
| 回测撮合假设(次日开盘+滑点、涨停拒单、同 K 止损优先、费用模型、T+1 批次) | ★★★★★ | **借鉴算法**;与我们已有回测假设对齐并补缺 | `backtest` |
| LLM 客户端三层结构 + 双协议降级链 + reasoning 能力表 | ★★★★★ | **重写为 Rust**(reqwest + eventsource);MiniMax M2/M3 `thinking.type`/`reasoning_split` 特殊处理直接进 minimax crate | `minimax` / `agent` |
| 文字策略 DSL(特征注册表 + 指纹冻结 + 确定性回测) | ★★★★☆ | **重写为 Rust**(serde/schemars 定义 DSL,参考其 capability catalog schema);工程量不小，建议二期 | `agent` + `quant` + `backtest` |
| 市场状态机 + 硬停止(含对称恢复确认) | ★★★★☆ | **借鉴算法** | `quant` |
| 提示词模板体系(system 禁令 / 数据压缩 / 五档情绪 / 按持仓动态拼装) | ★★★★☆ | **借鉴提示词**,改写为我们终端的分析场景(盘面总结、个股诊断、消息面判断) | `agent` |
| 腾讯/东财/新浪字段解析与三级降级链 | ★★★★☆ | **借鉴算法**(我们已有接入,对照补字段映射与降级顺序) | `market-data` |
| 缓存与 TTL 体系(stale 回退、文件版本键、盘前预热覆盖率门) | ★★★★☆ | **借鉴算法** | `market-data` / `storage` |
| 市场宽度采样(腾讯全市场快照 ≥5000 只校验)与成交额外推 | ★★★☆☆ | **借鉴算法**;日频终端可降级为收盘后一次性计算 | `market-data` / `quant` |
| 技术指标集(BBI、中式 KDJ、ATR-SMA、Z 双线、N 型摆动点) | ★★★☆☆ | **重写为 Rust**(均为 20 行内纯函数);注意与 TA-Lib 差异,带参考测试向量;N 型摆动点考虑并入缠论 | `technical` / `chanlun` |
| 板块潮汐/牛牛横截面聚合(强势分、有效强股数、题材归因) | ★★★☆☆ | **借鉴算法**,简化后移植 | `quant` |
| Z哥形态识别(防卖飞评分、S1/S2/S3、出货五式、卤煮) | ★★☆☆☆ | **借鉴算法**,按需摘取;规则琐碎但确定性强 | `trading-rules` |
| 15 个内置策略的入场 scorer 与阈值 | ★★☆☆☆ | **不照搬**:协议迭代极快(v40),阈值取自主线分数分布尾部、非交易结果搜索;只借框架语义 | — |
| 严格前向评估协议(Wilson 下界、聚类簇胜率、版本化证据) | ★★★☆☆ | **借鉴算法**,作为策略上线门禁思路 | `backtest` |
| NewsNow 快讯聚合接入 | ★★★☆☆ | **重写为 Rust**(纯 JSON GET);决策情报包模式(只进 LLM 上下文、不改风控)可参考 | `market-data` |
| FMP/NASDAQ/Yahoo 美股数据 | ★★☆☆☆ | 按需重写;我们 A 股优先 | `market-data` |
| 飞书/钉钉/企微/Telegram 通知(`app/messaging/`) | ✕ | **不移植**,整体剔除;边界清晰(仅 4 处外部触点),剔除无连带影响 | — |
| 多进程服务编排、FastAPI dashboard、Vue 前端、compat 兼容层 | ✕ | **不移植**:与我们 Tauri 架构不同;仅"重计算隔离 + 原子读模型"模式可借鉴 | — |
| akshare 依赖项(涨停池/两融/大宗/交易日历) | ★★☆☆☆ | **重写为 Rust**:涨停池可直接打东财 push2ex;同花顺资金流已被 niuone 用东财 clist 直替，照此路线 | `market-data` |

### 明确不搬的 Python 反模式

- `grok_service.py` 关闭 SSL 校验(CERT_NONE);`load_crossdesk_provider` YAML 私有 provider 兜底与硬编码个人 provider 名;`app/compat` 双导入兼容层;`practice_trader.py` 1.2 万行上帝文件(移植时按 `app/core` 分层拆，不照抄其组织方式)。

---

## 6. 优先级排序(按价值/成本比)

1. **P0 问财 OpenAPI provider**(`wencai` crate):纯 JSON POST + Bearer，无反爬，一举补齐龙虎榜、消息面证据、板块归属兜底三块我们目前空白的能力;niuone 的查询语句、分页上限、并发信号量、300s 缓存可直接照抄。成本约 1~2 天。
2. **P0 风控退出规则栈 + 回测撮合对齐**(`trading-rules`/`backtest`):R 倍分段止盈、结构止损冻结、峰值-2ATR 跟踪、时间窗退出、七约束定仓、同 K 止损优先——与我们"实时买卖点"定位最契合的确定性资产，全部纯函数，带现成参考测试。
3. **P1 LLM 客户端升级**(`minimax`/`agent`):三层结构、`ModelRequest` 值对象、reasoning 能力表(尤其 MiniMax 特判)、Chat/Responses 双协议降级链、deadline 预算重试。直接提升现有 minimax crate 的鲁棒性。
4. **P1 提示词体系**(`agent`):盘面总结/消息面判断两个角色的提示词骨架改写落地，数据"中性事实"压缩模式，serde+schemars 强约束 JSON 输出。
5. **P2 文字策略 DSL**(二期，工程量大):特征注册表版本化 + sha256 冻结 + 确定性回测闭环，是"用户自然语言自定义策略"的最稳妥实现路径。
6. **P2 市场状态机 + 横截面题材聚合**(`quant`):作为买卖点的市场环境过滤器。
7. **P3 其余**:技术指标补全、Z哥形态、NewsNow 快讯、美股数据源，按需摘取。

## 附:Rust 实现要点

- HTTP 层全部走 workspace 现有 `reqwest(rustls)`;腾讯/新浪 GBK 文本用 `encoding_rs`(gb18030)解码——这是唯一需要新增的轻量依赖候选。
- 东财 clist 若遇 TLS 指纹断连，先实测 `push2delay` 主机;不行再 `std::process::Command` 调 curl 保底(niuone 同款方案)。
- 金额/价格计算用 `rust_decimal`(niuone 的 A 股涨跌停价按板块幅度两位小数取整的规则需要十进制精确);workspace 当前未引入，实现撮合/定仓时评估加入。
- 所有 niuone 阈值(8%/12% 止盈、-3% 日亏损预算、0.75R/1R 分段等)移植时做成配置项而非硬编码——该项目自身迭代史(v25 曾放大风险预算 1.35 倍又取消)证明阈值不稳定。
- 参考测试：以 `tests/test_niuone_strategy.py`、`tests/test_sell_strategy_rules.py` 的断言语义为基准写 Rust 单元测试，指标实现(BBI/KDJ/ATR)用固定输入向量对拍。
