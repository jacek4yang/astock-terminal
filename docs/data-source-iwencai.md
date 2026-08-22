# iwencai (同花顺问财) 接入验证 — `crates/wencai` spike 报告

日期:2026-08-21/22 · 执行者:子代理(问财验证,任务 29) · 范围:仅 `crates/wencai/`

> 关联文档:`docs/data-source-ths-xueqiu.md`(此前结论:问财即使带合法 hexin-v 也弹验证码,标记 not viable)。本次 spike 把"验证码"这最后一环拆开看清,并交付可工作的 Rust 子集。

## TL;DR

| 环节 | 状态 | 证据 |
|---|---|---|
| hexin-v 签名(无 node,内嵌 QuickJS) | ✅ 完全可用 | 单测通过;token 被服务端接受 |
| 问财 search 端点存活 | ✅ | 实测返回业务 JSON(验证码挑战) |
| 验证码类型 | 📌 **滑块拼图**,非文字图 | 抓取验证页 JS 实证(captcha.min.js v1.4,`captcha_type=4`) |
| 滑块协议逆向 | ✅ 完整 | getPreHandle → getImg → getTicket → /ac_verification/check |
| 缺口识别 | ✅ 自研边界连续性算法 | 2 组实拍样本 live 验证被 getTicket 接受(拿到真实 ticket) |
| ddddocr(-tract) | ❌ 精度不足,已弃用 | 实拍样本上缺口定位 x=102,真值≈155(见 §2.3) |
| 过墙后取数 | ⛔ WAF IP 封禁(确认) | check code:0 后 robot-data 仍 "Nginx forbidden",连续 3 次复现,封禁 ~55 分钟(见 §4) |
| **官方 OpenAPI(推荐替代)** | ✅ 见 `docs/niuone-analysis.md` | `openapi.iwencai.com`,Bearer key,**无验证码无 JS 挑战** |

**交付**:crate 完整实现全链路(hexin-v → 查询 → 滑块自动求解 → 重试),应用层全部实测通过;最后一公里被 WAF 的 IP 信誉/指纹风控封死(连续 3 次复现)。**网页版问财纯 HTTP 路径到此封顶**;生产用途应走官方 OpenAPI(见 §7)。

## 1. hexin-v:Rust 自包含签名(成功)

- akshare `akshare/data/ths.js`(39 KB,2019 逆向,硬编码 `TOKEN_SERVER_TIME`)内嵌(`src/ths.js`,`include_str!`)。
- `rquickjs 0.12`(QuickJS-ng,纯 C,MSVC 直接编译,无外部依赖)求值 `v()`。
- **坑**:rquickjs 0.12 的 `eval` 默认 `EvalOptions { strict: true }`;ths.js 是 sloppy-mode 混淆代码(函数体内隐式全局赋值),严格模式抛 `ReferenceError: BROWSER_LIST is not defined`。解法:`eval_with_options` + `options.strict = false`(`src/hexin.rs`)。
- token 形态:~48 字符 URL-safe base64,每次不同;与 node 对照同形态且被接受。
- **会话语义(重要)**:浏览器中 chameleon JS 每会话只算一次 `v` cookie 并复用;服务端把验证码通过与(IP + v cookie)绑定。因此 `WencaiClient` 缓存一个会话级 token,不在每个请求重新生成(实测:check 与 retry 用不同 v 会触发封禁链)。

## 2. 验证码:滑块拼图(关键发现)

### 2.1 协议(全部实测)

1. `POST /customized/chart/get-robot-data`(带合法 hexin-v)→
   `{"code":0,"data":{"captcha_url":"http://www.iwencai.com/ac_verification/captcha/?host=..."}}`
2. 验证页加载 `captcha.min.js` v1.4 控件,`barText:"向右拖动滑块填充拼图"`。
3. `GET captcha.10jqka.com.cn/getPreHandle?captcha_type=4&appid=souniu_fight_spider&random=…&callback=PreHandle`(JSONP)
   → `{sign, urlParams, imgs:[背景id, 切片id], initx, inity}`(实测返回)
