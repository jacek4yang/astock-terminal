# 同花顺 (10jqka) / 雪球 (xueqiu) 数据源调研报告

调研日期：2026-08-22（本地时间，A 股 2026-08-21 收盘后实测）。调研方式：通读 akshare main 分支相关源码（`stock/stock_xq.py`、`stock_feature/stock_hot_xq.py`、`stock_feature/stock_board_concept_ths.py`、`stock_fundamental/stock_finance_ths.py`、`data/ths.js`）+ curl 实测全部关键端点。本地 `akshare-ref/` 不含这两家源码，akshare 源码取自 GitHub raw（直连可用，无需代理）。

## 结论速览

- **雪球高度可用**：游客 cookie `xq_a_token` 用纯 HTTP 两次 GET 即可拿到（先 GET `https://xueqiu.com/` 拿 WAF cookie，带 cookie 再 GET 一次即下发 `xq_a_token`），无需登录、无需 JS 挑战。行情/K线/分时/关注度/讨论数/财务报表/组合净值全部实测可用。唯一受限的是**组合调仓明细**（`rebalancing/history.json` 游客 token 返回 10022，需登录账号）。
- **同花顺分裂成两类**：
  - `d.10jqka.com.cn`（行情/K线/分时/板块指数）和 `basic.10jqka.com.cn`（财务 JSON）**完全不需要 hexin-v**，纯 HTTP + Referer 即可，实测全部可用 —— 这是我们应优先接入的部分。
  - `q.10jqka.com.cn` / `data.10jqka.com.cn` 的 `ajax/1` 分页接口有 **chameleon JS 挑战 + hexin-v cookie**；hexin-v 可以用 akshare 的 `ths.js` 在 JS 引擎里算出（akshare 用 py_mini_racer，我用 node 实测生成的 v 值可通过挑战），但 **Rust 侧需要内嵌 JS 引擎（boa/quickjs），不建议为此引入**。且**问财 iwencai 即使带合法 hexin-v 也直接弹验证码，纯 HTTP 不可行**。
- **建议接入优先级**：雪球 quote/kline/情绪/财务（高价值低风控）> d.10jqka K线/分时/realhead（作备份源）> basic.10jqka 财务（备用）> 其余 THS ajax 接口（需 JS 引擎，暂缓）> iwencai（不可行）。

---

## 1. 雪球 (xueqiu.com)

### 1.1 认证流程（2026-08-22 实测验证）

akshare 的做法（`stock/stock_xq.py`）是先 GET `https://xueqiu.com` 再带 cookie 请求 API。实测细节比源码注释更微妙：

1. 第一次 `GET https://xueqiu.com/`（带浏览器 UA）→ HTTP 200，但**只下发 WAF cookie**：`acw_tc`（阿里云盾）、`by`。**此时没有 `xq_a_token`**。
2. **带这些 cookie 再请求一次**（实测用 `GET https://xueqiu.com/hq`，同一会话）→ 下发完整 token 组：`xq_a_token`、`xq_r_token`、`xqat`、`xq_id_token`、`u`、`cookiesu`。
3. 之后 `stock.xueqiu.com` 的 v5 API 只需 cookie 里有 `xq_a_token` 即可（akshare 也只放这一个）。

无 token 直接调 API 的响应（实测）：

```json
{"error_description":"遇到错误，请刷新页面或者重新登录帐号后再试","error_uri":"/v5/stock/quote.json","error_data":null,"error_code":"400016"}
```

要点：

- token 是**游客 token**，未登录即可用；`xq_a_token` 有效期较长（经验值数天~数月），失效表现即 400016，重新走两遍 GET 换新即可。
- UA 无严格要求，桌面 Chrome UA 即可（akshare 用 iPhone UA 也行）。
- 频控：官方无文档。雪球对游客 IP+token 有隐性限流，高频轮询会触发 400/403 或滑块。建议单 token 串行、每秒 ≤2~3 请求，批量用 `batch/quote.json` 合并。

### 1.2 端点清单（全部实测于 2026-08-22）

