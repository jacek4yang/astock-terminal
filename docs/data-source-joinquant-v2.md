# 聚宽 (JoinQuant) 自动化接入调研 v2 —— 网页登录 + 研究环境 Jupyter 通道

调研日期:2026-08-22(UTC)。调研方式:curl + Python(websocket-client)实测,使用用户本人已授权的聚宽账号(<手机号>)。本文与 `data-source-joinquant.md`(v1,JQData/jqdatasdk 通道)互补:v1 结论是"jqdatasdk thrift 通道不值得在 Rust 重造";v2 找到一条**纯 HTTP/WebSocket、无浏览器、无 pickle** 的新通道 —— 官网网页登录 → 研究环境(JoinQuant Research,实为 JupyterHub)→ 在远端内核里执行聚宽研究 API 并取回 JSON。

## 结论速览

- **网页登录极简单**:单个表单 POST `username`/`pwd`,**密码明文走 HTTPS,无前端加密/无 RSA**。实测登录成功(code `00000`)。
- **验证码是自研拼图滑块**(非极验/网易云盾):仅当登录接口返回 code `105` 时触发(本次实测多次登录均未触发)。验证接口**只提交一个 axisX(缺口 x 距离),无轨迹上传**,与项目 `crates/captcha` 的 `solve_slider` 能力精确匹配。
- **研究环境 = JupyterHub 0.8.1 + 经典 notebook server 5.4.1**,全部挂在 `www.joinquant.com` 同域路径下,标准 Jupyter REST API(`/api/contents`、`/api/kernels` 等)带 cookie 即可用,**代码执行走 WebSocket `/api/kernels/<id>/channels`(Jupyter 消息协议),已端到端实测跑通**。可以在远端内核里直接调用聚宽研究 API(`get_price` / `get_fundamentals` / `get_index_stocks` / 宏观库等,命名空间预装,无需 import),把结果以 JSON 打印回传 —— **不需要 JQData 权限、不消耗 JQData 配额**。实测取到:平安银行日线至 2026-08-21(前一交易日)、沪深300成分 300 只、全市场股票 5211 只、估值快照、宏观 CPI 至 2026-07。
- **ws 握手有一个确定的坑**:请求头**带 `Origin` 会被 openresty 立即 502(确定性,A/B 各 3 次实测 100% 复现);不带 Origin 则秒连(101)**。tokio-tungstenite 默认不发 Origin,只要别手动加即可。
- **研究环境的数据"当前日期"上下文停在 2015-12-31**:`get_price` 不显式传 `start_date`/`end_date` 会默认回到 2015 年底。**客户端模板必须始终显式传日期**。
- **该实测账号未开通 JQData**(`get_current_token` 返回"未开通权限"),v1 文档所述 thrift 通道对此账号不可用;研究环境通道不受影响。
- **网页端数据页没有可依赖的 XHR**:`/api/valuation` 带登录 cookie 仍返回"缺失必要的通用参数"(非登录态问题,疑似签名参数);`/data/dict/*` 是静态数据字典文档页。数据获取应走研究环境内核,不要逆向网页内部 API。
- **主要工程风险**:研究实例会被回收(culling),每次会话要检查 spawn 状态;内核是 **Python 3.6.7**,注入代码要兼容 3.6;自动化使用研究环境需低频克制(§4.6)。

---

## 1. 登录流程(全部实测)

### 1.1 登录页与表单

- 登录入口:`GET https://www.joinquant.com/user/login/index`(旧路径 `/view/login`、`/login` 均 302 到 `/default/index/404`,勿用)。未登录访问 `/research` 会 302 到 `/user/login/index?redirect=...`。
- 页面为 JS 渲染,关键 bundle:`https://cdn.joinquant.com/std/dist/modules/user/login/index.bundle.js`(登录页)与 `.../general/login-dialog.bundle.js`(全站登录弹窗)。
- **两套字段名,别搞混**(本次实测踩坑):
  - 登录页(index.bundle.js)表单字段:`username` / `pwd` —— **真实可用**;
  - 登录弹窗(login-dialog.bundle.js)字段:`CyLoginForm[username]` / `CyLoginForm[pwd]` —— 用这套会得到"用户不存在或密码错误"。
