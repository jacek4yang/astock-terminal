# Tushare Pro 数据源调研报告

调研日期：2026-08-22（本地时间）。调研方式：通读 tushare.pro 官方文档（[HTTP 调取说明](https://tushare.pro/document/1?doc_id=130)、[积分与频次权限对应表](https://tushare.pro/document/1?doc_id=290)、[关于权限](https://tushare.pro/document/1?doc_id=108) 及各接口文档页）+ [PyPI tushare](https://pypi.org/project/tushare/) + 第三方非 Python SDK（[go-tushare](https://pkg.go.dev/github.com/fletcherlau/go-tushare)、[tushare-sdk (TS)](https://github.com/hestudy/tushare-sdk)）交叉验证。**未实测**（调研环境无 tushare token），文中标注"官方文档"的为文档口径，标注"第三方"的为 SDK/社区口径。

## 结论速览

- **纯 Rust 直连完全可行，且是官方支持的方式**：tushare pro 就是一个与语言无关的 HTTP JSON API——`POST https://api.tushare.pro`，body 为 `{api_name, token, params, fields}`，响应 `{code, msg, data:{fields, items}}`。Python SDK 只是这个 HTTP 接口的薄封装（官方文档原文）。无 cookie、无 JS 挑战、无签名计算，`reqwest + serde_json` 即可，接入成本是所有调研过的源里最低的。
- **不是免费匿名服务，但免费档真实可用**：注册（100 分）+ 完善资料（20 分）= 120 积分，0 元，可永久调用**非复权日线 `daily`**（50 次/分钟、8000 次/天）。这恰好匹配我们 data-foundation-v2 "只存 raw" 的模型。
- **关键短板：免费 120 档拿不到 `adj_factor`、`daily_basic`、财务、分红送股**——这些全部 **2000 积分起**（200 元/年）。所以 tushare 不能替代东财/腾讯/雪球免费源，只能作为**用户自带 token 的可选 provider**（与聚宽同模式，joinquant 调研文档 §4 已预留该模式）。
- **对我们最有价值的两点**：① 120 档的 `daily` 是权威 raw 日线备份/交叉验证源（vol 单位是**手**，与我们 `VolumeUnit::Lots` 天然一致）；② 2000 档的 `adj_factor` 是现成的**厂商复权因子金标**，可逐日对拍 `adjust.rs` 自算因子，比"与腾讯 qfq 对拍"更直接。
- **建议**：做成可选 provider，设置页填 token；无 token 整体禁用。120 档用途 = raw 日线备份源；2000+ 档追加 adj_factor 金标校验 + daily_basic 估值快照 + dividend 公司行为备份源。优先级中等（现有免费源已覆盖主链路）。

---

## 1. HTTP API 协议（官方文档口径）

### 1.1 请求/响应格式

- 端点：`POST https://api.tushare.pro`（官方示例用 `http://`，https 同路径可用，第三方 SDK 均走 https；Rust 侧建议一律 https）。
- 请求头：`Content-Type: application/json`，无需 cookie/UA/Referer/签名。
- 请求体（逐字）：

```json
{
  "api_name": "daily",
  "token": "用户token",
  "params": {"ts_code": "000001.SZ", "start_date": "20180701", "end_date": "20180718"},
  "fields": "ts_code,trade_date,open,close"
}
```

- `api_name` 接口名；`token` 用户唯一标识（官网注册 → 个人主页"接口 TOKEN"复制）；`params` 接口参数 dict；`fields` 逗号分隔字段过滤（可省，省略返回默认字段）。
- 响应体：

```json
{
  "code": 0,
  "msg": null,
  "data": {
    "fields": ["ts_code", "trade_date", "open", "close"],
    "items": [["000001.SZ", "20180718", 8.75, 8.70], ...]
  }
}
```

- `data.items` 是**行数组**（无列名），必须按 `data.fields` 的顺序对齐取值——Rust 侧不要写死列序，先读 `fields` 建索引再映射（tushare 历史上调整过默认字段顺序）。
- 官方文档只承诺 `code/msg/data` 三个顶层字段；实践中响应还带 `request_id`，部分接口带 `data.has_more`（配合 `limit/offset` 翻页，第三方 SDK 的自动分页基于此）。**未逐一验证，接入时以实测为准。**

### 1.2 错误语义

- `code = 0` 成功；非 0 失败，`msg` 为中文错误文本（Python SDK 的行为就是 `code != 0` 直接抛 `msg`）。
- 官方文档明示：`2002` = 权限问题（积分不足，msg 形如"抱歉，您没有接口访问权限，权限的具体详情访问：…doc_id=108"）。
- `40203` = 频率超限（第三方 go-tushare SDK 口径，其重试策略把 40203 与超时/连接错误列为可重试，其余业务错误立即失败）。
- token 无效/错误也是 `code != 0` + msg 文本（具体错误码官方未单列，按通用失败处理即可）。
- **对我们的含义**：重试策略 = 仅对网络错误/超时/40203 做指数退避（沿用现有自适应限速）；2002 不重试，直接标记"token 积分不足"进数据源健康面板。

### 1.3 积分/限流规则（官方 doc_id=290 + doc_id=108，2026-08 口径）

积分是**门槛制，不消耗**：达到档位即可调，积分越多频次越高。

| 积分 | 价格 | 每分钟频次 | 每天总量 | 可调数据 |
|---|---|---|---|---|
| 120 | 0 元（注册 100 + 完善资料 20） | 50 | 8000 次 | **仅非复权日线 `daily`**（另有 new_share、少数宏观利率接口为 120） |
| 2000+ | 200 元/年 | 200 | 100000 次/个 API | 常规接口（日线/周月线/复权因子/每日指标/财务/分红/龙虎榜等，见各接口文档） |
| 5000+ | 500 元/年 | 500 | 常规数据无上限 | 同上 + 更高频次 |
| 10000+/15000+ | 1000/1500 元/年 | 500 | 特色数据 | 盈利预测、筹码、券商金股等特色数据 |

**独立付费（不在积分体系内）**：历史分钟线 2000 元/年、实时分钟 1000 元/月、实时日线 200 元/月、港美股/新闻/公告等各自单独开通。**分钟数据对我们不可白嫖，明确不接**（与现有腾讯/东财分钟源定位不冲突）。

与本终端相关接口的最低积分（官方 doc_id=108 权限表 + 各接口文档页）：

| 接口 | 内容 | 最低积分 | 备注 |
|---|---|---|---|
| `daily` | A 股日线（未复权） | **120** | 15:00~17:00 入库，停牌期间无数据 |
| `stock_basic` | 股票列表/基础信息 | 2000 | 官方接口页现口径；历史上为基础积分可调，**门槛上调过**，120 档实测可能不可用 |
| `adj_factor` | 复权因子 | 2000 | 盘前 9:15~9:20 完成当日入库 |
| `daily_basic` | 每日指标（换手/估值/股本/市值） | 2000 | 单次最大 6000 行 |
| `weekly`/`monthly` | 周/月线 | 2000 | 不需要，我们由日线聚合 |
| `pro_bar` | 复权行情 | 2000 | 不需要，复权由 `adjust.rs` 运行时算 |
| `income`/`balancesheet`/`cashflow` | 财务三大表 | 2000 起 | 随财报实时更新 |
| `fina_indicator`/`forecast`/`express` | 财务指标/预告/快报 | 2000 起 | |
| `dividend` | 分红送股 | 2000 起 | 含除权除息日，见 §3.4 |
| `trade_cal` | 交易日历 | 2000 | 我们有替代源，非必需 |
| `index_daily`/`index_basic` | 指数日线/列表 | 2000 | 指数行情现有免费源已覆盖 |
| `stk_limit`/`moneyflow`/`top_list` 等 | 涨跌停/资金流/龙虎榜 | 2000 起 | 东财 datacenter 已覆盖 |

### 1.4 额度估算（120 免费档是否够用）

- `daily` 单次最大 6000 行 ≈ 全市场一天（~5400 只）一次请求拉完。盘后全市场增量 = **每天 1~2 次请求**，离 8000 次/天上限极远。
- 全市场历史回填：按 `trade_date` 逐日循环（官方建议用法），23 年 ≈ 5700 个交易日 = 5700 次请求 < 8000 次/天；50 次/分钟限速下约 2 小时跑完。**120 档做"日线增量 + 一次性回填"完全够。**
- 按 `ts_code` 逐股拉历史是最低效用法（官方文档明确建议"循环日期、不要循环 ts_code"），Rust 侧按 trade_date 批量落库。

---

## 2. 免费/低积分档能拿到什么（对照我们的需求）

| 我们的需求 | tushare 答案 | 结论 |
|---|---|---|
| raw 日线（data-foundation-v2 主存储） | `daily`，120 档免费，未复权、全历史 | ✅ 免费档核心价值 |
| 复权因子/公司行为 | `adj_factor` 2000 档；`dividend` 2000 档 | ❌ 免费档拿不到 → 公司行为仍靠东财 RPT_SHAREBONUS_DET（现状不变） |
| 每日指标（换手/PE/PB/市值） | `daily_basic` 2000 档 | ❌ 免费档无 → 继续用雪球 quote/东财 |
| 财务三大表/指标 | `income` 等 2000 档 | ❌ 免费档无 → 继续用雪球/东财 F10 |
| 分钟线 | 独立付费 2000 元/年 | ❌ 明确不接 |
| 实时行情 | 实时日线 200 元/月（独立权限） | ❌ 不接，实时继续用腾讯/新浪/雪球 |

一句话：**免费档 = 权威 raw 日线备份源；2000 档 = 全套基本面 + 复权金标，但那是用户自己的付费选择。**

---

## 3. 字段映射（对照 data-foundation-v2 的 raw + adj_factor 模型）

### 3.1 `daily` → 我们的 raw Bar（120 档）

官方输出字段（[daily 文档](https://tushare.pro/document/2?doc_id=27)）：

| tushare 字段 | 含义/单位 | 我们的模型 | 换算 |
|---|---|---|---|
| `ts_code` | `000001.SZ` / `600519.SH` / `xxxxxx.BJ` | 内部 code | 需要一层符号映射：去后缀 + 市场位（`.SH/.SZ/.BJ`）；与雪球 `SH600519`、THS `hs_600519` 并列第三套写法 |
| `trade_date` | `YYYYMMDD` 字符串 | `Bar.date` | 按 `%Y%m%d` 解析 |
| `open/high/low/close` | 元，未复权 | raw OHLC | 直存 |
| `pre_close` | **除权后的昨收** | 参考值 | 注意：除权日 pre_close ≠ 昨日 close，这正是复权语义，不要当脏数据 |
| `pct_chg` | 涨跌幅 %（**基于除权昨收**） | `Bar.pct` | 直存；语义与我们 `adjust.rs` 在除权日重算 pct 的做法一致（不会出现假摔） |
| `vol` | 成交量，**手** | `Bar.volume` + `VolumeUnit::Lots` | **天然一致，无需换算**（对比：雪球/东财是股） |
| `amount` | 成交额，**千元** | `Bar.amount`（元口径） | **×1000** |
| `ah_vol/ah_amount` | 盘后固定价成交（手/千元） | 无对应字段 | 忽略或记扩展列 |

其他：`daily` 未复权、停牌日**缺记录**（与 data-foundation-v2 约定一致，不用 volume=0 判停牌）。单次最大 6000 行。

### 3.2 `adj_factor` → 复权金标（2000 档）

- 字段仅三列：`ts_code, trade_date, adj_factor`（[adj_factor 文档](https://tushare.pro/document/2?doc_id=28)）。因子是**后复权累积因子**（上市首日 ≈ 1，随分红送转单调不减，如 000001.SZ 2018 年已累积到 108）。**除权日当天因子即变为新值**（盘前 9:15~9:20 入库当日因子，PIT 友好）。
- 与我们 `adjust.rs` 的关系：
  - tushare 口径：`后复权价(t) = raw(t) × adj_factor(t)`；`前复权价(t, 锚 T) = raw(t) × adj_factor(t)/adj_factor(T)`。
  - 我们的口径：`factor_qfq(t) = ∏{r_i | E_i > t}`，`qfq = raw × factor_qfq`。
  - 换算恒等式：**`factor_qfq(t, 锚 T) = adj_factor(t) / adj_factor(T)`**；单次行为因子比 **`r_i = adj_factor(E_i) / adj_factor(E_i 前一交易日)`**。
  - 用途①（推荐）：**金标对拍**。用 `adj_factor` 换算出的 qfq 序列与 `adjust.rs` 自算 qfq 逐日比对（容差可沿用 data-foundation-v2 的 0.5%），比"对拍腾讯 qfq"少一层供应商黑盒——data-foundation-v2 §验证金标可把 tushare 列为首选金标源（有 token 时）。
  - 用途②（可选）：从因子序列反推 `CorporateAction`——检测到 `r_i ≠ 1` 的日期即存在除权行为，可作为东财分红数据的**备份源/缺漏告警**；但反推只能得到复合因子比，拆不出 D/B/R/P 各项，不能替代结构化分红数据。
  - **PIT 注意**：`adj_factor` 全序列是"截至今天"的口径，但比率 `adj_factor(t)/adj_factor(T)`（t ≤ T）只反映 (t, T] 内的公司行为，锚定历史日 T 的 qfq 不受 T 之后行为影响——**直接用于回测是 PIT 安全的**。唯一 caveat：tushare 若事后修正历史因子（差错更正），回看会有微小改写，与东财分红数据对拍即可发现。

### 3.3 `daily_basic` → 估值/换手快照（2000 档）

| tushare 字段 | 单位 | 我们对应 |
|---|---|---|
| `turnover_rate` / `turnover_rate_f` | %（流通股/自由流通） | quote/日级换手率字段，直存 |
| `volume_ratio` | 量比 | 直存 |
| `pe` / `pe_ttm` / `pb` / `ps` / `ps_ttm` | 倍 | 与雪球 quote 的 `pe_ttm/pb` 同语义，可对拍 |
| `dv_ratio` / `dv_ttm` | 股息率 % | 直存 |
| `total_share`/`float_share`/`free_share` | **万股** | 股本字段，×10⁴ 转股 |
| `total_mv` / `circ_mv` | **万元** | 市值字段，×10⁴ 转元 |
| `limit_status` | 0~6 涨跌停状态枚举 | 可选，涨跌停判定辅助 |

### 3.4 `dividend` → `CorporateAction`（2000 档）

[dividend 文档](https://tushare.pro/document/2?doc_id=103) 字段到我们 schema（`corporate_actions` 表）的映射：

| tushare 字段 | `CorporateAction` 字段 | 说明 |
|---|---|---|
| `ex_date` | `ex_date` | 直接对应（YYYYMMDD 解析） |
| `imp_ann_date` / `ann_date` | `notice_date` | 实施公告日优先，为空退预案公告日；可强化我们的 strict PIT 变体（东财源 notice_date 常缺） |
| `cash_div_tax` | `cash_div` | **税前**每股分红，正是 `adjust.rs` 的 D 口径（注意别用成税后 `cash_div`） |
| `stk_div` | `bonus_share` | 每股送转合计（= `stk_bo_rate + stk_co_rate`，已是每股口径，无需 ÷10） |
| — | `rights_ratio` / `rights_price` | **tushare 无配股接口**，配股缺口与现状相同（data-foundation-v2 已注明"配股数据待补源"，tushare 解决不了） |
| `div_proc` | — | 只取"实施"状态（预案无 ex_date，样例数据中预案行 ex_date 为 None） |

### 3.5 财务三大表（2000 档）

`income`/`balancesheet`/`cashflow`/`fina_indicator` 均为"ts_code + ann_date/end_date/period 查询，金额单位元"的宽表，字段上百个。我们当前财务数据走雪球 finance/东财 F10 已覆盖；tushare 财务表结构规整、带 `ann_date`（公告日，PIT 友好）、`update_flag`（更正标记），**若未来做 PIT 财务因子库，tushare 2000 档是优于爬虫源的选择**。本期不展开字段级映射。

---

## 4. 稳定性与风险

| 风险 | 评估 | 对策 |
|---|---|---|
| 反爬/JS 挑战 | **无**。官方商业 API，token 即全部鉴权 | 不需要任何绕过手段 |
| 接口稳定性 | 高。2017 年 pro 上线至今协议未变，官方承诺 HTTP 通道（"http 目前运行良好，稳定性已经得到了验证"） | 按 `fields` 建列索引，防默认字段顺序调整 |
| 限流 | 硬限（40203 + 每日总量），超了就是报错，不会封号式反爬 | 沿用现有自适应限速 + 单飞合并；盘后批量按 trade_date 拉全市场（请求数最少） |
| 权限门槛上调 | 有先例（`stock_basic` 从基础积分上调到 2000） | provider 层对 2002 做优雅降级：标记健康面板、自动切回免费源，不静默失败 |
| token 安全 | token = 用户资产（付费积分），泄露=被盗用额度 | 本地存储（kv 表），不落日志、不上报、不进遥测 |
| 数据质量 | 社区维护，业界口碑中上；复权因子为官方自生产，盘前就绪 | adj_factor 与东财分红 + 腾讯 qfq 三方对拍（data-foundation-v2 §交叉验证管线原样适用） |
| 服务连续性 | 个人/小团队运营的商业服务，存在理论停运风险 | 定位为备份/金标源而非唯一主力源，主链路仍走免费源 |

---

## 5. Token 设置页设计（与聚宽同模式）

1. **设置页加"Tushare Pro"区块**：说明文案（注册地址 `tushare.pro`、注册 100 + 完善资料 20 = 120 免费积分、个人主页复制 token）+ token 输入框 + "验证并保存"按钮。
2. **验证逻辑**：用所填 token 调一次最便宜的有把握接口——推荐 `daily(trade_date=<最近交易日>, fields='ts_code,close')`（120 档必可用）而非 `stock_basic`（门槛 2000，免费 token 会误报无效）。`code == 0` → 保存；`code == 2002` → 提示"token 有效但积分不足 120"？（实际 2002 即权限不足）；其他 `code != 0` → 展示 `msg`。
3. **积分档探测**：保存后依次试调 `adj_factor` / `daily_basic`（各一次，trade_date 取最近交易日），根据 2002 与否推断档位（120 / 2000+），在设置页显示"当前档位：免费 120（仅日线）/ 2000+（全功能）"，并据此开关下游功能（金标对拍、daily_basic 同步等）。**这是用户体验关键点**：不要让 120 档用户看到一堆 2002 报错。
4. **无 token 时**：provider 整体禁用，不影响东财/腾讯/雪球现有链路（与 joinquant provider 的禁用语义一致）。
5. **生产安全修订**：本段调研早期提出的 SQLite/`kv` token 方案已废弃。v6 只把 Tushare token 写入 Windows Credential Manager；Engine 在请求期间读入私有内存对象，不进入 SQLite、环境变量、命令行、React 状态或日志。无 Credential Manager 凭据时 provider 必须显式禁用。

---

## 6. 结论与建议

1. **可行性：纯 Rust 直连，无悬念。** 官方 HTTP JSON API + body token 鉴权，`reqwest + serde_json` 一个 client 结构体搞定，是所有调研源中实现成本最低的（对比：雪球要 cookie 会话、THS 要 JSONP 剥壳、聚宽要 thrift+pickle）。
2. **定位：可选 token provider，不是主力免费源。** 免费 120 档只有 raw 日线——恰好是我们 data-foundation-v2 唯一必须存的东西，值得接为**第四路 raw 日线源 + 权威交叉验证源**（vol 单位"手"与我们 Lots 口径一致，pct_chg 已是除权口径，清洗成本最低）。
3. **2000 档的价值在于 `adj_factor` 金标**：`factor_qfq(t,T) = adj_factor(t)/adj_factor(T)` 恒等式让它成为 `adjust.rs` 最直接的验证基准；`dividend` 的 `imp_ann_date` 还能补我们 `notice_date` 缺口。但 200 元/年由用户自担，做成"检测到高档位自动启用增强功能"。
4. **不接的部分**：分钟线/实时（独立付费）、`pro_bar` 复权行情（我们自己算）、财务宽表（本期无 PIT 财务库需求）。
5. **优先级：中。** 主链路已被免费源覆盖；tushare 的收益是数据质量与验证体系（金标），不是覆盖率。建议在 data-foundation-v2 交叉验证管线落地时同步接入。

## 附：Rust 实现要点

- 单个 `TushareClient { http: reqwest::Client, token: String, rate: RateLimiter }`；`call(api_name, params, fields) -> Result<(Vec<String> fields, Vec<Vec<Value>> items)>`，按 `fields` 建 `HashMap<&str, usize>` 列索引后逐行映射。
- 统一错误枚举：`RateLimited(40203)` 可退避重试、`NoPermission(2002)` 记健康面板不重试、`Other(code, msg)` 直接失败。
- 批量同步按 `trade_date` 循环（全市场一天一次请求），不按 `ts_code` 循环；落库走 data-foundation-v2 现有 raw 管线（fqt=0 口径天然吻合）。
- 符号映射：内部 code ↔ `ts_code`（`.SH/.SZ/.BJ` 后缀），与雪球/THS 的映射层并列实现。
- 金额换算仅两处：`amount` 千元→元、`daily_basic` 万股/万元→股/元；`vol` 手直存。