以下请求头统一为：`Cookie: xq_a_token=<token>` + 浏览器 UA；热榜类建议加 `Referer: https://xueqiu.com/hq`。

#### A. 个股/指数/基金行情 — `GET https://stock.xueqiu.com/v5/stock/quote.json?symbol=SH600519&extend=detail` ✅

- symbol 格式：`SH600519` / `SZ000001` / `BJ430139` / `SH000001`（指数，实测可用）。
- JSON 路径：`data.quote`（字段：`current` 现价、`last_close`、`open/high/low`、`volume` **单位：股**、`amount` **单位：元**、`turnover_rate`、`percent`、`chg`、`amplitude`、`pe_ttm`、`pe_lyr`、`pb`、`market_capital`/`float_market_capital`（元）、`total_shares`/`float_shares`（股）、`limit_up`/`limit_down`、`timestamp`（毫秒）、`avg_price`）；`data.market.status` 给出市场状态（实测收盘后返回 `"休市"`，`time_zone: Asia/Shanghai`）。
- 实测样本（截断）：

```json
{"data":{"market":{"status_id":8,"region":"CN","status":"休市","time_zone":"Asia/Shanghai"},
 "quote":{"symbol":"SH600519","current":1272.83,"chg":-18.67,"volume":3347231,
 "turnover_rate":0.27,"float_shares":1250081601,"limit_down":1162.35,"lot_size":100,...}}}
```

#### B. 批量行情 — `GET https://stock.xueqiu.com/v5/stock/batch/quote.json?symbol=SH600519,SZ000001,SH000001` ✅

- 多 symbol 逗号分隔，一次返回 `data.items[].quote`，适合全市场轮询降频。

#### C. K线 — `GET https://stock.xueqiu.com/v5/stock/chart/kline.json` ✅

- 参数：`symbol`、`begin`（毫秒时间戳，取该时刻之前的 count 根）、`period`（`day/week/month/60m/30m/15m/5m/1m`，5m 实测可用）、`count`（负数向前取，如 `-284`）、`type`（**复权**：`before` 前复权 / `after` 后复权 / `normal` 不复权；注意后复权价可能远大于现价，实测茅台后复权 close=11432 vs 现价 1272）、`indicator=kline`（还可逗号加 `pe,pb,ps,pcf,market_capital,agt,ggt,balance`）。
- JSON 路径：`data.column` + `data.item[][]`。column 固定为 `timestamp,volume,open,high,low,close,chg,percent,turnoverrate,amount,volume_post,amount_post`（`*_post` 为盘后数据，分钟线常为 null）。
- 单位：volume **股**、amount **元**；timestamp 为毫秒（日线为当地 00:00）。
- 实测样本（日线前复权，截断）：

```json
{"data":{"symbol":"SH600519","column":["timestamp","volume","open","high","low","close",...],
 "item":[[1787241600000,3347231,1291.5,1291.5,1272.01,1272.83,-18.67,-1.45,0.27,4.278311022E9,1300,1654679.0]]},
 "error_code":0}
```

#### D. 当日分时 — `GET https://stock.xueqiu.com/v5/stock/chart/minute.json?symbol=SH600519&period=1d` ✅

- 返回 `data.last_close` + 分钟序列（盘中在 `data.mintimeline`，收盘后实测只有 `data.after` 盘后段），字段 `current/volume/avg_price/chg/percent/timestamp`。

#### E. 情绪指标：关注/讨论/分享交易排行 — `GET https://xueqiu.com/service/v5/stock/screener/screen` ✅（高价值）

- 参数：`category=CN`、`size`（≤200 每页）、`order=desc`、`order_by` ∈ `follow / follow7d / tweet / tweet7d / deal / deal7d`（关注总数、一周新增关注、讨论总数、一周新增讨论、分享交易总数/一周新增）、`only_count=0`、`page`。
- JSON 路径：`data.count`（全市场只数，实测 5629）、`data.list[]` 每行含 `symbol,name,current,pct,follow7d/tweet7d/...`。
- **个股级情绪序列可通过定时快照排行、或按 symbol 翻页定位获得**（akshare 只有排行用法）。
- 实测样本（截断）：