- 前端只对手机号做正则校验,**密码不做任何哈希/加密**,`form.serialize()` 原样 POST(HTTPS 传输)。

### 1.2 登录接口

```
POST https://www.joinquant.com/user/login/doLoginByText
Content-Type: application/x-www-form-urlencoded
X-Requested-With: XMLHttpRequest          (建议带,未验证是否必需)

username=<手机号>&pwd=<明文密码>
```

响应 JSON:

| code | 含义 | 处理 |
|---|---|---|
| `"00000"` | 成功。`data.redirect`(登录后跳转)、`data.user{user,mobile,nickName}` | 收 cookie,完成 |
| `"10000"` | 字段级错误,`error{}` | 提示用户检查输入 |
| `"20000"` | 通用失败,`msg` 如"用户不存在或密码错误"(用户不存在与密码错误**同文案**,无枚举 oracle) | 提示凭证错误 |
| `105` | **需要滑块验证** | 走 §1.4 流程后带 `valideCode` 重放 |

成功响应 `Set-Cookie: PHPSESSID=<26位>; HttpOnly`,并另有 `token=<40位hex>; HttpOnly`(实测登录后 cookie jar 中出现)。

实测记录(2026-08-22):`{"data":{"redirect":"/default/research/index?...","user":{"user":"847392","mobile":"<手机号>","nickName":"jacek4yang"}},"status":"0","code":"00000"}`。

### 1.3 登录后的 cookie 形态与有效期

| Cookie | 属性 | 有效期 | 用途 |
|---|---|---|---|
| `PHPSESSID` | HttpOnly,path=/ | **会话级**(服务端 TTL 未知,未做长周期验证) | 官网会话;同时是通往 JupyterHub 的"接力令牌"(§2.1) |
| `token` | HttpOnly,path=/ | 会话级,40 hex | 官网登录态 |
| `uid` | path=/ | 到 2037 年 | 设备指纹,登录前即有 |
| `_xsrf` | path=/ | 约 30 天 | notebook REST 写操作的 XSRF 令牌(§2.3) |
| `jupyter-hub-token` | HttpOnly,path=/hub | **约 30 天**(实测 expires) | JupyterHub 认证 |
| `user-<内部ID>` | HttpOnly,path=/user/<ID> | 约 30 天 | 单用户 notebook server 认证 |

会话探活:`GET /user/index/isLogin` → `{"data":{"isLogin":1,"userId":"...","userName":"<手机号>","alias":"jacek4yang",...},"code":"00000"}`(`isLogin:0` 即失效,重新登录)。

### 1.4 验证码(拼图滑块,自研)

触发链(JS 逆向自 index.bundle.js,接口均实测):

1. 登录返回 code `105` → 前端弹滑块。
2. `POST /common/verifyCode/captchar`(无参数)→ `data`:
   - `bgImg`:data-URI base64 **背景图**(带缺口,实测 363×142 PNG,约 110 KB base64);
   - `hqImg`:data-URI base64 **滑块拼图块**(约 56×56 PNG,带 alpha);
   - `axisY`:缺口 y 坐标;`bgImgW/bgImgH/blockW/blockH` 尺寸;`point`:前端拖动动画用的精灵图偏移数组(66 个,与求解无关)。
3. 求解缺口 x 距离后:`POST /common/verifyCode/validate`,**唯一参数 `axisX=<距离>`**。响应 `data:{result:bool, token, action}`。
   - **关键:服务端只收 axisX 一个标量,不上传拖动轨迹、不校验时间曲线**。失败可重新 `captchar` 再来。
4. `result=true` 拿到 `token` → 重新 POST `doLoginByText`,附加 `valideCode=<token>`,即完成登录。