4. `GET getImg?{urlParams}&iuk={id}` → 背景 340×195 JPEG(带拼图缺口)+ 切片 ~55×55 RGBA PNG(已存 fixtures)
5. `GET getTicket?{urlParams}&phrase={x};{inity};{宽};{高}&callback=verify` → `{"code":0,"ticket":"…"}`(实测拿到 ticket ×2)
6. `POST www.iwencai.com/ac_verification/check` 表单 `{ticket, phrase, signature, captcha_type:4}` → `code:0` 通过 / `1003` 答错(实测 code:0 ×2)

### 2.2 几何换算(从控件源码推出并实测确认)

页面以 `width:280` 初始化 → `scale=280/340`,显示高 `280/340*195≈160.588`;phrase 中 x 与 inity 均为**显示坐标**(图像坐标×scale)。实测:显示坐标 phrase 拿到 ticket;图像坐标 phrase 被拒 `{"code":-1,"msg":"Phrase Error."}`。此版本控件**不上报鼠标轨迹**。

### 2.3 缺口识别:ddddocr 评估与弃用

| 候选 | 结论 |
|---|---|
| `ddddocr 0.1.0`(mzdk100,ort/ONNX Runtime) | 弃用:需下载几十 MB 原生二进制;且问财不是文字验证码,OCR 无用武之地 |
| `ddddocr-tract 0.1.0`(纯 Rust tract) | 弃用:`slide_match`(canny+模板匹配)在实拍样本上定位 x=102,**真值 x≈155**(天空/云层背景干扰边缘匹配);getTicket 验证拒绝 |
| **自研边界连续性匹配**(`src/captcha.rs::find_gap`) | ✅ 采用。真值位置处切片边缘像素(原图内容)与缺口外侧背景像素颜色连续:以边界两侧 RGB 均差为评分取最小,且 y 搜索限制在服务端给的 inity ±3px。同一实拍样本得分 x=157,另两组 live 挑战 x=176/163,均被 getTicket 接受。依赖仅 `image`(jpeg+png) |

附益:弃用 ddddocr-tract 后 `captcha` feature 的二进制增量从(预计)数 MB 降到 **+438 KB**(release 实测)。

## 3. crate 结构(`crates/wencai`,package `astock-wencai`)

```
src/lib.rs       公共导出(WencaiClient/WencaiError/WencaiResult/WencaiRow/hexin_v)
src/error.rs     Network/Js/NeedCaptcha/CaptchaFailed/RateLimited/Parse
src/hexin.rs     hexin-v:内嵌 ths.js + QuickJS(强制 sloppy mode)
src/ths.js       akshare 原版 39 KB(未改动)
src/pace.rs      令牌桶限流:burst 3,之后 1 req/2s
src/wencai.rs    客户端:会话级 v token + cookie jar + 查询/重试/解析
src/captcha.rs   [feature=captcha] 滑块协议 + 边界连续性缺口识别(+1.5s 拟人拖动延时)
tests/live.rs    #[ignore] 实测 search("连续3天换手率大于5%的主板股票")
tests/fixtures/  解析 fixtures(合成,内有 _comment 注明)+ 实拍滑块图对
```

- 解析覆盖 pywencai 两条路径:`components[*].data.datas` 直出行;`xuangu_tableV1` → `gateway/urp/v7/landing/getDataList`。**解析 fixtures 为合成数据**(按 pywencai `convert.py` 结构构造)。
- "Nginx forbidden"(WAF 封禁,HTTP 200 + HTML)→ 映射为 `RateLimited{status:200}`。
- workspace 内无其他 crate 依赖本 crate;应用完全可在没有它的情况下工作。

## 4. 实测记录(2026-08-22,本机,无代理)

**成功的部分**:

```
getPreHandle → {"code":0,"data":{"sign":"667675e9…","imgs":["1a4c08be…","a4928f2f…"],"inity":33}}   ✅
getImg ×2    → 340×195 JPEG + 55×68 RGBA PNG                                                   ✅
边界连续性缺口定位 → x=176(图像坐标)→ phrase="144.94…;27.16…;280;160.588…"                  ✅
getTicket    → verify({"code":0,"msg":"ok","ticket":"415d475e8d80b0916747747a68655b80"})        ✅
/ac_verification/check → {"msg":null,"data":null,"code":0}                                     ✅(第二轮同样成功)
```

**被墙的部分(最终结论,3 次独立复现)**:

