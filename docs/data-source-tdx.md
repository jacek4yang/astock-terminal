# 通达信 (TDX) 行情 TCP 协议数据源调研报告

调研日期：2026-08-22（本地时间）。调研方式：通读两个开源实现源码 —— Go 版 [oficcejo/tdx-api](https://github.com/oficcejo/tdx-api)（实为 MIT 上游 [injoyai/tdx](https://github.com/injoyai/tdx) 的二开，协议层即上游代码）与 Rust 版 [jiangtaovan/tdxrs](https://github.com/jiangtaovan/tdxrs)（本地克隆于 `../research-tmp/`，HEAD 与 GitHub main 一致），关键结论（license、命令码、服务器列表）已在本地克隆中逐项核对。

## 结论速览

- **协议本身完全可用且简单**：TCP 直连、固定端口 7709，三步固定字节握手，**无账号、无鉴权、无 token**，连上即可查 K线/五档/分时/分笔/股票列表。这是所有免费 A 股源里**唯一不依赖 HTTP/网页接口、无 WAF 风险**的通道。
- **服务器 IP 是唯一的脆弱点**：可用服务器列表来自社区实测收集（tdxrs 内置 101 台、tdx-api 内置 34 台），单台会随时间失效，必须做**启动时并发探测 + 按延迟排序 + 失败切换**。两台以上实测来源交叉验证后，可用性可以接受。
- **tdxrs 是 MIT 协议、核心逻辑 100% 在 Rust、实现完成度高**（K线/五档/分时/逐笔/列表/财务/板块/本地文件解析全有），但**未发布 crates.io、pyo3 是无开关的硬依赖**，直接作 Rust 依赖会把 Python 链接成本拖进 Tauri 构建，**不建议直接依赖**。
- **推荐路径：借鉴 tdxrs 的协议代码内化**（MIT 允许，保留版权声明即可），在 `crates/tdx` 下自建约 2500–3000 行的协议内核 + 在 `crates/market-data` 增加 `TdxProvider` 接入现有 `DataProvider` failover 链。
- **定位：K线/快照/分时的第二通道与独立校验源**，排在腾讯/新浪之后、东财之前（东财仍独占资金流/搜索/F10）。注意 tdx 服务器**只返回未复权原始 K线**（fq 字段为协议保留位，不控复权），复权请求应返回 `NoProvider` 交给链上其他源。

---

## 1. 通达信 TCP 行情协议（源码级细节，已核实）

以下以 tdxrs（Rust，`src/protocol/constants.rs`、`src/net/utils.rs`）与 tdx-api（Go，`protocol/frame.go`、`protocol/const.go`）两份实现交叉核实，两者字节级一致。

### 1.1 连接与握手

- 服务器为裸 TCP，默认端口 **7709**。
- **无登录鉴权**。连接建立后发送三步固定字节序列（tdxrs `src/protocol/constants.rs` ≈L21-38 的 `SETUP_CMD1/2/3` 硬编码字节数组，与 pytdx 一致；Go 侧对应 `TypeConnect=0x000D`、Data=`[0x01]` 的极简连接包），每步读响应（可含 zlib 压缩），握手完成即进入查询状态。连接响应尾部是 GBK 编码的服务器信息串，可忽略。
- **心跳**：服务器无数据约 60 秒踢连接，客户端需每 ~30 秒发心跳包（`TypeHeart=0x0004`，无数据体）。tdxrs 在 `client.rs` 用心跳线程 + 失败自动切换服务器；tdx-api 在 `client.go` `OnConnected` 里 `GoTimerWriter(30s)`。

### 1.2 请求/响应二进制帧格式

**请求帧**（12 字节头 + payload，全小端）：

| 偏移 | 字段 | 说明 |
|---|---|---|
| 0-1 | magic = `0x010C` | 帧头魔数 |
| 2-5 | seq/flags u32 | 消息 ID，客户端自增（Go 侧 `atomic.AddUint32`） |
| 6-7 | zip_len u16 | = len(payload)+2 |
| 8-9 | unzip_len u16 | 与上一字段重复 |
| 10-11 | cmd u16 | 命令码（见 1.3） |
| 12+ | payload | 各命令自定义 |

K线请求 payload 共 26 字节：market u16 + code 6B ASCII + category u16 + fq u16（保留位）+ start u16 + count u16 + 10B 保留（构造见 tdxrs `src/net/utils.rs` `build_security_bars_packet`）。**实时五档是特例**：命令码为 u32、头结构不同（tdxrs `client.rs` ≈L870）。

**响应帧**（16 字节头 + 数据域）：

| 偏移 | 字段 | 说明 |
|---|---|---|
| 0-3 | magic = `0xB1CB7400` | 响应帧头，流式拆包时按它扫描对齐 |
| 4-8 | control + seq | 与请求对应 |
| 10-11 | method u16 | 回显命令码 |
| 12-13 | zip_size u16 | 数据域压缩后长度 |
| 14-15 | unzip_size u16 | 解压后长度；**zip_size ≠ unzip_size 时用 zlib 解压**（Rust 侧 `flate2::read::ZlibDecoder`，Go 侧 `zlib.NewReader`） |
| 16+ | payload | 记录流 |

### 1.3 命令码清单

| 命令码 | 功能 | 备注 |
|---|---|---|
| `0x000D` | 建立连接 | 握手用，无鉴权 |
| `0x0004` | 心跳 | 30s 一次 |
| `0x044E` | 市场证券数量 | 探测服务器活性也用它 |
| `0x0450` | 股票代码列表 | 每次 1000 只，分页拉取 |
| `0x053E` | 实时五档行情快照 | 单次上限 **60 只**（服务端硬限制） |
| `0x052D` | K线（股票+指数同一命令码） | 单次 ≤800 条，分页；指数解析多 4 字节（涨/跌家数）且成交量 ×100 |
| `0x051D` | 当日分时 | **两个实现都标注其价格编码异常**，实际均绕走 0x0FB4 |
| `0x0FB4` | 历史分时 | 实际用它取当日分时 |
| `0x0FC5` | 当日分笔成交 | 单次最多 1800 条 |
| `0x0FB5` | 历史分笔成交 | 单次最多 2000 条，只能查昨日及之前，最早 2000-06-09 |
| `0x0010` | 财务信息 | 34 项原始字段 |
| `0x000F` | 除权除息 (xdxr) | |
| `0x02C5` / `0x06B9` | 板块文件 meta / 内容 | block_zs/fg/gn.dat |

**K线周期 category**（0x052D payload 第 9 字节）：`0`=5分、`1`=15分、`2`=30分、`3`=60分、`4`=日K(变体)、`5`=周、`6`=月、`7`=1分、`8`=1分(变体)、`9`=日、`10`=季、`11`=年。

**市场编码**：`0`=深圳、`1`=上海（`2`=北京为社区扩展，通达信 hq 服务器本身不提供北交所列表，tdx-api 对北交所走爬虫补）。号段自动判市：`6→沪`、`0/30→深`、`510-515→沪基金`、`159→深基金`。

### 1.4 响应记录解码要点（通达信特有编码，实现时最易踩坑）

- **变长价格/整数 `getprice`**：首字节 bit7=续位、bit6=符号、低 6 位有效，后续字节各 7 位（tdx-api `protocol/types_price.go` `GetPrice`；即 pytdx `getprice`）。
- **4 字节类浮点 `getvolume`**：最高字节为指数点，按 `2^(logpoint*2-0x7f)` 拼凑，用于成交量/成交额/昨收（tdx-api `protocol/unit.go` `getVolume`）。
- **K线记录**：2 字节 Count 后，每条 = 4 字节时间（分钟线 `年=(x>>11)+2004` 位运算；日及以上为 YYYYMMDD 整数）+ 4 个 **差分链** getprice（open→close→high→low 依次累加）+ 4B volume + 4B amount。
- **五档记录**：每股 = 1B 市场 + 6B 代码 + 2B 活跃度 + DecodeK + 一串 CutInt（总手/现量/内外盘…）+ 4B 金额 + 5 组买卖档（各 getprice 价差×10 + 2×CutInt 量）。
- **分笔记录**：2 字节时间（距 0 点分钟数）+ 价格差分累加 + CutInt 量 + CutInt 单数 + CutInt 方向（0 买/1 卖/2 中性）。
- **价格单位是"厘"**（1 元 = 1000 厘），分钟线成交量要 ÷100，成交额同为厘。

## 2. 公开行情服务器 IP 列表来源与探测方法

**来源**（均为社区实测收集，无官方公布渠道）：

- **tdxrs**（`src/protocol/constants.rs` ≈L146-320）：双层结构 —— `PRIMARY_SERVERS` 10 台（注释说明跨 IP 段/跨运营商筛选理由，2026-07 两次修正）、`ALL_KNOWN_SERVERS` 101 台全量（标注 33 台探测可达 / 68 台不可达）。这是目前公开仓库里**维护最新、注释最完整**的一份。
- **tdx-api**（`hosts.go`，34 组，注明"2024-11-30 测试通过"）：上海 14 + 北京 7 + 广州 12 + 武汉 1，注释标注华为云/腾讯云/电信归属。
- 两份列表交集不大，合并去重后可得 ~120 台候选，互为备份。

**探测/选路方法**（两个项目一致，可直接借鉴）：

1. **并发 TCP 测速**：每台一个任务并发 `TcpStream::connect`，计时排序（tdx-api `FastHosts`，`hosts.go:84-115`）。
2. **深度探测**（tdxrs `client.rs` ≈L250-310 `probe_servers`）：在 TCP 延迟之上再测**握手延迟**和 **API 延迟**（实际发一个 0x044E 证券数量包），按 API 延迟升序选出最快可用服务器。TCP 能连 ≠ 协议可用，深度探测更可靠。
3. **顺序兜底**：按"上次成功 → 自定义 → PRIMARY → 全量列表"顺序遍历连接（tdxrs `connect_to_any`），失败节点进黑名单（tdxrs v0.6.7 新增 `block_server`）。
4. **运行时再平衡**：心跳失败/请求连续失败时切换到备选服务器。

**稳定性判断**：单台 IP 的生命周期以月计、不可控；但候选池 >100 台、探测自动化之后，作为**非唯一数据源**可用性可接受。绝不能把它做成唯一行情通道——现有腾讯/新浪/东财 HTTP 链仍是主链。

## 3. tdxrs Rust 实现评估

### 3.1 完成度（证据见 tdxrs 仓库）

- **全接口覆盖**：`src/net/client.rs` 实现日/周/月/分钟 K线（12 周期 + 自动分页 + 客户端侧前/后复权）、指数 K线、五档快照（60 只上限）、分时、逐笔、股票列表（带 30s TTL 缓存）、证券数量、财务（34 字段 + `finance_fields.rs` 45 个英文命名指标）、xdxr、板块；`src/fund/` 基金封装、`src/block/` 板块客户端、`src/profile/` F10（feature-gated）、`src/reader/` 本地 .day/.lc1/.lc5 文件解析。
- **工程质量较好**：无 unsafe；thiserror 错误体系 + 中英文错误码；重试退避 `[0.1, 0.5, 1.0, 2.0]s`；分交易时段自适应限流；日K空响应自动换服务器重试；README 称 139 个测试；维护活跃（2026-08 仍有提交）。
- **瑕疵**：当日分时 0x051D 作者自标"价格编码异常"绕走历史接口；北交所支持是残的（auto_market 只认 6/0/3 开头）；部分测试依赖仓库外的 pytdx golden 文件、网络测试硬编码线上 IP；单人项目（2026 年新建，巴士因子=1），API 变动快。

### 3.2 直接作为依赖？——不建议

- **未发布 crates.io**（只发了 PyPI），只能 git dependency，锁定性差。
- **`pyo3 0.28` 是无 feature 开关的硬依赖**（`Cargo.toml:19`，crate-type = cdylib+rlib），纯 Rust 引用会被拖进 Python 链接问题（Windows gnu 还需 MSYS2 dlltool）——对 Tauri v2 桌面构建是不可接受的白付成本。
- 同步阻塞 API 为主（tokio 异步客户端是可选层），与我们的 `#[async_trait] DataProvider` 对接仍需包一层。

### 3.3 内化协议代码（推荐路径）

License 为 **MIT**（`LICENSE`，Copyright (c) 2026 Chiang Tao / tdxrs Contributors），明确允许 copy/modify/sublicense，唯一义务是**在内化代码的文件头或 NOTICE 中保留版权声明与 MIT 声明**。无 copyleft 风险。

最小内化集合（行数为估算，含可裁剪空间）：

| 来源文件 | ≈行数 | 说明 |
|---|---|---|
| `src/protocol/constants.rs` | 400 | 魔数、命令码、握手字节、**服务器双列表** |
| `src/protocol/types.rs` | 260 | SecurityBar/Quote/TickData 结构体 |
| `src/protocol/parsers.rs` | 1350 | 全部响应解析器（核心） |
| `src/net/packet.rs` + `connection.rs` | 170 | 响应头 + TcpStream 封装 |
| `src/net/utils.rs` | ~400（裁剪后） | 握手、zlib 解压、建包（砍掉限流/复权上下文） |
| `src/net/client.rs` | ~800（裁剪后） | 请求方法全集（砍掉 pool/SmartClient/异步） |
| `src/error.rs` | ~150 | 错误体系（error_codes 可精简） |

**合计 ≈ 2500–3000 行**即可得到自有同步协议内核；连接池/心跳/测速/黑名单按我们第 4 节的设计自己写（这部分本来就要按我们的 async 架构重做）。内化后需**自补不依赖外部文件的协议 fixture 测试**（录几组真实响应字节流作 golden）。

另注意 tdx-api（Go）的参考价值：其 `protocol/model_*.go` 的记录布局注释比 tdxrs 更贴近 pytdx 原始语义，内化时若 tdxrs 某解析器存疑可交叉对照。但 oficcejo/tdx-api **仓库无 LICENSE 文件**（README 的 MIT 链接是死链），其协议层实质源自 MIT 上游 injoyai/tdx——看思路无碍，不要复制其 web/ 层自创代码。

## 4. 正确性注意事项（接入前必须逐条校准）

1. **复权**：服务器返回原始未复权数据，fq 字段为保留位。我们的 `Adjust::Qfq/Hfq` 请求 tdx provider 必须返回 `NoProvider`，交给 failover 链上腾讯/东财；tdx 只接 `Adjust::None`。
2. **价格单位是厘**（÷1000 得元），成交额同为厘；分钟线成交量 ÷100、指数 K线成交量 ×100、指数记录多 4 字节涨跌家数。
3. **当日分时 0x051D 有已知价格编码 bug**，两个实现均绕走 0x0FB4 历史分时接口取当日——照做。
4. **分页硬限制**：K线单次 ≤800、五档单次 ≤60 只、当日分笔 ≤1800、历史分笔 ≤2000；`DataProvider::kline(count)` 需内部循环分页。
5. **北交所不要承诺**：hq 服务器无北交所列表，auto_market 判定也是残的，BJ 符号直接 `NoProvider`。
6. **K线时间编码双轨**：分钟线是位打包相对时间（`(x>>11)+2004`），日及以上是 YYYYMMDD 整数，解码分支不可混。
7. **zlib 判断**：仅当响应头 zip_size ≠ unzip_size 时解压，且需校验解压后长度。
8. **连接无鉴权 ≠ 无限流**：服务器有频控，tdxrs 实测需要分交易时段限流；高频批量拉取（如全市场扫描）仍应走东财 HTTP 链，tdx 只补实时性场景。
9. **符号映射**：`Symbol` ↔ (market u16, 6 字节 code) 需一层映射，复用现有号段判市规则（`6→沪/0,30→深`）。
10. **服务器列表需可持续更新**：内置列表 + 启动探测是底线；建议在设置里留"自定义 tdx 服务器"入口，并记录每台服务器最近成功时间做本地排序。

## 5. crates/tdx 行情 provider 设计

### 5.1 分层

- **`crates/tdx`（新建，纯协议层，不依赖 async runtime）**：内化 tdxrs 协议内核。同步阻塞 `TcpStream` + 上述编解码，暴露 `TdxClient`（K线/五档/分时/分笔/列表/数量）、`probe_servers()`、`ServerPool`。文件头保留 tdxrs MIT 版权声明。依赖只需 `flate2`、`thiserror`（进根 `[workspace.dependencies]`），并把 `"crates/tdx"` 加进根 `members`。
- **`crates/market-data/src/providers/tdx.rs`（新建，adapter）**：`TdxProvider` 实现现有 `DataProvider` trait（`crates/market-data/src/provider.rs:13`），用 `tokio::task::spawn_blocking` 包同步调用。实现 `kline`（仅 `Adjust::None`）、`quote`（五档快照映射到 `Quote`，档位字段现有结构放不下则丢弃或后续扩展）、`minute`（走 0x0FB4）、`all_a_shares`（0x0450 分页 + 60s TTL 缓存，复用 `TtlCache`）、`index_kline`（需 secid→tdx 映射）。`search`/资金流/F10 等不实现，默认 `NoProvider`。
- 入链：`hub.rs` 的 failover 链改为 `[tencent, sina, tdx, eastmoney]`，`Inner::new` 预注册 tdx 熔断器；`crates/core/src/provenance.rs` 的 `Source` 枚举加 `Tdx` 变体（溯源标签自动流通到前端"数据源："展示）。

### 5.2 连接池与选路（在 crates/tdx 内）

- `ServerPool`：启动时并发 `probe_servers()`（TCP + 握手 + 0x044E API 三段延迟），按 API 延迟排序，取 top-N（如 3 台）各维持 1 条长连接进池；探测结果落盘缓存（`data_dir/tdx_servers.json`，含各服务器最近成功时间），下次启动先测缓存的 top 集合、后台补测全量。
- 每条连接一个 `Mutex<TdxConn>`（同步模型下最简单可靠；调用方经 spawn_blocking 获取），连接内 30s 心跳由独立线程负责。
- **断线重连**：IO 错误/心跳失败 → 该连接标记坏死、服务器进黑名单（带冷却），从排序列表取下一台重建；单次请求失败按 `[0.1, 0.5, 1.0]s` 退避重试且每次换服务器（复用 tdxrs 验证过的策略）。连续全池失败 → 上报错误，由熔断器（`breaker.rs` 现成）切断并展示在健康面板。

### 5.3 与现有基建的对接点（全部复用，不新造）

- 熔断器 `breaker.rs`、TTL 缓存 `cache.rs`、single-flight `hub.rs`、K线清洗 `validate.rs`：provider 入链即自动获得，无需 tdx 侧代码。
- 实时刷新模式：维持现有前端 2s/5s 轮询 Tauri command + 2s TTL 缓存的模式，tdx 五档快照延迟本就足够，**不引入 TCP 推送**（协议本身也是请求/响应式，无订阅推送）。
- 风险隔离：tdx 是"锦上添花"通道，任何一层失败都由 `Failover` 静默降级到东财，用户体验无感。

## 6. 结论与建议

1. **协议接入可行，工程量可控**：内化 tdxrs 协议内核（~3000 行，MIT）+ 一个 adapter provider，约等于现有东财 provider（794 行）的 2–3 倍工作量，且边界清晰。
2. **不要直接依赖 tdxrs crate**（pyo3 硬依赖 + 未上 crates.io）；内化时保留其版权声明。
3. **定位第二通道**：failover 链 `[tencent, sina, tdx, eastmoney]`，只接未复权 K线/五档/分时/全 A 列表；复权、资金流、搜索、F10 维持现状。
4. **服务器列表是唯一长期维护项**：内置 tdxrs + tdx-api 双来源合并的 ~120 台候选，启动并发深度探测选 top-3，结果本地缓存，留自定义入口。
5. **落地顺序建议**：先 `crates/tdx` 协议内核 + golden fixture 测试（录真实字节流）→ `TdxProvider` 只接 `kline(Adjust::None)` 冒烟 → 再补 quote/minute/all_a_shares → 最后接入健康面板与设置页自定义服务器。