与 `crates/captcha` 的对接:`solve_slider(background=bgImg, piece=Some(hqImg))` 走 alpha-mask 模板匹配(`detect_gap_with_template`),返回的 `distance` 即 `axisX` 候选;`LowConfidence` 或 `result=false` 时刷新 captchar 重试(bounded retries,与 crate 文档的约定一致)。`trajectory` 模块当前用不上(无轨迹上传),保留以应对服务端将来加行为校验。

相关辅助端点(注册/短信场景,本次未深入):`/common/verifyCode/valideCode?width=110`(字符图形验证码图片)、`/common/verifyCode/sendSMS`、`/user/login/doLoginByCode`(短信验证码登录,可作为密码失效时的备选人工通道)。

## 2. 研究环境(JoinQuant Research)= 可编程 Jupyter

### 2.1 认证接力链(全部实测)

```
已登录 cookie(PHPSESSID/token)
  │  GET /research  →  302 → /user/login/index?redirect=...(未登录时)
  │  GET /research  →  200,页面 iframe 到 /default/research/redirect
  ▼
GET /default/research/redirect  →  200,内嵌桥接页,含两段关键数据:
    var mob = "29005631157";            ← 内部用户 ID(非手机号!)
    var sessionId = "<PHPSESSID 同值>";
    Cy.postRedirect('https://www.joinquant.com/hub/login?next=%26url%3D',
                    {username: mob, token: sessionId});
  ▼
POST /hub/login   (application/x-www-form-urlencoded: username=<mob>, token=<PHPSESSID>)
  → 302 /hub/,Set-Cookie: jupyter-hub-token=...(HttpOnly, path=/hub, ~30天)
  → 响应头 x-jupyterhub-version: 0.8.1
  ▼
GET /hub/  → 302 /user/<mob>/   → 再 302 /hub/user/<mob>/ 触发 spawn
  (spawn 页面 200;实测约 10 秒就绪)
  ▼
GET /user/<mob>/api  → 200 {"version":"5.4.1"}   ← 单用户 notebook server 就绪
```

要点:`mob` 是聚宽内部数字 ID(本账号实测 `29005631157`),每次从桥接页解析,不要硬编码;Hub 登录令牌就是 `PHPSESSID` 的值。

### 2.2 Jupyter REST API(实测可用)

基址:`https://www.joinquant.com/user/29005631157`(= `/user/<mob>`)

| 端点 | 实测结果 |
|---|---|
| `GET /api` | `{"version":"5.4.1"}`(经典 notebook server,非 JupyterLab) |
| `GET /api/contents` | 研究目录文件列表(新手引导.ipynb 等),可读/写 |
| `GET /api/kernelspecs` | `python3`(默认)、`python2`、`python2new` |
| `GET /api/sessions` / `GET /api/kernels` | 正常返回 |
| `POST /api/kernels` `{"name":"python3"}` | 201,创建内核(见 §2.3 的 XSRF 要求) |
| `GET /tree` | 页面,同时种下 `_xsrf` cookie |

### 2.3 XSRF 与写操作

- notebook server 5.4.1 对所有 POST 校验 XSRF:**先 GET 一次 `/user/<mob>/tree` 拿 `_xsrf` cookie**,之后每个 POST 带请求头 `X-XSRFToken: <_xsrf 值>`。
- 不带时实测:403 `{"message":"'_xsrf' argument missing from POST"}`。
- 带上后实测:`POST /api/kernels` → 201 `{"id":"94ba68b3-...","name":"python3","execution_state":"starting"}`。

### 2.4 代码执行通道(WebSocket,Jupyter 消息协议)—— 已端到端实测

