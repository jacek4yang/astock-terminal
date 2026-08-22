# 聚宽 (JoinQuant / JQData) 数据源调研报告

调研日期：2026-08-22（本地时间）。调研方式：通读官方 jqdatasdk 源码（[JoinQuant/jqdatasdk](https://github.com/JoinQuant/jqdatasdk) master 分支）+ curl 实测 joinquant.com / dataapi.joinquant.com 公开端点。

## 结论速览

- **JQData 不是免费匿名服务**：必须有聚宽注册账号且开通 JQData 调用权限。免费试用账号存在（申请后开通，有效期一年，每天 100 万条基础数据），但需要人工提交申请，不是"注册即可用"。
- **协议是半封闭的**：历史/批量数据走 **私有 thrift TCP 协议**（`39.107.190.114:7000`），响应体是 **zlib 压缩的 Python pickle 对象**。从 Rust 直接实现成本很高（需要在 Rust 里解析 Python pickle），不建议。
- **HTTP API 确实存在但极窄**：`https://dataapi.joinquant.com/v2/apis` 只用于获取 token 和实时 tick，已实测可用（返回明文字符串）。
- **joinquant.com 网页端没有可依赖的匿名数据 API**：首页/文档页可匿名访问（HTTP 200），但都是 SPA 壳子；内部 JSON API（如 `/api/valuation`）要求"通用参数"（登录态/签名），匿名调用被拒。
- **建议**：作为**可选 provider** 集成（用户在设置中粘贴聚宽账号/密码或 token），且**通过 Python sidecar（jqdatasdk）调用**，不要在 Rust 里重造 thrift+pickle 协议栈。优先级低于已接入的免费源。

---

## 1. JQData (jqdatasdk)

### 1.1 提供的数据

按官方介绍与 SDK 源码（`api.py`、`finance_service.py`、`alpha101.py`、`alpha191.py`、`technical_analysis.py`）：

- **行情**：全市场（股票/指数/基金/期货/期权/可转债）日/周/月/分钟线，tick 级数据，实时 current tick；`get_price` 跨品种统一接口。
- **财务/基本面**：三大报表、市值/估值指标（`finance_service.py` + `fundamentals_tables_gen.py`）。
- **因子**：Alpha101、Alpha191、聚宽因子、技术指标（`technical_analysis.py`，2700 行）、分钟资金流、CNE5/CNE6 风险模型（部分为付费增值）。
- **宏观/其他**：交易日历（`calendar_service.py`）、期货信息、龙虎榜等（具体接口见其 API 文档 `https://www.joinquant.com/help/api/doc?name=JQDatadoc`，页面为 JS 渲染，curl 只能拿到 SPA 壳）。

### 1.2 账号要求与配额（关键）

- `auth(username, password)` 用的是**聚宽官网注册账号 + 密码**，但仅有账号不够——实测用假账号调 token 接口返回：

  ```
  error: 用户不存在或密码错误;
  如果未开通调用权限，请打开以下链接提交申请：https://www.joinquant.com/default/index/sdk
  ```

  即**必须单独提交 JQData 试用申请**（`https://www.joinquant.com/default/index/sdk`，需如实填写资料）。
- **免费试用配额**：申请通过后开通**一年有效期**的试用账号，**每天可调用 100 万条**基础数据（来源：jqdatasdk README / [知乎专栏](https://zhuanlan.zhihu.com/p/145049782) / [Gitee 镜像](https://gitee.com/honstdw/jqdatasdk)，多处一致）。基础数据含沪深 A 股行情等；因子库、tick、CNE 风险模型等为增值/付费项。
- 官方[《试用和购买说明》](https://www.joinquant.com/help/api/doc?name=logon&id=10263)还注明：jqdatasdk **非线程安全**、试用账号**仅支持 1 条并发连接**、建议单次查询不超过 10 万条。
- 配额可程序化查询：`get_query_count(field)`（"total"/"spare"），走 thrift 通道。

### 1.3 认证与协议（源码级细节，已核实）

源码文件：`jqdatasdk/client.py`、`jqdatasdk/thriftclient.py`（thrift IDL 内嵌）。

**通道 A —— thrift TCP（主数据通道，所有历史/批量数据都走这里）：**

- 服务器：硬编码默认 `39.107.190.114:7000`（`client.py:50-51`），可用 host/port 参数或环境变量覆盖。
- thrift IDL（`thriftclient.py` 内嵌，逐字）：

  ```thrift
  struct St_Query_Rsp { 1:required bool status, 2:optional string msg, 5:optional string error }
  struct St_Query_Req { 1:required string method_name, 2:required binary params }
  service JqDataService {
      St_Query_Rsp query(1:St_Query_Req rsp),
      St_Query_Rsp auth(1:string username, 2:string password, 5:bool compress,
                        8:string mac, 10:string version2, 11:string pyversion),
      St_Query_Rsp auth_by_token(1:string token)
  }
  ```

- 认证：`client.auth(username, password, compress=True, mac地址, sdk版本, python版本)` —— 注意会**上报 MAC 地址**和 Python 版本。
- 请求：`method_name` + **msgpack** 序列化的参数 dict；响应 `msg` = **zlib 压缩后的 Python pickle 字节流**（`client.py:302-307`），反序列化后得到含 `data_type: "pandas_dataframe"` 等标记的 dict，再重建 pandas 对象（`client.py:337-390`）。
- 断线检测用 `query("ping", {})` 期望返回 `"pong"`。

**通道 B —— HTTP（仅两个用途）：**

- 端点：`https://dataapi.joinquant.com/v2/apis`（`client.py:42`），POST，body 为 JSON 字符串。
- 取 token：`{"method":"get_current_token","mob":<用户名>,"pwd":<URL编码后的密码>}`，成功返回明文 token 字符串，失败返回 `error: ...` 开头的文本（已实测，见上）。
- 实时 tick：`{"method":"get_current_ticks2","token":<token>,"code":"000001.XSHE,...}"`（`api.py:762-771`）。未带 token 实测返回 `error: token不能为空`。
- 限流：HTTP 429 = "请求频率过高"。
- **历史 K 线/财务等所有重型接口没有 HTTP 版本**，SDK 全部走 thrift。

**从 Rust 直接调用的可行性**：

| 通道 | 可行性 | 说明 |
|---|---|---|
| HTTP token + 实时 tick | ✅ 可行 | 纯 JSON/文本，`reqwest` 即可；但只有 token 和实时 tick 两个方法 |
| thrift 数据通道 | ⚠️ 理论可行、实际不值 | Rust 有 `thrift` crate + `rmp`（msgpack）+ `flate2`（zlib），但响应是 **Python pickle**（含 pandas 内部结构），需要在 Rust 里实现 pickle 解析（有 `serde-pickle`，但 pandas DataFrame 的 pickle 布局多变）。维护成本高、脆弱 |

## 2. 聚宽网页端公开数据实测（curl，2026-08-22）

全部使用浏览器 UA、不带 cookie 实测：

| URL | 结果 |
|---|---|
| `https://www.joinquant.com/` | 200，HTML（可匿名访问，SPA） |
| `https://www.joinquant.com/default/index/sdk`（试用申请页） | 200，HTML SPA 壳（9 KB，无内联数据） |
| `https://www.joinquant.com/help/api/doc?name=JQDatadoc` | 200，SPA 壳（7 KB，内容 JS 渲染） |
| `https://www.joinquant.com/index/valuation`（指数估值页） | 200 但 **body 为空**，纯前端渲染 |
| `https://www.joinquant.com/api/valuation` | 200，`application/json`，body = `{"code":4,"msg":"缺失必要的通用参数"}` —— 存在内部 JSON API，但需要登录态/签名"通用参数"，匿名不可用 |
| `https://dataapi.joinquant.com/v2/apis` `get_current_token`（假账号） | 200，明文 `error: 用户不存在或密码错误;...` —— 接口本身在线、格式已确认 |
| 同上 `get_query_count` / `get_current_ticks2`（无 token） | `error: token不能为空` |

**结论**：网页端指数估值、市场温度类数据由前端调用登录态 API 获取，**没有可直接白嫖的匿名端点**。`/api/*` 家族的参数结构没有公开文档，逆向属于灰色地带且易失效，不建议作为依赖。（`/api/valuation` 的"通用参数"具体指什么未进一步逆向验证，如实说明。）

## 3. JQData 独有的、东财/同花顺/雪球拿不到的

- **聚宽因子库 / Alpha101 / Alpha191 官方实现**（SDK 内置计算模块，但注意：alpha101/191 本质是基于行情的本地计算，拿到分钟线后可自行复现）。
- **分钟级历史 K 线长周期**（东财/akshare 分钟线通常限近几日；JQData 分钟线历史长且稳定——这是它对我们最有价值的一点）。
- **tick/秒级历史**（付费项）。
- **期权数据**（50ETF 等期权行情/合约信息）。
- **CNE5/CNE6 风险模型**（机构向，付费）。
- **清洗质量**：复权/停牌处理号称经自营资管验证，比爬虫源稳定。

对我们这个 A 股终端：分钟线历史与清洗质量是主要吸引力；因子可本地算，不是独占壁垒。

## 4. 集成建议

1. **做成可选 provider，与 Tushare token 同模式**：设置页增加"聚宽 JQData"区块，用户自行申请试用账号后粘贴 用户名/密码（或 token）。无凭证时该 provider 整体禁用，不影响现有源。
2. **实现路径用 Python sidecar，不用 Rust 直连**：随应用带（或要求用户安装）一个极简 Python 进程 `pip install jqdatasdk`，通过 stdin/stdout 或本地 HTTP 暴露 `get_price` / `get_query_count` 等少数接口。理由：
   - thrift+msgpack+zlib+pickle 协议栈在 Rust 重写工作量大且脆弱（pickle 布局随服务端 pandas 版本变化）；
   - 官方 SDK 随服务端协议演进自动升级，sidecar 方案零维护；
   - 试用账号单连接限制下，sidecar 天然串行化请求。
3. **Rust 可直接白嫖的部分**：`POST https://dataapi.joinquant.com/v2/apis`（`get_current_token` + `get_current_ticks2`）协议简单已验证，若只要"实时 tick 快照"可纯 Rust 实现；但历史数据仍绕不开 thrift 通道，所以单独做这一条意义有限。
4. **不做的部分**：不要逆向 `www.joinquant.com/api/*` 网页内部 API（需登录态、无文档、易失效）。
5. **优先级**：低。现有 东财/同花顺/雪球 免费源已覆盖日线与实时行情；JQData 等待"有用户持账号且需要分钟级历史/高质量清洗数据"时再投入。

### 附：关键源码事实索引

- 默认 thrift 服务器：`client.py:50-51`（`39.107.190.114:7000`）
- HTTP 端点常量：`client.py:42`（`https://dataapi.joinquant.com/v2/apis`）
- thrift IDL：`thriftclient.py:4-32`
- 认证调用（含 MAC 上报）：`client.py:217-224`
- 请求/响应编解码（msgpack + zlib + pickle）：`client.py:284-320`
- token 获取（HTTP）：`client.py:509-525`
- 实时 tick（HTTP）：`api.py:762-801`
- 配额查询：`api.py:901-912`