```json
{"data":{"count":5629,"list":[{"pct":-2.12,"symbol":"SH688836","current":672.41,"follow7d":39503,"name":"C宇树-W"},...]}}
```

#### F. 热股榜 — `GET https://stock.xueqiu.com/v5/stock/hot_stock/list.json?size=100&_type=10&type=10` ✅

- `type/_type`：10=沪深、12=港股、13=美股。返回 `data.items[]`：`code,name,value`（热度值）,`increment,percent,current,rank_change`。

#### G. 财务三大表 — `GET https://stock.xueqiu.com/v5/stock/finance/cn/{income|balance|cash_flow}.json` ✅

- 参数：`symbol`、`type`（`Q4`=年报、`Q3`、`Q2`、`Q1`，也可 `all`）、`is_detail=true`、`count=5`。
- JSON 路径：`data.list[]`，每期一个对象，字段如 `report_date`（毫秒）、`report_name`、`total_revenue`、`net_profit_atsopc` 等，**每个指标是 `[值, 同比增速]` 二元数组**，金额单位**元**。实测 income/balance 均可用。
- 实测样本（截断）：

```json
{"data":{"quote_name":"贵州茅台","currency":"CNY","list":[{"report_date":1767110400000,"report_name":"2025年报",
 "net_profit_atsopc":[8.232006710168E10,-0.0453],"total_revenue":[1.7205417189091E11,-0.012],...}]}}
```

#### H. 组合（cube）公开数据

- 组合排行榜：`GET https://xueqiu.com/cubes/discover/rank/cube/list.json?category=12&count=20&market=cn&profit=monthly_gain` ✅ —— 注意 `profit` 合法值是 `daily_gain/weekly_gain/monthly_gain/annualized_gain` 等下划线形式，`month_gain` 会报 `33004 发现页profit非法参数`（实测踩坑）。返回组合 `symbol`（如 ZH1416500）、`monthly_gain`、`net_value`、`follower_count` 等。
- 组合净值曲线：`GET https://xueqiu.com/cubes/nav_daily/all.json?cube_symbol=ZHxxxx` ✅ 游客可用，返回 `[{symbol,name,list:[{time,date,value,percent}]}]`。
- **组合调仓明细：`GET https://xueqiu.com/cubes/rebalancing/history.json?cube_symbol=...&count=20&page=1` ❌ 游客 token 实测返回 `{"error_code":"10022"}`（无权限），多个公开组合均如此 —— 需登录账号 cookie（且部分组合仅对关注者开放）。结论：调仓数据不适合作为匿名数据源，除非让用户填自己的雪球 cookie。**

### 1.3 反爬与稳定性

- 反爬层：入口页的阿里云盾 `acw_tc`（纯 Set-Cookie，无 JS 计算）；API 层只看 `xq_a_token` + 频控。**全程无 JS 挑战**，Rust `reqwest` + cookie store 即可完整实现。
- 稳定性：v5 API 多年未变（akshare 持续在用），属高稳定。风险点是 token 过期与 IP 频控。
- 建议 TTL：quote/minute 3~5s（盘中）、kline 日线 1h、screener 情绪 5~15min、finance 6~24h、cube 净值 1h。
- 降级：雪球不可用 → 腾讯/新浪/东财行情（已接入）；财务 → 东财 datacenter 或 basic.10jqka。

---

## 2. 同花顺 (10jqka)

### 2.1 hexin-v / JS 挑战现状（诚实版）