- 端点:`wss://www.joinquant.com/user/<mob>/api/kernels/<kernel_id>/channels?session_id=<任意uuid>`。
- 认证:同域 Cookie(PHPSESSID/token/jupyter-hub-token/user-<mob> 全套)。
- **握手的确定性坑(实测 A/B 各 3 次,100% 复现)**:
  - 请求头**显式带 `Origin: https://www.joinquant.com` → openresty 立即 `502 Bad Gateway`(约 0.2s)**;
  - **不带 Origin → 立即 `101 Switching Protocols`(约 0.2s),协议交互稳定**。
  - (Python websocket-client 会自动从 URL 推导并发送 Origin,用它时必须 `suppress_origin=True`;curl / 裸 socket / tokio-tungstenite 默认不发 Origin,天然没问题。**Rust 侧唯一要做的是:别手动加 Origin 头。**)
- 协议:标准 Jupyter wire protocol(v5.x):发送 `execute_request`(channel=shell,`content.code` 为 Python 源码,`msg_id`=uuid),在 iopub 上按 `parent_header.msg_id` 过滤收 `stream`(stdout)/`execute_result`/`error`,以 `status: idle` 判定执行结束。
- 实测:连接后执行 `print('hello'); import sys; print(sys.version)` 正常回显,内核为 **Python 3.6.7**。

### 2.5 在研究内核里取数(已实测)

研究环境内核命名空间**预装聚宽研究 API**(`get_price`、`get_fundamentals`、`get_index_stocks`、`get_all_securities`、`query`、`valuation` 等,无需 import);`jqdata` 模块另有 `finance`、`macro`、`get_all_trade_days`、`get_concepts` 等。研究环境数据调用**不消耗 JQData 配额**(本账号未开通 JQData 仍全部可用)。实测记录:

| 调用 | 实测结果 |
|---|---|
| `get_price('000001.XSHE', start_date='2026-08-10', end_date='2026-08-21', frequency='daily')` | 10 个交易日全量返回,数据到 **2026-08-21**(前一交易日) |
| `get_price(..., count=3)` 不传日期 | **默认上下文日期停在 2015-12-31**,返回 2015 年末数据 —— 模板必须显式传日期 |
| `get_price(..., start='2025-01-01', end='2025-12-31')` | 243 个交易日,完整 |
| `get_index_stocks('000300.XSHG')` | 300 只成分 |
| `get_all_securities(types=['stock'], date='2026-08-20')` | 5211 只 |
| `get_fundamentals(query(valuation...), date='2026-08-20')` | 平安银行 PE 5.09、总市值 2212 亿 |
| `jqdata.get_all_trade_days()` | 交易日历(延伸至 2028 年,为未来含预估) |
| `from jqdata import macro; macro.run_query(query(macro.MAC_CPI_MONTH)...)` | 宏观库可用,表名前缀是 **`MAC_`**(非旧文档的 `MACRO_`),CPI 月度数据到 2026-07 |

取数回传模式:把结果以带前缀的单行 JSON 打到 stdout,客户端在 iopub `stream` 消息里按前缀截取:

```python
# 注入内核执行(注意兼容 Python 3.6)
import json
df = get_price('000300.XSHG', start_date='2026-08-01', end_date='2026-08-21',
               frequency='daily', fields=['open','high','low','close','volume'])
print('JQJSON:' + json.dumps(df.reset_index().astype(str).to_dict('records'), ensure_ascii=False))
```

大数据量分批,或内核写文件后走 `GET /api/contents/<path>?format=text` 拉取。stdout 文本含中文时注意:内核 stdout 编码可能把中文打成乱码(实测宏观表 area_name 乱码),**涉及中文的字段在内核侧先做 `codecs`/`unicode_escape` 或 base64 包装**,客户端解码。

## 3. 网页端数据页 XHR(结论:不可依赖)

带完整登录 cookie 实测:

| URL | 结果 |
|---|---|
| `/api/valuation` | `{"code":4,"msg":"缺失必要的通用参数"}` —— 与匿名相同。"通用参数"不是登录态,疑似签名/内部参数,未逆向、不建议逆向 |
| `/index/valuation`、`/macro/index` | 302 跳转(页面已迁移/下线) |
| `/data/dict/indexData` | 200,126 KB **服务端渲染的数据字典文档页**(静态文档,非数据接口) |
| `/data`、`/view/community/list` | 200,产品页/社区页,无数据 XHR |
| 首页 `/` | 200,仅 800 B 的 iframe 壳(指向 `fund.joinquant.com` 基金站) |

