# 数据底座 v2 规格:原始价格 + 复权因子 + 公司行为(point-in-time)

## 原则
- 数据库**只存原始(不复权)OHLCV + amount** 和公司行为记录;前/后复权在运行时由因子计算。
- 三种价格语义:不复权=当时真实成交价(涨跌停/撮合/回测成交);前复权=以指定锚点(默认最新)修正历史(技术指标/看图);后复权=以最早为基准累积(长期收益研究)。
- **Point-in-time**:在日期 T 做分析/回测时,只能使用除权除息日 ≤ T(且公告日 ≤ T)的公司行为,锚定到 T 的价格——禁止用今天的完整因子回穿历史信息集。
- 数据量有界:增量更新 + LRU 清理(沿用 cache_budget),单票 10 年日 K ≈ 2500 行 ≈ 几十 KB,全 A 缓存也在 GB 级以下可控。

## 复权因子数学(逐字实现口径)
对每次公司行为(除权日 E,昨收 C):
- 每股现金分红 D(税前)、每股送股+转增 B、每股配股 R 配股价 P:
  除权理论价 X = (C − D + P×R) / (1 + B + R);因子比 r = X / C
- 累积因子(前复权,锚=最新):factor_qfq(t) = ∏{r_i | E_i > t},最新日 factor=1
- 前复权价 = raw × factor_qfq;后复权价 = raw × factor_hfq,factor_hfq(t)=∏{r_i | E_i ≤ t}(最早=1 的倒数口径,实现时统一:hfq(t) = qfq(t) / qfq(t0) × raw(t0))
- 成交额/成交量:复权只调价格;手数口径成交量不变(与主流软件一致);均额计算用复权后价格时注明。
- 验证金标:自选因子算出的 qfq 序列必须与腾讯 qfq、东财 fqt=1 在重叠区间逐日吻合(容差 0.5%,除权日附近重点核对 600519/002594 等分红送转股)。

## Schema 变更(storage 迁移 v4)
- `corporate_actions(code, ex_date, notice_date NULL, cash_div REAL, bonus_share REAL, rights_ratio REAL, rights_price REAL NULL, source, source_url, fetched_at, PRIMARY KEY(code, ex_date))` —— 来源:东财 RPT_SHAREBONUS_DET(已有)扩展字段 + 配股数据(akshare 调研后补充源)。
- Parquet 时序改存 raw(fqt=0);读路径:`load_bars + load_actions → 运行时复权`。

## 数据管线
1. 拉 K 线:优先**不复权**原始数据(东财 fqt=0 为基准,腾讯空 fq 参数交叉验证)。
2. 拉公司行为:分红送转配股,增量(按 ex_date > 本地最大)。
3. 因子计算器 `adjust.rs`(纯函数):输入 raw bars + actions(按 PIT 截断)→ 输出指定锚点的 qfq/hfq/raw 序列。
4. 交叉验证:与服务商 qfq 对比,偏差超阈值记日志并在 UI 数据源健康面板标记。

## 回测对接
backtest crate 接受 `AdjustmentPolicy::{Raw, QfqAsOf(date)}`;默认回测用 QfqAsOf(当前 bar 日期)逐日锚定,成交价用 raw 口径日志双记。

## WAF/稳定性策略(诚实边界:不能"杜绝",只能最小化+自愈)
1. 持久缓存是第一防线:历史 K 线落盘,重复查看零请求;每天每票只增量 1 根。
2. 熔断器(agent-24):腾讯 WAF 触发后冷却 10min,期间东财直接服务,不再每次白付两次失败。
3. 单飞合并:同标的并发请求只发一次上游。
4. 自适应限速(已有):429/5xx/超时指数退避;扫描批处理走队列+进度。
5. 多源交叉:腾讯/新浪/东财互备;单源长期不可用进健康面板,不静默失败。
6. UA 池轮换 + host 池(已有);不做绕过 WAF 的挑战求解(违法/脆弱),靠少请求+多源达到稳定。

## 性能目标
- 个股页二次打开:P95 < 100ms(全本地)。
- 复权计算:单票 10 年 < 1ms(纯 f64 循环)。
- 增量更新:每日盘后每票 1-2 个请求。