- THS 的反爬体系叫 chameleon：命中风控时返回一段引用 `//s.thsi.cn/js/chameleon/chameleon.x.x.min.js` 的 HTML，执行后算出 cookie `v`（即 hexin-v）再刷新。
- akshare 的方案：内置一份 2019 年逆向出来的 `akshare/data/ths.js`（39KB，硬编码 `TOKEN_SERVER_TIME = 1572845499.629`），用 **py_mini_racer** 执行 `v()` 算出 cookie 值。**实测（2026-08-22）用 node 执行该脚本生成的 v 值仍被服务端接受**（`data.10jqka.com.cn/funds/hyzjl` 带 `Cookie: v=<生成值>` 返回真实表格，不带则返回 chameleon 挑战页）。
- **对我们的含义**：`v` 算法是一次性逆向产物，Rust 侧要么内嵌 JS 引擎（boa/quickjs，增加数 MB 依赖），要么预生成/缓存 v 值低频使用。因此本报告明确区分"无需 v 可用"与"需要 v"两类，**建议只接入前者**。
- **iwencai 问财**：`POST http://www.iwencai.com/customized/chart/get-robot-data` 实测**不带 v 和带合法 v（cookie + `hexin-v` 头）均返回**：

```json
{"code":0,"data":{"captcha_url":"http://www.iwencai.com/ac_verification/captcha/?host=..."}}
```

  即直接弹图形验证码（大概率机房 IP/无浏览器指纹所致）。**结论：问财纯 HTTP 不可行，标记为 not viable**（要上只能走真实浏览器/Playwright，且随时可能封号）。
- 其他已死/被拒：`q.10jqka.com.cn/api.php?t=indexflash` → `<h1>Nginx forbidden.</h1>`；`q.10jqka.com.cn/gn/detail/field/.../ajax/1/code/xxx`（概念成分股分页）**带合法 v 仍 Nginx forbidden**（IPv4/IPv6 均试），该接口当前不可用，第一页成分股只能解析 `gn/detail/code/xxx/` HTML 页面本身（HTTP 200）。

### 2.2 无需 hexin-v 的可用端点（实测 ✅，推荐接入）

统一请求头：浏览器 UA + `Referer`（见各条）。无 cookie 要求。

#### A. 个股/指数/板块 K线 — `d.10jqka.com.cn` ✅（高价值，可作东财备份）

```
http://d.10jqka.com.cn/v6/line/hs_600519/01/last.js      # 个股日线（当年+索引）
http://d.10jqka.com.cn/v6/line/hs_600519/01/2025.js      # 个股日线指定年份
http://d.10jqka.com.cn/v6/line/zs_1A0001/01/last.js      # 指数（上证指数，zs_ 前缀）
http://d.10jqka.com.cn/v4/line/bk_881121/01/last.js      # 板块指数（bk_ 前缀，注意是 v4）
```

- 必须 `Referer: http://stockpage.10jqka.com.cn/600519/`（或 `http://q.10jqka.com.cn`），不带 Referer 会 403。
- 返回 **JSONP**：`quotebridge_v6_line_hs_600519_01_last({...})`，需剥壳。
- 结构：`data` 字段是分号分隔的记录串，每条 `日期,开,高,低,收,成交量(股),成交额(元),换手率,...`；`year` 字段给出各年份交易日数量索引；`01` 是周期+复权组合代码（akshare 用 01=前复权日线；分钟线另有代码），文档化程度低，接入时逐个验证。
- 实测样本（截断）：

```
quotebridge_v6_line_hs_600519_01_2025({"data":"20250102,1444.35,1444.84,1400.35,1408.35,5002870,7490883800.00,0.398,,,0;20250103,...
```

#### B. 实时报价头 — `http://d.10jqka.com.cn/v2/realhead/hs_600519/last.js` ✅

- 同样只需 Referer。JSONP，字段是**数字键字典**（`items` 内 `"10"`=现价、`"8"`=昨收、`"7"`=今开、`"9"`=最低、`"13"`=成交量（股）、`"19"`=成交额（元）等，完整映射需对照 THS 行情页 JS，用前建议先抽样校验）。实测数值与雪球 quote 完全对齐（成交量 3347231 股、成交额 4.278e9 元）。

#### C. 当日分时 — `http://d.10jqka.com.cn/v6/time/hs_600519/last.js` ✅

- JSONP。实测样本（截断）：

```
quotebridge_v6_time_hs_600519_last({"hs_600519":{"name":"贵州茅台","open":0,"stop":0,
 "tradeTime":["0930-1130","1300-1500"],"pre":"1291.50","date":"20260821",
 "data":"0930,1291.50,21568050,1291.500,16700;0931,1285.00,562877...
```