聚宽官网没有"指数成分/财务/宏观"的数据浏览页;这些数据只在研究环境 API 与 JQData 里。网页内部 API 无文档、带签名、易失效,**不作为依赖**。

## 4. 纯 Rust 实现设计(reqwest + tokio-tungstenite,无浏览器)

### 4.1 模块划分(建议 `crates/market-data` 下新增 provider,或独立 `joinquant` 模块)

```
joinquant/
  auth.rs     登录、会话探活、滑块降级
  hub.rs      JupyterHub 接力、spawn 管理
  kernel.rs   内核生命周期 + ws 执行协议
  query.rs    数据查询模板(get_price 等)与 JSON 解析
```

### 4.2 依赖

- `reqwest`(rustls,`cookies` feature)—— cookie jar 自动管理;
- `tokio-tungstenite`(rustls)—— ws 执行通道;
- `image` + `base64` —— 解码 captchar 的 data-URI,喂给 `astock-captcha`;
- `astock-captcha`(项目已有,`crates/captcha`)—— `solve_slider`;
- `serde_json`;正则(或手写扫描)解析桥接页的 `mob`/`sessionId`。

### 4.3 登录状态机

```
ensure_session():
  if jar 有 PHPSESSID 且 GET /user/index/isLogin 返回 isLogin==1 → 复用
  else login()

login():
  POST /user/login/doLoginByText {username, pwd}
  match code:
    "00000" → 成功
    105     → captcha_loop():
                最多 N=5 次:
                  POST /common/verifyCode/captchar
                  解 base64 → DynamicImage bg / piece
                  astock_captcha::solve_slider(bg, Some(piece))
                    Err(LowConfidence) → continue(刷新重试)
                  POST /common/verifyCode/validate {axisX: distance}
                  result==true → 带 valideCode=token 重放 doLoginByText
                N 次仍败 → 报错,降级提示用户到设置页手动处理
    其他    → 返回错误文案
```

注意点:
- 字段名用 `username`/`pwd`(登录页那套),不要用 `CyLoginForm[...]`;
- `validate` 只需 axisX;若实测发现 axisX 语义有偏移(缺口左边缘 vs 滑块起点),用"distance ± 滑块初始偏移"校准,失败自动刷新重试即可收敛;
- 全程统一桌面 Chrome UA、显式 `X-Requested-With: XMLHttpRequest`。

### 4.4 Hub 接力与 spawn 管理

```
ensure_kernel():
  GET /default/research/redirect
    正则取 mob("var mob = \"(\d+)\"")与 sessionId
  POST /hub/login {username: mob, token: sessionId}   (允许重定向,收 jupyter-hub-token)
  GET /hub/ → 跟到 /user/<mob>/(触发 spawn)
  轮询 GET /user/<mob>/api,间隔 2s,超时 60s,直到 200
  GET /user/<mob>/tree                          (拿 _xsrf)
  kernels = GET /api/kernels
  有空闲 python3 内核则复用,否则 POST /api/kernels {"name":"python3"}(带 X-XSRFToken)
```

研究实例会被回收(无活动 culling):每次取数前 `GET /user/<mob>/api` 探活,302/超时则重走 spawn。实例冷启动实测约 10s。

### 4.5 ws 执行通道