```
check 成功后立即 POST get-robot-data → <h1>Nginx forbidden.</h1>(HTTP 200)                      ⛔
```

- 三次独立求解序列(curl ×2 + Rust 客户端 ×1,横跨 IPv4/IPv6)全部以 check `code:0` 成功、随后 robot-data 被 Nginx 层封禁告终。封禁按 IP+协议栈维度,时长约 55 分钟(IPv6 封于 ~10:35,~11:29 解封;每次求解序列后再次被封)。
- 已排除的因素:hexin-v 合法性(被封前同样请求能拿到验证码挑战)、phrase 正确性(ticket 已拿到)、check 本身(code:0)、会话 v cookie 稳定性(Rust 客户端全程同一 v,仍被封)、请求头完备性(末轮已带 sec-ch-ua/Sec-Fetch-*/Accept-Language 全套)。
- **根因判定**:应用层(验证码)已被我们完整攻破,拦截发生在 WAF 层——它按 IP 信誉(机房/家宽 IDC 段)+ 客户端指纹(rustls/curl 的 TLS 栈均非 Chrome)对"验证码通过后的脚本客户端"做二次风控。这与 `data-source-ths-xueqiu.md` 的原结论一致:网页版问财需要真实浏览器(Playwright + 住宅 IP),纯 HTTP 到此封顶。
- 若未来要强行走通:需要 Chrome TLS 指纹(如 boringssl impersonate 类客户端)+ 住宅代理 IP,投入产出不合理,不推荐。

## 5. 成本测量(release,本机 rustc 1.95.0)

| 项 | 默认(无 feature) | +captcha |
|---|---|---|
| 依赖数(含传递) | 160 | 173(+13,image/png/jpeg 解码) |
| 单元测试 exe | 2.18 MB | 2.62 MB(**+438 KB**) |
| 外部运行时文件 | 无(ths.js 内嵌) | 无(无需任何模型) |
| 依赖编译(增量实测) | rquickjs-sys C 编译 ~30s | +image 系 ~25s |

注:曾集成 ddddocr-tract(tract-onnx 0.21)评估,编译 ~2m09s、依赖 +60 余个 crate;因精度问题弃用后上述成本消失。另外 `kstring` 需固定 2.0.2(2.0.3+ 要求 rustc 1.96)——弃用 tract 后该约束随之消失,`cargo update -p kstring --precise 2.0.2` 的 lockfile 变更可回退。

## 6. 集成建议

- **首选官方 OpenAPI(§7)**;本 crate 定位为研究产物/备用路径。
- `captcha` feature 默认关闭;即便开启,问财对机房 IP 的 WAF 风控也意味着该 provider 只能 **best-effort**:UI 层对 `NeedCaptcha`/`CaptchaFailed`/`RateLimited` 统一降级为"问财暂不可用"。
- 若未来在住宅 IP / 带浏览器指纹的环境部署,本 crate 的全链路(含滑块)有真实通过记录,可直接启用。
- 频次:盘前/盘后低频;crate 内置 burst 3 + 1 req/2s;不做盘中轮询。
- 若服务端加轨迹校验,`captcha.rs` 的 phrase 生成处是注入拟人轨迹的扩展点。

## 7. 推荐替代:问财官方 OpenAPI

调研(`docs/niuone-analysis.md`)确认同花顺官方提供 **`openapi.iwencai.com`**:`Authorization: Bearer {API_KEY}`,纯 JSON POST,**无验证码、无 JS 挑战、无 hexin-v**。需要自然语言选股/问财数据时,优先走该通道(申请 key),而不是本 crate 的网页逆向路径。本 crate 作为网页协议的可行性与风控边界的研究产物保留:hexin-v QuickJS 签名与滑块求解器均已实测可用,若未来环境(住宅 IP/真实指纹)变化可直接启用。

## 8. 复现命令

```sh
cargo test -p astock-wencai                                        # 默认 feature,7 个单测
cargo test -p astock-wencai --features captcha                     # +滑块,9 个单测
cargo test -p astock-wencai --features captcha --test live -- --ignored --nocapture   # 实测
cargo clippy -p astock-wencai --all-targets --no-deps -- -D warnings                  # 两种 feature 均绿
```