- `data` 每条 `时间,现价,累计成交额(元),均价,累计成交量` —— 分时累计量与日线差约 100 倍量级（疑似手），接入前必须用同日数据交叉校准；`stop` 字段标记停牌（0=正常）。

#### D. 财务三大表 JSON — `basic.10jqka.com.cn` ✅（无需任何 cookie）

```
https://basic.10jqka.com.cn/api/stock/finance/600519_debt.json     # 资产负债
https://basic.10jqka.com.cn/api/stock/finance/600519_benefit.json  # 利润表
https://basic.10jqka.com.cn/api/stock/finance/600519_cash.json     # 现金流
```

- 返回是**双层 JSON**：外层 `{"flashData":"<转义的JSON字符串>"}`，需解析两次（akshare `json.loads(json.loads(r.text)["flashData"])`）。内层 `title` 为科目名（含 `[科目,单位,序号,...]` 复合项），`report`/`year`/`simple` 分别对应按报告期/年度/单季度。金额单位随科目（title 里标注，多为"元"）。
- `https://basic.10jqka.com.cn/new/600519/finance.html` 页面内 `<p id="main">` 还内嵌主要指标 JSON（akshare `stock_financial_abstract_ths` 用法），页面 HTTP 200 实测可抓。
- `https://basic.10jqka.com.cn/600519/company.html` 等公司资料页 HTTP 200（HTML 抓取）。

### 2.3 需要 hexin-v 的端点（暂不接入，仅记录）

以下不带 `v` cookie 时返回 chameleon JS 挑战页；带 node 生成的合法 v 后返回真实数据（GBK 编码 HTML 表格）：

| 端点 | 内容 | 带 v 实测 |
|---|---|---|
| `q.10jqka.com.cn/thshy/index/field/199112/order/desc/page/N/ajax/1/` | 行业板块列表分页 | ✅ 返回表格 HTML |
| `q.10jqka.com.cn/gn/index/field/addtime/order/desc/page/N/ajax/1/` | 概念板块列表分页 | ✅ 返回表格 HTML（含 `gn/detail/code/xxxxxx/` 链接） |
| `data.10jqka.com.cn/funds/hyzjl/field/tradezdf/order/desc/page/1/ajax/1/` | 行业资金流 | ✅ 返回表格 HTML |
| `data.10jqka.com.cn/market/longhu/` | 龙虎榜页 | ✅（页面本身 200 无需 v，HTML 抓取） |
| `q.10jqka.com.cn/gn/detail/field/.../ajax/1/code/xxx` | 概念成分股分页 | ❌ Nginx forbidden（带 v 也不行，当前已废） |

注意这些 ajax 接口全部返回 **GBK 编码的 HTML 表格片段**，需要 HTML 解析 + 转码，比 JSON 接口脆弱。若未来要接，方案是 Rust 内嵌 boa/quickjs 执行 `ths.js` 的 `v()`（v 值有效期约分钟级，需每次会话重新生成）。

---

## 3. 健壮性评估汇总

| 端点 | 反爬强度 | 预期稳定性 | 建议 TTL | 降级策略 |
|---|---|---|---|---|
| 雪球 quote/batch quote | 低（游客 token） | 高 | 3~5s（盘中） | 腾讯/新浪/东财 quote |
| 雪球 kline/minute | 低 | 高 | 日线 1h，分钟 1min | 东财 kline；d.10jqka K线 |
| 雪球 screener 情绪 / hot_stock | 低 | 中（接口偶有调整） | 5~15min | 仅排行榜快照降级，无替代源 |
| 雪球 finance | 低 | 高 | 6~24h | 东财 F10 / basic.10jqka finance |
| 雪球 cube 排行/净值 | 低 | 中 | 1h | 无替代，失败即跳过 |
| 雪球 cube 调仓明细 | **需登录** | — | — | 不可用（匿名） |
| d.10jqka K线/分时/realhead | 低（仅 Referer） | 中高（JSONP 格式多年未变） | 日线 1h，realhead 5~10s | 东财/雪球 |
| basic.10jqka finance JSON | 低 | 中（双层 JSON 结构脆） | 6~24h | 东财/雪球 finance |
| q.10jqka / data.10jqka ajax | **高（hexin-v + chameleon）** | 中 | 只适低频批量（每日 1~2 次全量） | 东财板块/资金流/龙虎榜 datacenter |
| iwencai 问财 | **极高（验证码）** | — | — | **不可行，放弃** |