- `tokio-tungstenite::connect_async`,请求头注入完整 Cookie(含 HttpOnly 各值,从 jar 拼装),URL 带 `session_id=<uuid>`。**不要设置 `Origin` 头**(带了会被 openresty 确定性 502,见 §2.4);tungstenite 默认不发,保持默认即可。
- 握手失败仍做兜底重试(网络抖动/服务端重启):每次尝试 12s 超时,指数退避 1s→2s→4s…上限 30s;成功后保持单连接复用,断线才重连。
- 协议:构造 `execute_request`(header.msg_type / content.code / msg_id=uuid);收帧过滤 `parent_header.msg_id == 本请求`;聚合 iopub 的 `stream.text`;`error` 取 traceback;`status==idle` 结束。shell 的 `execute_reply` 作最终状态。
- 单次执行设总超时(如 120s),超时 `DELETE /api/kernels/<id>` 后重建。
- 注入代码必须兼容 **Python 3.6**(内核实测 3.6.7):不用 3.8+ 语法(海象运算符、f-string `=` 说明符等)。
- 输出协议:内核代码把结果打印为 `JQJSON:<json>` 行;客户端按行扫描。**中文字段在内核侧 base64 或 `unicode_escape` 包装**(内核 stdout 非 UTF-8,直接 print 中文会乱码,已实测)。大于 ~1 MB 的结果改走"内核写文件 → `GET /api/contents/<path>?format=text` 拉取 → 删除文件"。

### 4.6 限流、反爬与合规

- 登录接口有频率保护("操作太频繁了~"),登录失败重试至少间隔 5-10s;正常情况下**登录一次、复用会话**,不要每次取数都登录。
- 研究环境面向交互式使用,自动化批量取数需克制:单内核、串行请求、秒级间隔;高频/批量抓取有封号风险,应作为用户自有账号的可选增强源,并在设置页明示风险。
- 所有凭证只存本地(与 Tushare token 同模式),不上报。

### 4.7 字段映射(研究 API → 终端内部模型,要点)

| 聚宽研究 API | 返回 | 终端模型 |
|---|---|---|
| `get_price(security, start_date, end_date, frequency='daily', fq='pre', fields)`(**必须显式传日期**,默认上下文停在 2015-12-31) | DataFrame,index=date | `DailyBar{date, open, high, low, close, volume, money}`;证券代码 `000300.XSHG`→`SH000300` 式内部码(XSHG→SH,XSHE→SZ) |
| `get_price(..., frequency='1m')` | 分钟线 DataFrame | `MinuteBar` |
| `get_index_stocks('000300.XSHG', date)` | 代码列表 | 指数成分 |
| `get_fundamentals(query(valuation/indicator/...), date)` | 财务/估值 DataFrame | 基本面快照(PIT 用 `statDate`/`pubDate` 字段) |
| `get_all_securities(types=['stock'], date)` | 全市场证券表 | 证券主数据(实测 5211 只 A 股) |
| `jqdata.get_all_trade_days()` | 日期数组 | 交易日历 |
| `from jqdata import macro; macro.run_query(query(macro.MAC_*))` | 宏观 DataFrame(表名前缀 `MAC_`,如 `MAC_CPI_MONTH`) | 宏观序列 |

### 4.8 与 v1 结论的关系

- JQData HTTP token 通道(`dataapi.joinquant.com/v2/apis`)实测确认:**本账号未开通 JQData**("未开通权限";顺带证实 jqdatasdk 的 `pwd` 字段要 URL 编码,双编码会报"用户不存在或密码错误")。
- 若用户日后开通 JQData 免费试用(1 年、100 万条/天,需人工申请),thrift 通道仍按 v1 结论走 Python sidecar;本 v2 通道**不需要 JQData 权限**,优先级更高。
- 研究环境通道的产出是"借聚宽服务器执行官方研究 API",数据口径与 JQData 一致,但执行环境有资源/时长限制,不适合大批量历史回灌;日线级、因子快照级拉取完全够用。

## 5. 风险汇总

| 风险 | 等级 | 缓解 |
|---|---|---|
| ws 握手误带 Origin 头被 502 | 低 | 确定性规则:不发 Origin(§2.4/§4.5) |
| 滑块验证码触发(低频) | 低 | `crates/captcha` 自动求解;失败降级手动 |
| 研究实例被回收 | 中 | 探活 + 自动重 spawn(§4.4) |
| 会话过期 | 低 | isLogin 探活 + 自动重登 |
| 研究环境默认上下文日期=2015 陷阱 | 低 | 查询模板强制显式日期(§2.5) |
| 内核 stdout 中文乱码 | 低 | 中文字段内核侧 base64/escape 包装(§4.5) |
| 违反聚宽服务条款/封号 | 中 | 用户自有账号、低频串行、设置页明示;默认关闭 |
| 服务端接口变更(无公开 API 承诺) | 中 | 全部端点封装在 provider 内,失败可整体降级禁用 |
| 研究环境资源限制(内存/执行时长) | 中 | 分批查询、大结果走文件通道 |

## 6. 结论与建议

1. **可行,且是聚宽系最务实的自动化通道**:纯 reqwest + tungstenite 即可实现"登录 → 研究环境内核执行官方 API → JSON 回传",全程无浏览器、无 thrift/pickle。
2. **做成可选 provider**(与 Tushare 同模式):设置页填聚宽账号密码;无凭证/登录失败时整体禁用,不影响现有免费源。
3. **验证码降级已具备现成能力**:`crates/captcha` 的模板匹配滑块求解与聚宽拼图完全对口;且聚宽 validate 不收轨迹,成功率高。
4. **不要做的**:不要逆向 `/api/*` 网页签名 API;不要高频批量抓研究环境;不要在 Rust 里重造 jqdatasdk thrift 栈(v1 结论不变)。
5. **定位**:研究环境通道适合日线/财务/成分/宏观的按需拉取与策略验证;分钟级大批量历史仍建议东财/akshare 系或用户自开 JQData 后走 sidecar。

### 附:实测事实索引(2026-08-22)

- 登录成功:`POST /user/login/doLoginByText`,字段 `username`/`pwd`,响应 code `"00000"`,user=847392,nickName=jacek4yang
- 错误字段名对照:`CyLoginForm[username]` → code `"20000"` "用户不存在或密码错误"
- 会话探活:`GET /user/index/isLogin` → `isLogin:1`
- 桥接页:`GET /default/research/redirect` → `mob=29005631157`,`sessionId=PHPSESSID`
- Hub 登录:`POST /hub/login` → 302,`x-jupyterhub-version: 0.8.1`,`jupyter-hub-token` 30 天
- spawn:首次访问 `/user/29005631157/` 后约 10s 就绪
- notebook API:`GET /user/29005631157/api` → `5.4.1`;kernelspecs=python3/python2/python2new
- XSRF:无 `X-XSRFToken` → 403 `'_xsrf' argument missing from POST`;带后 `POST /api/kernels` → 201
- ws:`/user/29005631157/api/kernels/<id>/channels?session_id=<uuid>` —— **带 `Origin` 头确定性 502(A/B 各 3 次 100% 复现);不带 Origin 秒连 101**;连接后 `execute_request` 执行 `print`/`get_price` 等均正常返回
- 内核执行:Python **3.6.7**;研究 API 命名空间预装;`get_price` 显式日期取到 2026-08-21;`get_index_stocks('000300.XSHG')`=300;`get_all_securities`=5211;`get_fundamentals(valuation)` 正常;`jqdata.get_all_trade_days()` 正常;`jqdata.macro` 表前缀 `MAC_`,CPI 到 2026-07
- 陷阱:研究环境默认上下文日期=2015-12-31(`get_price` 不传日期返回 2015 年数据);内核 stdout 非 UTF-8,中文需包装
- 验证码:`POST /common/verifyCode/captchar` → bgImg(363×142)/hqImg(~56×56)/axisY;`POST /common/verifyCode/validate {axisX}` → `{result, token}`
- JQData:`get_current_token`(本账号)→ "error: 未开通权限";`pwd` 需单层 URL 编码
- 网页数据 XHR:`/api/valuation` 带 cookie 仍 `{"code":4,"msg":"缺失必要的通用参数"}`