**只适合低频批量抓取的**：所有需 hexin-v 的 THS ajax 接口（板块列表、行业资金流）——即便未来接入 JS 引擎，也应限制为每日盘前/盘后 1~2 次全量快照并落库，不做盘中轮询。

## 4. 正确性注意事项

1. **成交量单位**：雪球 quote/kline 的 `volume` 是**股**（茅台 3347231 = 3.35 万手），不是手；THS d 接口日线成交量同为**股**；THS 分时 `data` 串的累计量字段疑似为**手**（与日线差约 100 倍量级），接入前必须用同日数据交叉校准。成交额两边都是**元**（不是万元）。
2. **复权**：雪球 `type=before/after/normal`；后复权价会远超名义价（茅台 11432 vs 1272），与东财 `fqt` 语义一致但不要混用。THS `d` 接口路径中的 `01` 段是周期+复权组合代码，akshare 用 `01`（前复权日线），改周期/复权需换代码，文档化程度低，接入时逐个验证。
3. **时区/时间戳**：雪球全部为**毫秒** Unix 时间戳，市场状态字段明示 `Asia/Shanghai`；日线 kline 的 timestamp 是当地 00:00:00（UTC+8）。THS K线只有 `YYYYMMdd` 字符串日期，分时为 `HHMM` 字符串 + 单独 `date` 字段。
4. **停牌表示**：雪球停牌股 quote 中 `current` 可能为 null 或维持昨收，`market.status` 只表示市场级状态；kline 停牌日**缺记录**（无零量行）。THS 分时 `stop` 字段（0/1）标记停牌，日线停牌日同样缺记录。不要用"volume=0"判断停牌。
5. **编码**：THS HTML 接口（q./data.10jqka ajax、龙虎榜页）是 **GBK**；d.10jqka JSONP 和 basic.10jqka JSON 是 UTF-8（JSONP 内中文以 `\uXXXX` 转义形式出现）。
6. **JSONP 剥壳**：d.10jqka 返回 `quotebridge_xxx(...)`，按第一个 `{` 到最后一个 `}` 截取（akshare 的 `data_text[data_text.find("{"):-1]` 同款做法）。
7. **双层 JSON**：basic.10jqka finance 要 `parse(parse(text).flashData)`。
8. **hexin-v 时效**：v 值绑定服务端时间窗，akshare 的 ths.js 硬编码 2019 时间戳仍被接受（实测），但不要缓存 v 值跨会话复用——每次抓取会话重新生成。
9. **雪球错误语义**：`error_code` 为字符串；`400016`=token 失效（重新取 cookie 即可自愈），`10022`=无权限（登录态问题，重试无用），`33004`=参数非法。建议只对 400016 做自动刷新 token 重试一次。
10. **符号体系**：雪球用 `SH600519/SZ000001/BJ430139`；THS d 接口用 `hs_600519`（hs_ 前缀沪深通吃，指数用 `zs_1A0001` 这类数字代码，板块用 `bk_881121`）。需要维护一层 symbol 映射，北交所支持度 THS 侧未验证。

## 附：Rust 实现要点

- 雪球：`reqwest::Client` + `cookie_store`，启动时 GET 两次 `https://xueqiu.com`（第二次任意路径如 `/hq`）即持票；所有 `stock.xueqiu.com` 请求自动带票。token 失效（400016）→ 重建会话重试一次。
- d.10jqka：纯 GET + Referer 头 + JSONP 剥壳 + 分号/逗号切分，无状态，实现最简单，适合做东财 K线的交叉校验/备份源。
- 不引入 JS 引擎：本期不接 q./data.10jqka ajax 与 iwencai；板块/资金流/龙虎榜继续用东财 datacenter。
