//! EastMoney 数据中心扩展报表:`datacenter-web.eastmoney.com` 统一报表接口、
//! `np-anotice-stock` 公告接口、`push2ex` 涨停板六池、板块 clist。
//!
//! 与 [`crate::providers::eastmoney_f10`] 一样,datacenter 报表返回
//! `{"result": {"pages": N, "count": M, "data": [...]}}` 信封(无顶层 `data`),
//! 不能走 `get_json_pool`,直接对单 host 请求,分页上限 `pageSize = 500`。
//!
//! # 接口清单(2026-08-22 实测可用,样例见 tests/fixtures/em_datacenter/)
//!
//! | 方法 | 报表/路径 | 说明 |
//! |------|-----------|------|
//! | [`EmDataCenter::billboard_detail`] | `RPT_DAILYBILLBOARD_DETAILSNEW` | 龙虎榜详情 |
//! | [`EmDataCenter::block_trade`] | `RPT_DATA_BLOCKTRADE` | 大宗交易每日明细 |
//! | [`EmDataCenter::margin_daily`] | `RPTA_WEB_MARGIN_DAILYTRADE` | 两融账户统计 |
//! | [`EmDataCenter::org_survey`] | `RPT_ORG_SURVEYNEW` | 机构调研统计 |
//! | [`EmDataCenter::holder_num_latest`] | `RPT_HOLDERNUMLATEST` | 股东户数(最新) |
//! | [`EmDataCenter::earnings_predict`] | `RPT_PUBLIC_OP_NEWPREDICT` | 业绩预告(datacenter /securities 路径) |
//! | [`EmDataCenter::lift_stage`] | `RPT_LIFT_STAGE` | 限售解禁明细 |
//! | [`EmDataCenter::suspensions`] | `RPT_CUSTOM_SUSPEND_DATA_INTERFACE` | 停复牌 |
//! | [`EmDataCenter::notices`] | `np-anotice-stock/api/security/ann` | 公告大全 |
//! | [`EmDataCenter::zt_pool`] 等 6 个 | `push2ex/getTopic*Pool` | 涨停/昨日涨停/强势/次新/炸板/跌停池 |
//! | [`EmDataCenter::board_list`] / [`EmDataCenter::board_cons`] | push2 `clist` | 板块列表(`m:90+t:2` 行业 / `m:90+t:3` 概念)与成分股 |
//!
//! # 单位约定(解析时已统一换算,字段文档逐项注明)
//!
//! - push2ex 池:`p`(最新价)、`ztp`(涨停价)为**整数厘**,解析时 **÷1000 → 元**;
//!   `amount`/`fund`/`fba`/`ltsz`/`tshare` 为**元**;`hs`/`zdp`/`zf`/`zs` 为 **%**;
//!   `fbt`/`lbt`/`yfbt` 为 `HHMMSS` 整数,解析为 `"HH:MM:SS"` 字符串。
//! - `RPT_LIFT_STAGE`:`*_FREE_SHARES` 为**万股**(×10000 → 股),`LIFT_MARKET_CAP`
//!   为**万元**(×10000 → 元),与 akshare 口径一致。
//! - `RPTA_WEB_MARGIN_DAILYTRADE`:`*_BALANCE` / `*_AMT` / `TOTAL_GUARANTEE` 为
//!   **亿元**(保留原始单位,不做换算,字段名以 `_yi` 结尾明示);
//!   `PERSONAL_INVESTOR_NUM` 为**万名**;`AVG_GUARANTEE_RATIO` 为 **%**。
//! - 龙虎榜/大宗交易/股东户数/业绩预告:金额为**元**,价格为**元**,比率为 **%**,
//!   股本为**股**,户数为**户**。

use crate::cache::{ttl, TtlCache};
use crate::http::{HttpClient, EM_TOKEN};
use crate::providers::eastmoney::QUOTE_HOSTS;
use crate::providers::json_f64;
use astock_core::{DataError, Fetched, Source};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// datacenter-web 报表 host。
const DC_WEB_HOST: &str = "https://datacenter-web.eastmoney.com";
/// datacenter /securities 路径 host(业绩预告)。
const DC_SEC_HOST: &str = "https://datacenter.eastmoney.com";
/// 公告 host。
const NOTICE_HOST: &str = "https://np-anotice-stock.eastmoney.com";
/// push2ex 涨停池 host。
const PUSH2EX_HOST: &str = "https://push2ex.eastmoney.com";
/// push2ex 涨停池专用 ut token(与行情 EM_TOKEN 不同,akshare 同款)。
const POOL_UT: &str = "7eea3edcaed734bea9cbfc24409ed989";

/// datacenter 报表分页上限(单页 ≤ 500,实测更大的值会被拒绝/截断)。
const DC_PAGE_SIZE: u32 = 500;
/// 公告接口固定 page_size。
const NOTICE_PAGE_SIZE: u32 = 100;
/// 板块 clist 单页大小。push2 clist 上游实际单页上限是 100 行
/// (请求 pz=500 也只回 100 行,与 `eastmoney.rs` 的 `CLIST_PAGE_SIZE` 一致),
/// 页数必须按 100 推算,否则会漏页。
const CLIST_PAGE_SIZE: u32 = 100;

/// 报表类数据变更频率低,缓存 10 分钟。(共享 cache 在容量压力下会驱逐早于
/// `ttl::MAX` 的条目——此处缓存是 best-effort,与 `eastmoney_f10` 同一取舍。)
const REPORT_TTL: Duration = Duration::from_secs(600);

/// 报表查询参数。
struct ReportQuery<'a> {
    /// `DC_WEB_HOST` 或 `DC_SEC_HOST`(后者走 /securities 路径)。
    host: &'a str,
    report_name: &'a str,
    columns: &'a str,
    filter: Option<String>,
    sort_columns: &'a str,
    sort_types: &'a str,
    quote_columns: Option<&'a str>,
    max_pages: u32,
    op: &'static str,
}

/// 东财数据中心扩展报表适配器。纯报表接口,不实现 `DataProvider`
/// (与 `EastMoneyF10` 同一模式),由 hub 作为独立字段暴露。
pub struct EmDataCenter {
    http: Arc<HttpClient>,
    cache: Arc<TtlCache>,
}

// ---------------------------------------------------------------------------
// 行结构(每报表一个强类型 Row + 解析函数)
// ---------------------------------------------------------------------------

/// 龙虎榜详情行(`RPT_DAILYBILLBOARD_DETAILSNEW`)。金额单位:元;比率:%。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BillboardRow {
    /// 6 位代码。
    pub code: String,
    /// 带市场后缀代码,如 `000017.SZ`。
    pub secucode: String,
    /// 简称。
    pub name: String,
    /// 上榜日。
    pub trade_date: Option<NaiveDate>,
    /// 收盘价(元)。
    pub close_price: Option<f64>,
    /// 涨跌幅(%)。
    pub change_rate: Option<f64>,
    /// 龙虎榜净买额(元)。
    pub net_amt: Option<f64>,
    /// 龙虎榜买入额(元)。
    pub buy_amt: Option<f64>,
    /// 龙虎榜卖出额(元)。
    pub sell_amt: Option<f64>,
    /// 龙虎榜成交额(元)。
    pub deal_amt: Option<f64>,
    /// 市场总成交额(元)。
    pub accum_amount: Option<f64>,
    /// 净买额占总成交比(%)。
    pub deal_net_ratio: Option<f64>,
    /// 成交额占总成交比(%)。
    pub deal_amount_ratio: Option<f64>,
    /// 换手率(%)。
    pub turnover_rate: Option<f64>,
    /// 流通市值(元)。
    pub free_market_cap: Option<f64>,
    /// 上榜原因。
    pub explanation: String,
    /// 上榜后 1/2/5/10 日涨跌幅(%)。
    pub d1_change: Option<f64>,
    /// 见 `d1_change`。
    pub d2_change: Option<f64>,
    /// 见 `d1_change`。
    pub d5_change: Option<f64>,
    /// 见 `d1_change`。
    pub d10_change: Option<f64>,
}

/// 大宗交易每日明细行(`RPT_DATA_BLOCKTRADE`)。价格:元;成交量:股;
/// 成交额:元(已用 `DEAL_VOLUME × DEAL_PRICE == DEAL_AMT` 验证)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockTradeRow {
    /// 交易日期。
    pub trade_date: Option<NaiveDate>,
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 当日收盘价(元)。
    pub close_price: Option<f64>,
    /// 当日涨跌幅(%)。
    pub change_rate: Option<f64>,
    /// 大宗成交价(元)。
    pub deal_price: Option<f64>,
    /// 折溢率(**小数比率**,相对收盘价;实测 11.68 vs 11.69 → -0.000855,
    /// 即 -0.0855%;上游原始口径,未乘 100)。
    pub premium_ratio: Option<f64>,
    /// 成交量(股)。
    pub deal_volume: Option<f64>,
    /// 成交额(元)。
    pub deal_amt: Option<f64>,
    /// 成交额/流通市值(%)。
    pub turnover_rate: Option<f64>,
    /// 买方营业部。
    pub buyer_name: String,
    /// 卖方营业部。
    pub seller_name: String,
}

/// 两融账户统计行(`RPTA_WEB_MARGIN_DAILYTRADE`)。
/// 余额/买入额/担保物单位:**亿元**(字段以 `_yi` 结尾);比率为 %。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarginDailyRow {
    /// 统计日期。
    pub statistics_date: Option<NaiveDate>,
    /// 融资余额(亿元)。
    pub fin_balance_yi: Option<f64>,
    /// 融券余额(亿元)。
    pub loan_balance_yi: Option<f64>,
    /// 两融余额合计(亿元)。
    pub margin_balance_yi: Option<f64>,
    /// 融资买入额(亿元)。
    pub fin_buy_amt_yi: Option<f64>,
    /// 融券卖出额(亿元)。
    pub loan_sell_amt_yi: Option<f64>,
    /// 证券公司数量(家)。
    pub security_org_num: Option<f64>,
    /// 营业部数量(家)。
    pub operatedept_num: Option<f64>,
    /// 个人投资者数量(万名,实测 837.5079 ≈ 837.5 万)。
    pub personal_investor_num: Option<f64>,
    /// 机构投资者数量(家)。
    pub org_investor_num: Option<f64>,
    /// 参与交易的投资者数量(名)。
    pub investor_num: Option<f64>,
    /// 有两融负债的投资者数量(名)。
    pub marginliab_investor_num: Option<f64>,
    /// 担保物总价值(亿元)。
    pub total_guarantee_yi: Option<f64>,
    /// 平均维持担保比例(%)。
    pub avg_guarantee_ratio: Option<f64>,
}

/// 机构调研统计行(`RPT_ORG_SURVEYNEW`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrgSurveyRow {
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 公告日期。
    pub notice_date: Option<NaiveDate>,
    /// 接待开始日期。
    pub receive_start_date: Option<NaiveDate>,
    /// 接待结束日期。
    pub receive_end_date: Option<NaiveDate>,
    /// 接待机构数量(`SUM`)。
    pub org_count: Option<f64>,
    /// 接待方式描述。
    pub receive_way_explain: String,
    /// 接待地点。
    pub receive_place: String,
    /// 接待人员(公司方)。
    pub receptionist: String,
    /// 调研机构(可能为多家拼接,原始字符串)。
    pub receive_object: String,
    /// 最新价(元,`quoteColumns` f2 注入)。
    pub close_price: Option<f64>,
    /// 涨跌幅(%,f3 注入)。
    pub change_rate: Option<f64>,
}

/// 股东户数行(`RPT_HOLDERNUMLATEST`)。市值:元;股本:股;户数:户。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HolderNumRow {
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 本次统计截止日。
    pub end_date: Option<NaiveDate>,
    /// 上次统计截止日。
    pub pre_end_date: Option<NaiveDate>,
    /// 公告日期。
    pub hold_notice_date: Option<NaiveDate>,
    /// 股东户数-本次(户)。
    pub holder_num: Option<f64>,
    /// 股东户数-上次(户)。
    pub pre_holder_num: Option<f64>,
    /// 户数增减(户)。
    pub holder_num_change: Option<f64>,
    /// 户数增减比例(%)。
    pub holder_num_ratio: Option<f64>,
    /// 区间涨跌幅(%)。
    pub interval_change: Option<f64>,
    /// 户均持股市值(元)。
    pub avg_market_cap: Option<f64>,
    /// 户均持股数量(股)。
    pub avg_hold_num: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 总股本(股)。
    pub total_a_shares: Option<f64>,
}

/// 业绩预告行(`RPT_PUBLIC_OP_NEWPREDICT`)。金额:元;幅度:%。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EarningsPredictRow {
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 公告日期。
    pub notice_date: Option<NaiveDate>,
    /// 报告期(如 2026-06-30)。
    pub report_date: Option<NaiveDate>,
    /// 预测指标(如"归属于上市公司股东的净利润")。
    pub predict_finance: String,
    /// 预告类型(预增/预减/扭亏/首亏 …)。
    pub predict_type: String,
    /// 业绩变动描述。
    pub predict_content: String,
    /// 业绩变动原因。
    pub change_reason: String,
    /// 预测数值下限(元)。
    pub predict_amt_lower: Option<f64>,
    /// 预测数值上限(元)。
    pub predict_amt_upper: Option<f64>,
    /// 业绩变动幅度下限(%)。
    pub add_amp_lower: Option<f64>,
    /// 业绩变动幅度上限(%)。
    pub add_amp_upper: Option<f64>,
    /// 上年同期值(元)。
    pub preyear_same_period: Option<f64>,
}

/// 限售解禁明细行(`RPT_LIFT_STAGE`)。
/// 数量字段已由**万股 ×10000 → 股**;市值由**万元 ×10000 → 元**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiftStageRow {
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 解禁时间。
    pub free_date: Option<NaiveDate>,
    /// 实际解禁数量(股,原始 `CURRENT_FREE_SHARES` 万股 ×10000)。
    pub current_free_shares: Option<f64>,
    /// 解禁数量(股,原始 `ABLE_FREE_SHARES` 万股 ×10000)。
    pub able_free_shares: Option<f64>,
    /// 实际解禁市值(元,原始 `LIFT_MARKET_CAP` 万元 ×10000)。
    pub lift_market_cap: Option<f64>,
    /// 占解禁前流通市值比例(小数,实测 0.0000287)。
    pub free_ratio: Option<f64>,
    /// 解禁前一交易日收盘价(元)。
    pub pre_close: Option<f64>,
    /// 解禁前 20 日涨跌幅(%)。
    pub b20_change: Option<f64>,
    /// 解禁后 20 日涨跌幅(%,未来解禁为 null)。
    pub a20_change: Option<f64>,
    /// 限售股类型(如"首发原股东限售股份")。
    pub free_shares_type: String,
}

/// 停复牌行(`RPT_CUSTOM_SUSPEND_DATA_INTERFACE`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuspendRow {
    /// 6 位代码。
    pub code: String,
    /// 简称。
    pub name: String,
    /// 停牌开始日期。
    pub suspend_start_date: Option<NaiveDate>,
    /// 停牌时间(含时分秒,原始字符串)。
    pub suspend_start_time: String,
    /// 停牌截止时间(可能未定为 null)。
    pub suspend_end_time: String,
    /// 停牌期限(如"连续停牌"、"停牌1天")。
    pub suspend_expire: String,
    /// 停牌原因。
    pub suspend_reason: String,
    /// 所属市场(如"深交所风险警示板")。
    pub trade_market: String,
    /// 预计复牌时间。
    pub predict_resume_date: Option<NaiveDate>,
}

/// 公告行(`np-anotice-stock/api/security/ann`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoticeRow {
    /// 公告编码(art_code)。
    pub art_code: String,
    /// 公告标题。
    pub title: String,
    /// 公告日期。
    pub notice_date: Option<NaiveDate>,
    /// 页面展示时间(原始字符串,格式 `YYYY-MM-DD HH:MM:SS:mmm`)。
    pub display_time: String,
    /// 公告类型(column_name,如"其他"、"定期报告")。
    pub column_name: String,
    /// 6 位代码(取 `codes` 中首个 A 股条目)。
    pub stock_code: String,
    /// 简称。
    pub stock_name: String,
    /// 详情页 URL(`data.eastmoney.com/notices/detail/{code}/{art_code}.html`)。
    pub url: String,
}

/// 公告分类节点(`f_node` 参数)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeNode {
    /// 全部。
    All,
    /// 财务报告。
    Financial,
    /// 融资公告。
    Financing,
    /// 风险提示。
    Risk,
    /// 信息变更。
    InfoChange,
    /// 重大事项。
    Major,
    /// 资产重组。
    Restructure,
    /// 持股变动。
    HoldingChange,
}

impl NoticeNode {
    fn f_node(self) -> &'static str {
        match self {
            NoticeNode::All => "0",
            NoticeNode::Financial => "1",
            NoticeNode::Financing => "2",
            NoticeNode::Risk => "3",
            NoticeNode::InfoChange => "4",
            NoticeNode::Major => "5",
            NoticeNode::Restructure => "6",
            NoticeNode::HoldingChange => "7",
        }
    }
}

// --- push2ex 六池(价格字段已 ÷1000 → 元;金额:元;比率:%) ---

/// 涨停统计(`zttj`):近 `days` 天内涨停 `ct` 次。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitStat {
    /// 统计窗口天数。
    pub days: u32,
    /// 窗口内涨停次数。
    pub ct: u32,
}

/// 涨停股池行(`getTopicZTPool`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZtPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 换手率(%)。
    pub turnover: Option<f64>,
    /// 连板数。
    pub limit_times: Option<u32>,
    /// 首次封板时间(`HH:MM:SS`)。
    pub first_lock_time: String,
    /// 最后封板时间(`HH:MM:SS`)。
    pub last_lock_time: String,
    /// 封板资金(元)。
    pub lock_fund: Option<f64>,
    /// 炸板次数。
    pub break_times: Option<u32>,
    /// 涨停统计。
    pub limit_stat: Option<LimitStat>,
    /// 所属行业。
    pub industry: String,
}

/// 昨日涨停股池行(`getYesterdayZTPool`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrevZtPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨停价(元,`ztp` ÷1000)。
    pub limit_price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 换手率(%)。
    pub turnover: Option<f64>,
    /// 振幅(%)。
    pub amplitude: Option<f64>,
    /// 涨速(%)。
    pub speed: Option<f64>,
    /// 昨日封板时间(`HH:MM:SS`)。
    pub yesterday_lock_time: String,
    /// 昨日连板数。
    pub yesterday_limit_times: Option<u32>,
    /// 涨停统计。
    pub limit_stat: Option<LimitStat>,
    /// 所属行业。
    pub industry: String,
}

/// 强势股池入选理由(`ztf`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StrongReason {
    /// 60 日新高。
    NewHigh60,
    /// 近期多次涨停。
    RecentLimits,
    /// 60 日新高且近期多次涨停。
    Both,
    /// 未知代码。
    Unknown,
}

/// 强势股池行(`getTopicQSPool`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrongPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨停价(元,`ztp` ÷1000)。
    pub limit_price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 换手率(%)。
    pub turnover: Option<f64>,
    /// 是否 60 日新高(`nh == 1`)。
    pub is_new_high: bool,
    /// 入选理由(`ztf`)。
    pub reason: StrongReason,
    /// 量比。
    pub volume_ratio: Option<f64>,
    /// 涨速(%)。
    pub speed: Option<f64>,
    /// 涨停统计。
    pub limit_stat: Option<LimitStat>,
    /// 所属行业。
    pub industry: String,
}

/// 次新股池行(`getTopicCXPooll`,注意路径末尾双写 l 是上游原名)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubNewPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨停价(元,`ztp` ÷1000;无涨停价的新股为 1e9 哨兵,解析为 None)。
    pub limit_price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 转手率(%)。
    pub turnover: Option<f64>,
    /// 开板几日。
    pub open_days: Option<u32>,
    /// 开板日期(`od`,`YYYYMMDD`)。
    pub open_date: Option<NaiveDate>,
    /// 上市日期(`ipod`,`YYYYMMDD`;0 表示未知 → None)。
    pub ipo_date: Option<NaiveDate>,
    /// 是否新高(`nh == 1`)。
    pub is_new_high: bool,
    /// 涨停统计。
    pub limit_stat: Option<LimitStat>,
    /// 所属行业。
    pub industry: String,
}

/// 炸板股池行(`getTopicZBPool`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrokenPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨停价(元,`ztp` ÷1000)。
    pub limit_price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 换手率(%)。
    pub turnover: Option<f64>,
    /// 首次封板时间(`HH:MM:SS`)。
    pub first_lock_time: String,
    /// 炸板次数。
    pub break_times: Option<u32>,
    /// 振幅(%)。
    pub amplitude: Option<f64>,
    /// 涨速(%)。
    pub speed: Option<f64>,
    /// 涨停统计。
    pub limit_stat: Option<LimitStat>,
    /// 所属行业。
    pub industry: String,
}

/// 跌停股池行(`getTopicDTPool`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DtPoolRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,`p` ÷1000)。
    pub price: Option<f64>,
    /// 涨跌幅(%)。
    pub pct: Option<f64>,
    /// 成交额(元)。
    pub amount: Option<f64>,
    /// 流通市值(元)。
    pub float_market_cap: Option<f64>,
    /// 总市值(元)。
    pub total_market_cap: Option<f64>,
    /// 动态市盈率。
    pub pe_dynamic: Option<f64>,
    /// 换手率(%)。
    pub turnover: Option<f64>,
    /// 封单资金(元)。
    pub lock_fund: Option<f64>,
    /// 最后封板时间(`HH:MM:SS`)。
    pub last_lock_time: String,
    /// 板上成交额(元)。
    pub board_amount: Option<f64>,
    /// 连续跌停天数。
    pub limit_down_days: Option<u32>,
    /// 开板次数。
    pub open_times: Option<u32>,
    /// 所属行业。
    pub industry: String,
}

/// 板块类别(`fs=m:90+t:2` / `m:90+t:3`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardKind {
    /// 行业板块(2026-08-21 实测 496 个,含二级行业)。
    Industry,
    /// 概念板块(2026-08-21 实测 504 个)。
    Concept,
}

impl BoardKind {
    fn fs(self) -> &'static str {
        match self {
            BoardKind::Industry => "m:90+t:2",
            BoardKind::Concept => "m:90+t:3",
        }
    }

    fn cache_key(self) -> &'static str {
        match self {
            BoardKind::Industry => "dc_board_industry",
            BoardKind::Concept => "dc_board_concept",
        }
    }
}

/// 板块列表行(clist `m:90+t:2/t:3`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardRow {
    /// 板块代码(`BK0xxx`)。
    pub code: String,
    /// 板块名称。
    pub name: String,
    /// 最新点位(f2)。
    pub price: Option<f64>,
    /// 涨跌幅(%,f3)。
    pub pct: Option<f64>,
    /// 主力净流入(元,f62)。
    pub main_net_inflow: Option<f64>,
    /// 领涨股名称(f128)。
    pub lead_stock: String,
    /// 领涨股代码(f140)。
    pub lead_stock_code: String,
}

/// 板块成分股行(clist `fs=b:{BK代码}`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardConsRow {
    /// 6 位代码。
    pub code: String,
    /// 名称。
    pub name: String,
    /// 最新价(元,f2)。
    pub price: Option<f64>,
    /// 涨跌幅(%,f3)。
    pub pct: Option<f64>,
    /// 市盈率(动,f9)。
    pub pe: Option<f64>,
    /// 总市值(元,f20)。
    pub total_market_cap: Option<f64>,
    /// 流通市值(元,f21)。
    pub float_market_cap: Option<f64>,
}

// ---------------------------------------------------------------------------
// 解析辅助
// ---------------------------------------------------------------------------

/// 取字符串字段(缺失/null → 空串,裁剪空白)。
fn jstr(row: &serde_json::Value, key: &str) -> String {
    row.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 取数值字段(复用 `json_f64` 的宽松规则:数字/数字字符串可用,"-"/""/null → None)。
fn jnum(row: &serde_json::Value, key: &str) -> Option<f64> {
    row.get(key).and_then(json_f64)
}

/// 取 u32 字段(上游整数经 f64 中转)。
fn ju32(row: &serde_json::Value, key: &str) -> Option<u32> {
    jnum(row, key).map(|n| n as u32)
}

/// 解析 `"YYYY-MM-DD ..."` 或 `"YYYY-MM-DD"` 日期(取前 10 字符)。
fn jdate(row: &serde_json::Value, key: &str) -> Option<NaiveDate> {
    let s = row.get(key).and_then(|v| v.as_str())?;
    let head = s.get(..10)?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").ok()
}

/// 解析 `YYYYMMDD` 整数日期(push2ex `od`/`ipod`);0/缺失 → None。
fn jdate_compact(row: &serde_json::Value, key: &str) -> Option<NaiveDate> {
    let n = jnum(row, key)? as u32;
    if n == 0 {
        return None;
    }
    NaiveDate::parse_from_str(&n.to_string(), "%Y%m%d").ok()
}

/// push2ex 价格字段:整数厘 ÷1000 → 元。
fn pool_price(row: &serde_json::Value, key: &str) -> Option<f64> {
    jnum(row, key).map(|p| p / 1000.0)
}

/// push2ex `HHMMSS` 整数 → `"HH:MM:SS"`(如 92500 → "09:25:00")。
fn pool_time(row: &serde_json::Value, key: &str) -> String {
    match jnum(row, key) {
        Some(n) if n >= 0.0 => {
            let s = format!("{:06}", n as u64);
            format!("{}:{}:{}", &s[0..2], &s[2..4], &s[4..6])
        }
        _ => String::new(),
    }
}

/// 解析 `zttj` 涨停统计 `{days, ct}`。
fn pool_limit_stat(row: &serde_json::Value) -> Option<LimitStat> {
    let z = row.get("zttj")?;
    Some(LimitStat {
        days: ju32(z, "days")?,
        ct: ju32(z, "ct")?,
    })
}

/// 池行公共字段(code/name/price/pct/amount/市值/换手)。
struct PoolBase {
    code: String,
    name: String,
    price: Option<f64>,
    pct: Option<f64>,
    amount: Option<f64>,
    float_market_cap: Option<f64>,
    total_market_cap: Option<f64>,
    turnover: Option<f64>,
}

fn pool_base(row: &serde_json::Value) -> PoolBase {
    PoolBase {
        code: jstr(row, "c"),
        name: jstr(row, "n"),
        price: pool_price(row, "p"),
        pct: jnum(row, "zdp"),
        amount: jnum(row, "amount"),
        float_market_cap: jnum(row, "ltsz"),
        total_market_cap: jnum(row, "tshare"),
        turnover: jnum(row, "hs"),
    }
}

// ---------------------------------------------------------------------------
// 各报表解析函数(纯函数,fixture 单测直接调用)
// ---------------------------------------------------------------------------

/// 解析一行龙虎榜详情。
pub fn parse_billboard_row(row: &serde_json::Value) -> BillboardRow {
    BillboardRow {
        code: jstr(row, "SECURITY_CODE"),
        secucode: jstr(row, "SECUCODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        trade_date: jdate(row, "TRADE_DATE"),
        close_price: jnum(row, "CLOSE_PRICE"),
        change_rate: jnum(row, "CHANGE_RATE"),
        net_amt: jnum(row, "BILLBOARD_NET_AMT"),
        buy_amt: jnum(row, "BILLBOARD_BUY_AMT"),
        sell_amt: jnum(row, "BILLBOARD_SELL_AMT"),
        deal_amt: jnum(row, "BILLBOARD_DEAL_AMT"),
        accum_amount: jnum(row, "ACCUM_AMOUNT"),
        deal_net_ratio: jnum(row, "DEAL_NET_RATIO"),
        deal_amount_ratio: jnum(row, "DEAL_AMOUNT_RATIO"),
        turnover_rate: jnum(row, "TURNOVERRATE"),
        free_market_cap: jnum(row, "FREE_MARKET_CAP"),
        explanation: jstr(row, "EXPLANATION"),
        d1_change: jnum(row, "D1_CLOSE_ADJCHRATE"),
        d2_change: jnum(row, "D2_CLOSE_ADJCHRATE"),
        d5_change: jnum(row, "D5_CLOSE_ADJCHRATE"),
        d10_change: jnum(row, "D10_CLOSE_ADJCHRATE"),
    }
}

/// 解析一行大宗交易明细。
pub fn parse_block_trade_row(row: &serde_json::Value) -> BlockTradeRow {
    BlockTradeRow {
        trade_date: jdate(row, "TRADE_DATE"),
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        close_price: jnum(row, "CLOSE_PRICE"),
        change_rate: jnum(row, "CHANGE_RATE"),
        deal_price: jnum(row, "DEAL_PRICE"),
        premium_ratio: jnum(row, "PREMIUM_RATIO"),
        deal_volume: jnum(row, "DEAL_VOLUME"),
        deal_amt: jnum(row, "DEAL_AMT"),
        turnover_rate: jnum(row, "TURNOVER_RATE"),
        buyer_name: jstr(row, "BUYER_NAME"),
        seller_name: jstr(row, "SELLER_NAME"),
    }
}

/// 解析一行两融账户统计。
pub fn parse_margin_daily_row(row: &serde_json::Value) -> MarginDailyRow {
    MarginDailyRow {
        statistics_date: jdate(row, "STATISTICS_DATE"),
        fin_balance_yi: jnum(row, "FIN_BALANCE"),
        loan_balance_yi: jnum(row, "LOAN_BALANCE"),
        margin_balance_yi: jnum(row, "MARGIN_BALANCE"),
        fin_buy_amt_yi: jnum(row, "FIN_BUY_AMT"),
        loan_sell_amt_yi: jnum(row, "LOAN_SELL_AMT"),
        security_org_num: jnum(row, "SECURITY_ORG_NUM"),
        operatedept_num: jnum(row, "OPERATEDEPT_NUM"),
        personal_investor_num: jnum(row, "PERSONAL_INVESTOR_NUM"),
        org_investor_num: jnum(row, "ORG_INVESTOR_NUM"),
        investor_num: jnum(row, "INVESTOR_NUM"),
        marginliab_investor_num: jnum(row, "MARGINLIAB_INVESTOR_NUM"),
        total_guarantee_yi: jnum(row, "TOTAL_GUARANTEE"),
        avg_guarantee_ratio: jnum(row, "AVG_GUARANTEE_RATIO"),
    }
}

/// 解析一行机构调研统计。
pub fn parse_org_survey_row(row: &serde_json::Value) -> OrgSurveyRow {
    OrgSurveyRow {
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        notice_date: jdate(row, "NOTICE_DATE"),
        receive_start_date: jdate(row, "RECEIVE_START_DATE"),
        receive_end_date: jdate(row, "RECEIVE_END_DATE"),
        org_count: jnum(row, "SUM"),
        receive_way_explain: jstr(row, "RECEIVE_WAY_EXPLAIN"),
        receive_place: jstr(row, "RECEIVE_PLACE"),
        receptionist: jstr(row, "RECEPTIONIST"),
        receive_object: jstr(row, "RECEIVE_OBJECT"),
        close_price: jnum(row, "CLOSE_PRICE"),
        change_rate: jnum(row, "CHANGE_RATE"),
    }
}

/// 解析一行股东户数。
pub fn parse_holder_num_row(row: &serde_json::Value) -> HolderNumRow {
    HolderNumRow {
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        end_date: jdate(row, "END_DATE"),
        pre_end_date: jdate(row, "PRE_END_DATE"),
        hold_notice_date: jdate(row, "HOLD_NOTICE_DATE"),
        holder_num: jnum(row, "HOLDER_NUM"),
        pre_holder_num: jnum(row, "PRE_HOLDER_NUM"),
        holder_num_change: jnum(row, "HOLDER_NUM_CHANGE"),
        holder_num_ratio: jnum(row, "HOLDER_NUM_RATIO"),
        interval_change: jnum(row, "INTERVAL_CHRATE"),
        avg_market_cap: jnum(row, "AVG_MARKET_CAP"),
        avg_hold_num: jnum(row, "AVG_HOLD_NUM"),
        total_market_cap: jnum(row, "TOTAL_MARKET_CAP"),
        total_a_shares: jnum(row, "TOTAL_A_SHARES"),
    }
}

/// 解析一行业绩预告。
pub fn parse_earnings_predict_row(row: &serde_json::Value) -> EarningsPredictRow {
    EarningsPredictRow {
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        notice_date: jdate(row, "NOTICE_DATE"),
        report_date: jdate(row, "REPORT_DATE"),
        predict_finance: jstr(row, "PREDICT_FINANCE"),
        predict_type: jstr(row, "PREDICT_TYPE"),
        predict_content: jstr(row, "PREDICT_CONTENT"),
        change_reason: jstr(row, "CHANGE_REASON_EXPLAIN"),
        predict_amt_lower: jnum(row, "PREDICT_AMT_LOWER"),
        predict_amt_upper: jnum(row, "PREDICT_AMT_UPPER"),
        add_amp_lower: jnum(row, "ADD_AMP_LOWER"),
        add_amp_upper: jnum(row, "ADD_AMP_UPPER"),
        preyear_same_period: jnum(row, "PREYEAR_SAME_PERIOD"),
    }
}

/// 解析一行限售解禁(万股→股、万元→元换算在此完成)。
pub fn parse_lift_stage_row(row: &serde_json::Value) -> LiftStageRow {
    LiftStageRow {
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        free_date: jdate(row, "FREE_DATE"),
        current_free_shares: jnum(row, "CURRENT_FREE_SHARES").map(|v| v * 10000.0),
        able_free_shares: jnum(row, "ABLE_FREE_SHARES").map(|v| v * 10000.0),
        lift_market_cap: jnum(row, "LIFT_MARKET_CAP").map(|v| v * 10000.0),
        free_ratio: jnum(row, "FREE_RATIO"),
        pre_close: jnum(row, "NEW"),
        b20_change: jnum(row, "B20_ADJCHRATE"),
        a20_change: jnum(row, "A20_ADJCHRATE"),
        free_shares_type: jstr(row, "FREE_SHARES_TYPE"),
    }
}

/// 解析一行停复牌。
pub fn parse_suspend_row(row: &serde_json::Value) -> SuspendRow {
    SuspendRow {
        code: jstr(row, "SECURITY_CODE"),
        name: jstr(row, "SECURITY_NAME_ABBR"),
        suspend_start_date: jdate(row, "SUSPEND_START_DATE"),
        suspend_start_time: jstr(row, "SUSPEND_START_TIME"),
        suspend_end_time: jstr(row, "SUSPEND_END_TIME"),
        suspend_expire: jstr(row, "SUSPEND_EXPIRE"),
        suspend_reason: jstr(row, "SUSPEND_REASON"),
        trade_market: jstr(row, "TRADE_MARKET"),
        predict_resume_date: jdate(row, "PREDICT_RESUME_DATE"),
    }
}

/// 解析一条公告。`codes` 中取首个 `ann_type` 以 "A" 开头的条目(与 akshare
/// 一致:多代码公告取 A 股代码)。
pub fn parse_notice_row(row: &serde_json::Value) -> NoticeRow {
    let empty = serde_json::Value::Null;
    let codes = row
        .get("codes")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let code_entry = codes
        .iter()
        .find(|c| {
            c.get("ann_type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t.starts_with('A'))
        })
        .or(codes.first())
        .unwrap_or(&empty);
    let column_name = row
        .get("columns")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .map(|c| jstr(c, "column_name"))
        .unwrap_or_default();
    let art_code = jstr(row, "art_code");
    let stock_code = jstr(code_entry, "stock_code");
    NoticeRow {
        url: format!("https://data.eastmoney.com/notices/detail/{stock_code}/{art_code}.html"),
        art_code,
        title: jstr(row, "title"),
        notice_date: jdate(row, "notice_date"),
        display_time: jstr(row, "display_time"),
        column_name,
        stock_code,
        stock_name: jstr(code_entry, "short_name"),
    }
}

/// 解析一行涨停池。
pub fn parse_zt_pool_row(row: &serde_json::Value) -> ZtPoolRow {
    let b = pool_base(row);
    ZtPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        turnover: b.turnover,
        limit_times: ju32(row, "lbc"),
        first_lock_time: pool_time(row, "fbt"),
        last_lock_time: pool_time(row, "lbt"),
        lock_fund: jnum(row, "fund"),
        break_times: ju32(row, "zbc"),
        limit_stat: pool_limit_stat(row),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行昨日涨停池。
pub fn parse_prev_zt_pool_row(row: &serde_json::Value) -> PrevZtPoolRow {
    let b = pool_base(row);
    PrevZtPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        limit_price: pool_price(row, "ztp"),
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        turnover: b.turnover,
        amplitude: jnum(row, "zf"),
        speed: jnum(row, "zs"),
        yesterday_lock_time: pool_time(row, "yfbt"),
        yesterday_limit_times: ju32(row, "ylbc"),
        limit_stat: pool_limit_stat(row),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行强势股池。
pub fn parse_strong_pool_row(row: &serde_json::Value) -> StrongPoolRow {
    let b = pool_base(row);
    let reason = match jstr(row, "ztf").as_str() {
        "1" => StrongReason::NewHigh60,
        "2" => StrongReason::RecentLimits,
        "3" => StrongReason::Both,
        _ => StrongReason::Unknown,
    };
    StrongPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        limit_price: pool_price(row, "ztp"),
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        turnover: b.turnover,
        is_new_high: ju32(row, "nh") == Some(1),
        reason,
        volume_ratio: jnum(row, "lb"),
        speed: jnum(row, "zs"),
        limit_stat: pool_limit_stat(row),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行次新股池。`ztp` 的 1e9 哨兵(未开过一字板的新股无涨停价)→ None,
/// 与 akshare `> 100000 → NA` 的规则一致(比较在 ÷1000 后的元单位上进行)。
pub fn parse_sub_new_pool_row(row: &serde_json::Value) -> SubNewPoolRow {
    let b = pool_base(row);
    let limit_price = pool_price(row, "ztp").filter(|p| *p <= 100_000.0);
    SubNewPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        limit_price,
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        turnover: b.turnover,
        open_days: ju32(row, "ods"),
        open_date: jdate_compact(row, "od"),
        ipo_date: jdate_compact(row, "ipod"),
        is_new_high: ju32(row, "nh") == Some(1),
        limit_stat: pool_limit_stat(row),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行炸板股池。
pub fn parse_broken_pool_row(row: &serde_json::Value) -> BrokenPoolRow {
    let b = pool_base(row);
    BrokenPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        limit_price: pool_price(row, "ztp"),
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        turnover: b.turnover,
        first_lock_time: pool_time(row, "fbt"),
        break_times: ju32(row, "zbc"),
        amplitude: jnum(row, "zf"),
        speed: jnum(row, "zs"),
        limit_stat: pool_limit_stat(row),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行跌停股池。
pub fn parse_dt_pool_row(row: &serde_json::Value) -> DtPoolRow {
    let b = pool_base(row);
    DtPoolRow {
        code: b.code,
        name: b.name,
        price: b.price,
        pct: b.pct,
        amount: b.amount,
        float_market_cap: b.float_market_cap,
        total_market_cap: b.total_market_cap,
        pe_dynamic: jnum(row, "pe"),
        turnover: b.turnover,
        lock_fund: jnum(row, "fund"),
        last_lock_time: pool_time(row, "lbt"),
        board_amount: jnum(row, "fba"),
        limit_down_days: ju32(row, "days"),
        open_times: ju32(row, "oc"),
        industry: jstr(row, "hybk"),
    }
}

/// 解析一行板块列表。
pub fn parse_board_row(row: &serde_json::Value) -> BoardRow {
    BoardRow {
        code: jstr(row, "f12"),
        name: jstr(row, "f14"),
        price: jnum(row, "f2"),
        pct: jnum(row, "f3"),
        main_net_inflow: jnum(row, "f62"),
        lead_stock: jstr(row, "f128"),
        lead_stock_code: jstr(row, "f140"),
    }
}

/// 解析一行板块成分股。
pub fn parse_board_cons_row(row: &serde_json::Value) -> BoardConsRow {
    BoardConsRow {
        code: jstr(row, "f12"),
        name: jstr(row, "f14"),
        price: jnum(row, "f2"),
        pct: jnum(row, "f3"),
        pe: jnum(row, "f9"),
        total_market_cap: jnum(row, "f20"),
        float_market_cap: jnum(row, "f21"),
    }
}

// ---------------------------------------------------------------------------
// 适配器实现
// ---------------------------------------------------------------------------

impl EmDataCenter {
    /// Wrap the shared HTTP client and cache.
    pub fn new(http: Arc<HttpClient>, cache: Arc<TtlCache>) -> Self {
        EmDataCenter { http, cache }
    }

    /// datacenter 报表单页请求,返回 `(rows, total_pages)`。
    ///
    /// 上游拒绝参数时返回 `{"success": false, "message": ...}`(无 `result`),
    /// 映射为 `DataError::Parse`;`result` 为 null(合法空结果,如某日无停复牌)
    /// 返回空页。
    async fn report_page(
        &self,
        query: &ReportQuery<'_>,
        page: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DataError> {
        let mut params = vec![
            ("reportName".to_string(), query.report_name.to_string()),
            ("columns".to_string(), query.columns.to_string()),
            ("sortColumns".to_string(), query.sort_columns.to_string()),
            ("sortTypes".to_string(), query.sort_types.to_string()),
            ("pageSize".to_string(), DC_PAGE_SIZE.to_string()),
            ("pageNumber".to_string(), page.to_string()),
            ("source".to_string(), "WEB".to_string()),
            ("client".to_string(), "WEB".to_string()),
        ];
        if let Some(filter) = &query.filter {
            params.push(("filter".to_string(), filter.clone()));
        }
        if let Some(qc) = query.quote_columns {
            params.push(("quoteColumns".to_string(), qc.to_string()));
        }
        let path = if query.host == DC_SEC_HOST {
            "/securities/api/data/v1/get"
        } else {
            "/api/data/v1/get"
        };
        let url = format!("{}{path}", query.host);
        let value = self.http.get_json(&url, &params).await?;
        if value.get("success").and_then(|s| s.as_bool()) == Some(false) {
            let message = value
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
                .to_string();
            return Err(DataError::Parse {
                upstream: format!("{} ({})", query.host, query.op),
                message,
            });
        }
        let result = value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if result.is_null() {
            return Ok((Vec::new(), 0));
        }
        let pages = result.get("pages").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
        let rows = result
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        Ok((rows, pages))
    }

    /// 循环分页直到数据取完或达到 `max_pages`(pageSize 固定 500)。
    /// 空结果(合法)返回空 Vec。
    async fn report_rows(
        &self,
        query: &ReportQuery<'_>,
    ) -> Result<Vec<serde_json::Value>, DataError> {
        let (first, pages) = self.report_page(query, 1).await?;
        let mut rows = first;
        let last = pages.min(query.max_pages);
        for page in 2..=last {
            let (mut more, _) = self.report_page(query, page).await?;
            if more.is_empty() {
                break;
            }
            rows.append(&mut more);
        }
        Ok(rows)
    }

    /// 通用报表获取:缓存 → 分页 → 逐行解析。
    async fn report<T, F>(
        &self,
        cache_key: &str,
        query: &ReportQuery<'_>,
        parse: F,
    ) -> Result<Fetched<Vec<T>>, DataError>
    where
        T: Serialize + serde::de::DeserializeOwned,
        F: Fn(&serde_json::Value) -> T,
    {
        if let Some(hit) = self.cache.get::<Fetched<Vec<T>>>(cache_key, REPORT_TTL) {
            return Ok(hit);
        }
        let rows = self.report_rows(query).await?;
        let out = Fetched::now(
            rows.iter().map(parse).collect::<Vec<T>>(),
            Source::EastMoney,
        );
        self.cache.set(cache_key, &out);
        Ok(out)
    }

    /// 龙虎榜详情(`RPT_DAILYBILLBOARD_DETAILSNEW`),按上榜日区间过滤。
    /// 一天约 100~300 行,`max_pages=4`(2000 行)足够覆盖一周以上。
    pub async fn billboard_detail(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<BillboardRow>>, DataError> {
        let key = format!("dc_billboard_{start}_{end}_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_DAILYBILLBOARD_DETAILSNEW",
            columns: "SECURITY_CODE,SECUCODE,SECURITY_NAME_ABBR,TRADE_DATE,EXPLAIN,CLOSE_PRICE,CHANGE_RATE,BILLBOARD_NET_AMT,BILLBOARD_BUY_AMT,BILLBOARD_SELL_AMT,BILLBOARD_DEAL_AMT,ACCUM_AMOUNT,DEAL_NET_RATIO,DEAL_AMOUNT_RATIO,TURNOVERRATE,FREE_MARKET_CAP,EXPLANATION,D1_CLOSE_ADJCHRATE,D2_CLOSE_ADJCHRATE,D5_CLOSE_ADJCHRATE,D10_CLOSE_ADJCHRATE,SECURITY_TYPE_CODE",
            filter: Some(format!("(TRADE_DATE>='{start}')(TRADE_DATE<='{end}')")),
            sort_columns: "SECURITY_CODE,TRADE_DATE",
            sort_types: "1,-1",
            quote_columns: None,
            max_pages,
            op: "billboard_detail",
        };
        self.report(&key, &query, parse_billboard_row).await
    }

    /// 大宗交易每日明细(`RPT_DATA_BLOCKTRADE`,A 股,`SECURITY_TYPE_WEB=1`)。
    pub async fn block_trade(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<BlockTradeRow>>, DataError> {
        let key = format!("dc_blocktrade_{start}_{end}_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_DATA_BLOCKTRADE",
            columns: "TRADE_DATE,SECURITY_CODE,SECUCODE,SECURITY_NAME_ABBR,CHANGE_RATE,CLOSE_PRICE,DEAL_PRICE,PREMIUM_RATIO,DEAL_VOLUME,DEAL_AMT,TURNOVER_RATE,BUYER_NAME,SELLER_NAME,BUYER_CODE,SELLER_CODE",
            filter: Some(format!(
                "(SECURITY_TYPE_WEB=1)(TRADE_DATE>='{start}')(TRADE_DATE<='{end}')"
            )),
            sort_columns: "SECURITY_CODE",
            sort_types: "1",
            quote_columns: None,
            max_pages,
            op: "block_trade",
        };
        self.report(&key, &query, parse_block_trade_row).await
    }

    /// 两融账户统计(`RPTA_WEB_MARGIN_DAILYTRADE`),最新日期在前。
    /// 一页 500 行 ≈ 2 年交易日;`max_pages` 控制历史深度。
    pub async fn margin_daily(
        &self,
        max_pages: u32,
    ) -> Result<Fetched<Vec<MarginDailyRow>>, DataError> {
        let key = format!("dc_margin_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPTA_WEB_MARGIN_DAILYTRADE",
            columns: "ALL",
            filter: None,
            sort_columns: "STATISTICS_DATE",
            sort_types: "-1",
            quote_columns: None,
            max_pages,
            op: "margin_daily",
        };
        self.report(&key, &query, parse_margin_daily_row).await
    }

    /// 机构调研统计(`RPT_ORG_SURVEYNEW`),`since` 之后的公告。
    pub async fn org_survey(
        &self,
        since: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<OrgSurveyRow>>, DataError> {
        let key = format!("dc_orgsurvey_{since}_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_ORG_SURVEYNEW",
            columns: "ALL",
            filter: Some(format!(
                "(NUMBERNEW=\"1\")(IS_SOURCE=\"1\")(NOTICE_DATE>'{since}')"
            )),
            sort_columns: "NOTICE_DATE,SUM,RECEIVE_START_DATE,SECURITY_CODE",
            sort_types: "-1,-1,-1,1",
            quote_columns: Some("f2~01~SECURITY_CODE~CLOSE_PRICE,f3~01~SECURITY_CODE~CHANGE_RATE"),
            max_pages,
            op: "org_survey",
        };
        self.report(&key, &query, parse_org_survey_row).await
    }

    /// 股东户数(最新一期,`RPT_HOLDERNUMLATEST`)。全市场 ~5400 行,
    /// 需要 `max_pages >= 11` 才能取全。
    pub async fn holder_num_latest(
        &self,
        max_pages: u32,
    ) -> Result<Fetched<Vec<HolderNumRow>>, DataError> {
        let key = format!("dc_holdernum_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_HOLDERNUMLATEST",
            columns: "SECURITY_CODE,SECURITY_NAME_ABBR,END_DATE,INTERVAL_CHRATE,AVG_MARKET_CAP,AVG_HOLD_NUM,TOTAL_MARKET_CAP,TOTAL_A_SHARES,HOLD_NOTICE_DATE,HOLDER_NUM,PRE_HOLDER_NUM,HOLDER_NUM_CHANGE,HOLDER_NUM_RATIO,PRE_END_DATE",
            filter: None,
            sort_columns: "HOLD_NOTICE_DATE,SECURITY_CODE",
            sort_types: "-1,-1",
            quote_columns: Some("f2,f3"),
            max_pages,
            op: "holder_num_latest",
        };
        self.report(&key, &query, parse_holder_num_row).await
    }

    /// 业绩预告(`RPT_PUBLIC_OP_NEWPREDICT`,走 datacenter /securities 路径)。
    /// `report_date` 为报告期(如 2026-06-30)。
    pub async fn earnings_predict(
        &self,
        report_date: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<EarningsPredictRow>>, DataError> {
        let key = format!("dc_predict_{report_date}_{max_pages}");
        let query = ReportQuery {
            host: DC_SEC_HOST,
            report_name: "RPT_PUBLIC_OP_NEWPREDICT",
            columns: "ALL",
            filter: Some(format!("(REPORT_DATE='{report_date}')")),
            sort_columns: "NOTICE_DATE,SECURITY_CODE",
            sort_types: "-1,-1",
            quote_columns: None,
            max_pages,
            op: "earnings_predict",
        };
        self.report(&key, &query, parse_earnings_predict_row).await
    }

    /// 限售解禁明细(`RPT_LIFT_STAGE`),按解禁日区间过滤。
    pub async fn lift_stage(
        &self,
        start: NaiveDate,
        end: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<LiftStageRow>>, DataError> {
        let key = format!("dc_lift_{start}_{end}_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_LIFT_STAGE",
            columns: "SECURITY_CODE,SECURITY_NAME_ABBR,FREE_DATE,CURRENT_FREE_SHARES,ABLE_FREE_SHARES,LIFT_MARKET_CAP,FREE_RATIO,NEW,B20_ADJCHRATE,A20_ADJCHRATE,FREE_SHARES_TYPE,TOTAL_RATIO,NON_FREE_SHARES,BATCH_HOLDER_NUM",
            filter: Some(format!("(FREE_DATE>='{start}')(FREE_DATE<='{end}')")),
            sort_columns: "FREE_DATE,CURRENT_FREE_SHARES",
            sort_types: "1,1",
            quote_columns: None,
            max_pages,
            op: "lift_stage",
        };
        self.report(&key, &query, parse_lift_stage_row).await
    }

    /// 停复牌(`RPT_CUSTOM_SUSPEND_DATA_INTERFACE`)。
    ///
    /// 注意:filter 必须包含 `(MARKET="全部")`——该参数值是中文,HTTP 层按
    /// UTF-8 百分号编码后上游可正确解析(2026-08-22 实测;curl 下需手工
    /// 编码,git bash 的 GBK 字节会被上游拒绝)。
    pub async fn suspensions(
        &self,
        date: NaiveDate,
        max_pages: u32,
    ) -> Result<Fetched<Vec<SuspendRow>>, DataError> {
        let key = format!("dc_suspend_{date}_{max_pages}");
        let query = ReportQuery {
            host: DC_WEB_HOST,
            report_name: "RPT_CUSTOM_SUSPEND_DATA_INTERFACE",
            columns: "ALL",
            filter: Some(format!("(MARKET=\"全部\")(DATETIME='{date}')")),
            sort_columns: "SUSPEND_START_DATE",
            sort_types: "-1",
            quote_columns: None,
            max_pages,
            op: "suspensions",
        };
        self.report(&key, &query, parse_suspend_row).await
    }

    /// 公告大全(`np-anotice-stock/api/security/ann`)。
    ///
    /// `stock_list` 为 None 时取全市场;`begin`/`end` 为公告日期区间(均可选)。
    /// 分页 `page_size=100` 固定,按 `total_hits` 推算页数。
    pub async fn notices(
        &self,
        stock_list: Option<&str>,
        node: NoticeNode,
        begin: Option<NaiveDate>,
        end: Option<NaiveDate>,
        max_pages: u32,
    ) -> Result<Fetched<Vec<NoticeRow>>, DataError> {
        let key = format!(
            "dc_notice_{}_{:?}_{:?}_{:?}_{}",
            stock_list.unwrap_or("*"),
            node,
            begin,
            end,
            max_pages
        );
        if let Some(hit) = self.cache.get::<Fetched<Vec<NoticeRow>>>(&key, REPORT_TTL) {
            return Ok(hit);
        }
        let url = format!("{NOTICE_HOST}/api/security/ann");
        let mut rows = Vec::new();
        let mut page = 1_u32;
        loop {
            let mut params = vec![
                ("sr".to_string(), "-1".to_string()),
                ("page_size".to_string(), NOTICE_PAGE_SIZE.to_string()),
                ("page_index".to_string(), page.to_string()),
                ("ann_type".to_string(), "A".to_string()),
                ("client_source".to_string(), "web".to_string()),
                ("f_node".to_string(), node.f_node().to_string()),
                ("s_node".to_string(), "0".to_string()),
            ];
            if let Some(stock) = stock_list {
                params.push(("stock_list".to_string(), stock.to_string()));
            }
            if let Some(b) = begin {
                params.push(("begin_time".to_string(), b.to_string()));
            }
            if let Some(e) = end {
                params.push(("end_time".to_string(), e.to_string()));
            }
            let value = self.http.get_json(&url, &params).await?;
            if value.get("success").and_then(|s| s.as_bool()) == Some(false) {
                return Err(DataError::Parse {
                    upstream: format!("{NOTICE_HOST} (notices)"),
                    message: "success=false".to_string(),
                });
            }
            let data = value
                .get("data")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            if data.is_null() {
                break;
            }
            let total_hits = data.get("total_hits").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
            let total_pages = total_hits.div_ceil(NOTICE_PAGE_SIZE);
            let list = data
                .get("list")
                .and_then(|l| l.as_array())
                .cloned()
                .unwrap_or_default();
            if list.is_empty() {
                break;
            }
            rows.extend(list);
            page += 1;
            if page > total_pages.min(max_pages) {
                break;
            }
        }
        let out = Fetched::now(
            rows.iter().map(parse_notice_row).collect::<Vec<_>>(),
            Source::EastMoney,
        );
        self.cache.set(&key, &out);
        Ok(out)
    }

    /// push2ex 池通用请求:`data.pool` 原始 JSON 行;`data` 为 null
    /// (非交易日/无数据)时返回空 Vec。`date` 格式 `YYYYMMDD`。
    async fn pool_rows(
        &self,
        path: &str,
        sort: &str,
        date: NaiveDate,
        op: &'static str,
    ) -> Result<Vec<serde_json::Value>, DataError> {
        let params = vec![
            ("ut".to_string(), POOL_UT.to_string()),
            ("dpt".to_string(), "wz.ztzt".to_string()),
            ("Pageindex".to_string(), "0".to_string()),
            ("pagesize".to_string(), "10000".to_string()),
            ("sort".to_string(), sort.to_string()),
            ("date".to_string(), date.format("%Y%m%d").to_string()),
        ];
        let url = format!("{PUSH2EX_HOST}/{path}");
        let value = self.http.get_json(&url, &params).await?;
        if value.get("rc").and_then(|r| r.as_i64()) != Some(0) {
            return Err(DataError::Parse {
                upstream: format!("{PUSH2EX_HOST} ({op})"),
                message: format!(
                    "rc != 0: {}",
                    value.get("rc").unwrap_or(&serde_json::Value::Null)
                ),
            });
        }
        let data = value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if data.is_null() {
            return Ok(Vec::new());
        }
        Ok(data
            .get("pool")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// 通用池获取:短 TTL 缓存 → 请求 → 逐行解析。
    async fn pool<T, F>(
        &self,
        cache_key: &str,
        path: &str,
        sort: &str,
        date: NaiveDate,
        op: &'static str,
        parse: F,
    ) -> Result<Fetched<Vec<T>>, DataError>
    where
        T: Serialize + serde::de::DeserializeOwned,
        F: Fn(&serde_json::Value) -> T,
    {
        if let Some(hit) = self.cache.get::<Fetched<Vec<T>>>(cache_key, ttl::REALTIME) {
            return Ok(hit);
        }
        let rows = self.pool_rows(path, sort, date, op).await?;
        let out = Fetched::now(
            rows.iter().map(parse).collect::<Vec<T>>(),
            Source::EastMoney,
        );
        self.cache.set(cache_key, &out);
        Ok(out)
    }

    /// 涨停股池(不含未中断连续一字涨停板的新股,不含 ST/科创板)。
    pub async fn zt_pool(&self, date: NaiveDate) -> Result<Fetched<Vec<ZtPoolRow>>, DataError> {
        let key = format!("dc_ztpool_{date}");
        self.pool(
            &key,
            "getTopicZTPool",
            "fbt:asc",
            date,
            "zt_pool",
            parse_zt_pool_row,
        )
        .await
    }

    /// 昨日涨停股池(上一交易日收盘涨停的股票今日表现)。
    pub async fn prev_zt_pool(
        &self,
        date: NaiveDate,
    ) -> Result<Fetched<Vec<PrevZtPoolRow>>, DataError> {
        let key = format!("dc_zrztpool_{date}");
        self.pool(
            &key,
            "getYesterdayZTPool",
            "zs:desc",
            date,
            "prev_zt_pool",
            parse_prev_zt_pool_row,
        )
        .await
    }

    /// 强势股池(60 日新高或近期多次涨停)。
    pub async fn strong_pool(
        &self,
        date: NaiveDate,
    ) -> Result<Fetched<Vec<StrongPoolRow>>, DataError> {
        let key = format!("dc_qspool_{date}");
        self.pool(
            &key,
            "getTopicQSPool",
            "zdp:desc",
            date,
            "strong_pool",
            parse_strong_pool_row,
        )
        .await
    }

    /// 次新股池(上市一年以内且中断连续一字涨停板)。
    pub async fn sub_new_pool(
        &self,
        date: NaiveDate,
    ) -> Result<Fetched<Vec<SubNewPoolRow>>, DataError> {
        let key = format!("dc_cxpool_{date}");
        self.pool(
            &key,
            "getTopicCXPooll",
            "ods:asc",
            date,
            "sub_new_pool",
            parse_sub_new_pool_row,
        )
        .await
    }

    /// 炸板股池(当日触板未封;上游仅保留最近 30 个交易日)。
    pub async fn broken_pool(
        &self,
        date: NaiveDate,
    ) -> Result<Fetched<Vec<BrokenPoolRow>>, DataError> {
        let key = format!("dc_zbpool_{date}");
        self.pool(
            &key,
            "getTopicZBPool",
            "fbt:asc",
            date,
            "broken_pool",
            parse_broken_pool_row,
        )
        .await
    }

    /// 跌停股池(上游仅保留最近 30 个交易日)。
    pub async fn dt_pool(&self, date: NaiveDate) -> Result<Fetched<Vec<DtPoolRow>>, DataError> {
        let key = format!("dc_dtpool_{date}");
        self.pool(
            &key,
            "getTopicDTPool",
            "fund:asc",
            date,
            "dt_pool",
            parse_dt_pool_row,
        )
        .await
    }

    /// 板块 clist 单页(走 push2 行情 host 池,带 `ut` token),返回 `(rows, total)`。
    async fn clist_page(
        &self,
        fs: &str,
        fields: &str,
        page: u32,
        op: &'static str,
    ) -> Result<(Vec<serde_json::Value>, u32), DataError> {
        let params = vec![
            ("po".to_string(), "1".to_string()),
            ("np".to_string(), "1".to_string()),
            ("fltt".to_string(), "2".to_string()),
            ("invt".to_string(), "2".to_string()),
            ("fields".to_string(), fields.to_string()),
            ("fs".to_string(), fs.to_string()),
            ("pz".to_string(), CLIST_PAGE_SIZE.to_string()),
            ("pn".to_string(), page.to_string()),
            ("ut".to_string(), EM_TOKEN.to_string()),
        ];
        let value = self
            .http
            .get_json_pool("/api/qt/clist/get", &params, &QUOTE_HOSTS, op)
            .await?;
        let d = value
            .get("data")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let total = d.get("total").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
        // `diff` 通常为数组;个别响应用按序号键控的对象——两种都接受
        // (与 `eastmoney.rs` 的 clist 处理一致)。
        let diff = match d.get("diff") {
            Some(serde_json::Value::Array(a)) => a.clone(),
            Some(serde_json::Value::Object(o)) => o.values().cloned().collect(),
            _ => Vec::new(),
        };
        Ok((diff, total))
    }

    /// clist 循环分页(pz=500)。
    async fn clist_rows(
        &self,
        fs: &str,
        fields: &str,
        max_pages: u32,
        op: &'static str,
    ) -> Result<Vec<serde_json::Value>, DataError> {
        let (first, total) = self.clist_page(fs, fields, 1, op).await?;
        let mut rows = first;
        let last = total.div_ceil(CLIST_PAGE_SIZE).min(max_pages);
        for page in 2..=last {
            let (mut more, _) = self.clist_page(fs, fields, page, op).await?;
            if more.is_empty() {
                break;
            }
            rows.append(&mut more);
        }
        Ok(rows)
    }

    /// 板块列表(行业 `m:90+t:2` / 概念 `m:90+t:3`),含领涨股与主力净流入。
    /// 2026-08-21 实测行业 496 / 概念 504 个,按单页 100 行需 5~6 页。
    pub async fn board_list(&self, kind: BoardKind) -> Result<Fetched<Vec<BoardRow>>, DataError> {
        let key = kind.cache_key();
        if let Some(hit) = self.cache.get::<Fetched<Vec<BoardRow>>>(key, ttl::ALL_A) {
            return Ok(hit);
        }
        let rows = self
            .clist_rows(kind.fs(), "f12,f14,f2,f3,f62,f128,f140", 8, "board_list")
            .await?;
        if rows.is_empty() {
            return Err(DataError::Empty(format!("board_list {:?}", kind)));
        }
        let out = Fetched::now(
            rows.iter().map(parse_board_row).collect::<Vec<_>>(),
            Source::EastMoney,
        );
        self.cache.set(key, &out);
        Ok(out)
    }

    /// 板块成分股(`fs=b:{BK代码}`,如 `b:BK0475` 银行)。
    pub async fn board_cons(
        &self,
        board_code: &str,
        max_pages: u32,
    ) -> Result<Fetched<Vec<BoardConsRow>>, DataError> {
        let key = format!("dc_boardcons_{board_code}_{max_pages}");
        if let Some(hit) = self
            .cache
            .get::<Fetched<Vec<BoardConsRow>>>(&key, ttl::ALL_A)
        {
            return Ok(hit);
        }
        let fs = format!("b:{board_code}");
        let rows = self
            .clist_rows(&fs, "f12,f14,f2,f3,f9,f20,f21", max_pages, "board_cons")
            .await?;
        if rows.is_empty() {
            return Err(DataError::Empty(format!("board_cons {board_code}")));
        }
        let out = Fetched::now(
            rows.iter().map(parse_board_cons_row).collect::<Vec<_>>(),
            Source::EastMoney,
        );
        self.cache.set(&key, &out);
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 读取 tests/fixtures/em_datacenter 下的真实响应样例
    /// (2026-08-22 实测 curl 抓取;全部为公开市场数据,无个人信息,无需脱敏)。
    fn fixture(name: &str) -> serde_json::Value {
        let path = format!(
            "{}/tests/fixtures/em_datacenter/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let body =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"));
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("parse fixture {path}: {e}"))
    }

    fn report_rows(value: &serde_json::Value) -> Vec<serde_json::Value> {
        value
            .get("result")
            .and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default()
    }

    fn pool_rows(value: &serde_json::Value) -> Vec<serde_json::Value> {
        value
            .get("data")
            .and_then(|d| d.get("pool"))
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default()
    }

    // --- 报表 fixture 金标测试(数值来自 2026-08-21/22 实盘响应) ---

    #[test]
    fn billboard_fixture_golden() {
        let rows = report_rows(&fixture("lhb.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_billboard_row(&rows[0]);
        assert_eq!(row.code, "000017");
        assert_eq!(row.secucode, "000017.SZ");
        assert_eq!(row.name, "深中华A");
        assert_eq!(row.trade_date.unwrap().to_string(), "2026-08-21");
        assert_eq!(row.close_price, Some(7.11));
        assert_eq!(row.change_rate, Some(10.0619));
        assert_eq!(row.net_amt, Some(4_599_091.67));
        assert_eq!(row.buy_amt, Some(104_887_598.01));
        assert_eq!(row.sell_amt, Some(100_288_506.34));
        assert_eq!(row.accum_amount, Some(403_808_386.0));
        assert_eq!(row.turnover_rate, Some(9.4301));
        assert_eq!(row.free_market_cap, Some(3_524_173_953.35));
        assert_eq!(
            row.explanation,
            "连续三个交易日内，涨幅偏离值累计达到20%的证券"
        );
        // D1/D2/D5/D10 对最近上榜日为 null。
        assert!(row.d1_change.is_none());
        // 恒等式:净买额 = 买入额 - 卖出额。
        let net = row.buy_amt.unwrap() - row.sell_amt.unwrap();
        assert!((net - row.net_amt.unwrap()).abs() < 0.01);
    }

    #[test]
    fn block_trade_fixture_golden() {
        let rows = report_rows(&fixture("blocktrade.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_block_trade_row(&rows[0]);
        assert_eq!(row.code, "000007");
        assert_eq!(row.name, "全新好");
        assert_eq!(row.trade_date.unwrap().to_string(), "2026-08-18");
        assert_eq!(row.deal_price, Some(11.68));
        assert_eq!(row.close_price, Some(11.69));
        assert_eq!(row.deal_volume, Some(900_000.0));
        assert_eq!(row.deal_amt, Some(10_512_000.0));
        // 单位校验:成交量(股) × 成交价(元) == 成交额(元)。
        let amt = row.deal_volume.unwrap() * row.deal_price.unwrap();
        assert!((amt - row.deal_amt.unwrap()).abs() < 1.0);
        // 折溢率为小数比率:≈ 成交价/收盘价 - 1。
        let premium = row.deal_price.unwrap() / row.close_price.unwrap() - 1.0;
        assert!((premium - row.premium_ratio.unwrap()).abs() < 1e-4);
        assert!(row.buyer_name.contains("华鑫证券"));
    }

    #[test]
    fn margin_fixture_golden() {
        let rows = report_rows(&fixture("margin.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_margin_daily_row(&rows[0]);
        assert_eq!(row.statistics_date.unwrap().to_string(), "2026-08-20");
        // 亿元口径:融资余额 2.6 万亿 → 26395.00368567 亿。
        assert_eq!(row.fin_balance_yi, Some(26_395.003_685_67));
        assert_eq!(row.loan_balance_yi, Some(268.452_946_96));
        assert_eq!(row.margin_balance_yi, Some(26_663.456_632_64));
        // 恒等式:两融余额 = 融资余额 + 融券余额。
        let total = row.fin_balance_yi.unwrap() + row.loan_balance_yi.unwrap();
        assert!((total - row.margin_balance_yi.unwrap()).abs() < 0.01);
        // 个人投资者为万名口径(837.5 万名)。
        assert_eq!(row.personal_investor_num, Some(837.5079));
        assert_eq!(row.avg_guarantee_ratio, Some(277.7545));
    }

    #[test]
    fn org_survey_fixture_golden() {
        let rows = report_rows(&fixture("org_survey.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_org_survey_row(&rows[0]);
        assert_eq!(row.code, "300761");
        assert_eq!(row.name, "立华股份");
        assert_eq!(row.notice_date.unwrap().to_string(), "2026-08-22");
        assert_eq!(row.receive_start_date.unwrap().to_string(), "2026-08-21");
        assert!(row.receive_object.contains("中信证券"));
        assert_eq!(row.receive_place, "电话会议");
        assert!(row.receive_way_explain.contains("电话会议"));
    }

    #[test]
    fn holder_num_fixture_golden() {
        let rows = report_rows(&fixture("holdernum.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_holder_num_row(&rows[0]);
        assert_eq!(row.code, "688707");
        assert_eq!(row.name, "振华新材");
        assert_eq!(row.end_date.unwrap().to_string(), "2026-06-30");
        assert_eq!(row.pre_end_date.unwrap().to_string(), "2026-05-08");
        assert_eq!(row.holder_num, Some(20_275.0));
        assert_eq!(row.pre_holder_num, Some(23_530.0));
        assert_eq!(row.holder_num_change, Some(-3_255.0));
        // 恒等式:户数增减 = 本次 - 上次。
        let change = row.holder_num.unwrap() - row.pre_holder_num.unwrap();
        assert!((change - row.holder_num_change.unwrap()).abs() < f64::EPSILON);
        // 增减比例 ≈ 增减/上次 × 100。
        let ratio = change / row.pre_holder_num.unwrap() * 100.0;
        assert!((ratio - row.holder_num_ratio.unwrap()).abs() < 0.01);
        assert_eq!(row.total_a_shares, Some(508_740_205.0));
    }

    #[test]
    fn earnings_predict_fixture_golden() {
        let rows = report_rows(&fixture("predict.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_earnings_predict_row(&rows[0]);
        assert_eq!(row.code, "600187");
        assert_eq!(row.name, "*ST国中");
        assert_eq!(row.notice_date.unwrap().to_string(), "2026-08-19");
        assert_eq!(row.report_date.unwrap().to_string(), "2026-06-30");
        assert_eq!(row.predict_finance, "归属于上市公司股东的净利润");
        assert_eq!(row.predict_type, "扭亏");
        assert_eq!(row.predict_amt_lower, Some(2_650_000.0));
        assert_eq!(row.predict_amt_upper, Some(3_150_000.0));
        assert_eq!(row.add_amp_lower, Some(114.47));
        assert_eq!(row.add_amp_upper, Some(117.19));
        assert_eq!(row.preyear_same_period, Some(-18_320_000.0));
        assert!(row.predict_content.contains("265万元至315万元"));
    }

    #[test]
    fn lift_stage_fixture_unit_conversion() {
        let rows = report_rows(&fixture("lift.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_lift_stage_row(&rows[0]);
        assert_eq!(row.code, "002157");
        assert_eq!(row.name, "正邦科技");
        assert_eq!(row.free_date.unwrap().to_string(), "2026-08-24");
        // 万股 → 股:22.3567 万股 → 223_567 股。
        assert_eq!(row.current_free_shares, Some(223_567.0));
        // 万元 → 元:65.505131 万元 → 655_051.31 元。
        assert_eq!(row.lift_market_cap, Some(655_051.31));
        assert_eq!(row.pre_close, Some(2.93));
        // 交叉校验:解禁股数 × 收盘价 ≈ 解禁市值。
        let cap = row.current_free_shares.unwrap() * row.pre_close.unwrap();
        assert!((cap - row.lift_market_cap.unwrap()).abs() / cap < 0.001);
        assert_eq!(row.free_shares_type, "其他类型");
        // 未来解禁:B20/A20 涨跌幅为 null。
        assert!(row.b20_change.is_none());
    }

    #[test]
    fn suspend_fixture_golden() {
        let rows = report_rows(&fixture("suspend.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_suspend_row(&rows[0]);
        assert_eq!(row.code, "200016");
        assert_eq!(row.name, "*ST康佳B");
        assert_eq!(row.suspend_start_date.unwrap().to_string(), "2026-08-24");
        assert_eq!(row.suspend_expire, "连续停牌");
        assert_eq!(row.suspend_reason, "刊登重要公告");
        assert_eq!(row.trade_market, "深交所风险警示板");
        // SUSPEND_END_TIME / PREDICT_RESUME_DATE 为 null。
        assert_eq!(row.suspend_end_time, "");
        assert!(row.predict_resume_date.is_none());
    }

    #[test]
    fn notice_fixture_golden() {
        let value = fixture("notice.json");
        let list = value
            .get("data")
            .and_then(|d| d.get("list"))
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(list.len(), 3);
        let row = parse_notice_row(&list[0]);
        assert_eq!(row.art_code, "AN202608141827994407");
        assert_eq!(row.stock_code, "600519");
        assert_eq!(row.stock_name, "贵州茅台");
        assert_eq!(row.notice_date.unwrap().to_string(), "2026-08-15");
        assert_eq!(row.column_name, "其他");
        assert!(row.title.contains("业绩说明会"));
        assert_eq!(
            row.url,
            "https://data.eastmoney.com/notices/detail/600519/AN202608141827994407.html"
        );
    }

    // --- push2ex 六池 fixture 金标测试 ---

    #[test]
    fn zt_pool_fixture_golden() {
        let rows = pool_rows(&fixture("pool_zt.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_zt_pool_row(&rows[0]);
        assert_eq!(row.code, "000017");
        assert_eq!(row.name, "深中华A");
        // p=7110(厘)÷1000 → 7.11 元。
        assert_eq!(row.price, Some(7.11));
        assert_eq!(row.amount, Some(294_182_288.0));
        assert_eq!(row.lock_fund, Some(39_809_601.0));
        assert_eq!(row.limit_times, Some(2));
        // fbt=92500 → "09:25:00"(前导零补齐)。
        assert_eq!(row.first_lock_time, "09:25:00");
        assert_eq!(row.last_lock_time, "09:39:09");
        assert_eq!(row.break_times, Some(1));
        assert_eq!(row.limit_stat, Some(LimitStat { days: 2, ct: 2 }));
        assert_eq!(row.industry, "饰品");
    }

    #[test]
    fn prev_zt_pool_fixture_golden() {
        let rows = pool_rows(&fixture("pool_zrzt.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_prev_zt_pool_row(&rows[0]);
        assert_eq!(row.code, "920344");
        assert_eq!(row.price, Some(26.8));
        assert_eq!(row.limit_price, Some(32.18));
        assert_eq!(row.yesterday_lock_time, "09:30:30");
        assert_eq!(row.yesterday_limit_times, Some(1));
        assert_eq!(row.limit_stat, Some(LimitStat { days: 2, ct: 1 }));
    }

    #[test]
    fn strong_pool_fixture_golden() {
        let rows = pool_rows(&fixture("pool_qs.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_strong_pool_row(&rows[0]);
        assert_eq!(row.code, "688185");
        assert_eq!(row.name, "康希诺");
        assert_eq!(row.price, Some(77.32));
        assert_eq!(row.limit_price, Some(77.32));
        assert!(row.is_new_high);
        // ztf="1" → 60 日新高。
        assert_eq!(row.reason, StrongReason::NewHigh60);
        assert_eq!(row.volume_ratio, Some(10.113_892_555_236_816));
    }

    #[test]
    fn sub_new_pool_fixture_ztp_sentinel() {
        let rows = pool_rows(&fixture("pool_cx.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_sub_new_pool_row(&rows[0]);
        assert_eq!(row.code, "920093");
        assert_eq!(row.price, Some(23.19));
        // ztp=1_000_000_000 哨兵(÷1000 = 100 万元,>100000)→ None。
        assert_eq!(row.limit_price, None);
        assert_eq!(row.open_days, Some(1));
        assert_eq!(row.open_date.unwrap().to_string(), "2026-08-21");
        assert_eq!(row.ipo_date.unwrap().to_string(), "2026-08-21");
        assert!(!row.is_new_high);
    }

    #[test]
    fn broken_pool_fixture_golden() {
        let rows = pool_rows(&fixture("pool_zb.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_broken_pool_row(&rows[0]);
        assert_eq!(row.code, "600250");
        assert_eq!(row.price, Some(9.15));
        assert_eq!(row.limit_price, Some(10.16));
        assert_eq!(row.first_lock_time, "09:30:30");
        assert_eq!(row.break_times, Some(1));
        assert!((row.amplitude.unwrap() - 11.255_411_148_071_289).abs() < 1e-9);
    }

    #[test]
    fn dt_pool_fixture_golden() {
        let rows = pool_rows(&fixture("pool_dt.json"));
        assert_eq!(rows.len(), 3);
        let row = parse_dt_pool_row(&rows[0]);
        assert_eq!(row.code, "603102");
        assert_eq!(row.name, "百合股份");
        assert_eq!(row.price, Some(39.72));
        assert!((row.pe_dynamic.unwrap() - 20.476_640_701_293_945).abs() < 1e-9);
        assert_eq!(row.lock_fund, Some(4_202_376.0));
        assert_eq!(row.last_lock_time, "15:00:00");
        assert_eq!(row.board_amount, Some(47_771_244.0));
        assert_eq!(row.limit_down_days, Some(1));
        assert_eq!(row.open_times, Some(13));
    }

    // --- 板块 clist fixture 金标测试 ---

    #[test]
    fn board_fixtures_golden() {
        let industry = fixture("board_industry.json");
        let diff = industry
            .get("data")
            .and_then(|d| d.get("diff"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(diff.len(), 3);
        let row = parse_board_row(&diff[0]);
        assert_eq!(row.code, "BK0420");
        assert_eq!(row.name, "航空机场");
        assert_eq!(row.price, Some(4002.71));
        assert_eq!(row.pct, Some(-1.05));
        assert_eq!(row.lead_stock, "海控B股");
        assert_eq!(row.lead_stock_code, "900945");

        let cons = fixture("board_cons.json");
        let diff = cons
            .get("data")
            .and_then(|d| d.get("diff"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();
        assert_eq!(diff.len(), 3);
        let row = parse_board_cons_row(&diff[0]);
        assert_eq!(row.code, "601288");
        assert_eq!(row.name, "农业银行");
        assert_eq!(row.price, Some(6.78));
        assert_eq!(row.pe, Some(7.89));
        assert_eq!(row.total_market_cap, Some(2_372_884_969_659.0));
    }

    // --- 解析辅助函数单测 ---

    #[test]
    fn pool_time_zero_pads_and_formats() {
        let row = serde_json::json!({"fbt": 92500, "lbt": 150000, "bad": null});
        assert_eq!(pool_time(&row, "fbt"), "09:25:00");
        assert_eq!(pool_time(&row, "lbt"), "15:00:00");
        assert_eq!(pool_time(&row, "bad"), "");
        assert_eq!(pool_time(&row, "missing"), "");
    }

    #[test]
    fn pool_price_divides_by_1000() {
        let row = serde_json::json!({"p": 7110, "ztp": "13160", "null_p": null});
        assert_eq!(pool_price(&row, "p"), Some(7.11));
        assert_eq!(pool_price(&row, "ztp"), Some(13.16));
        assert_eq!(pool_price(&row, "null_p"), None);
        assert_eq!(pool_price(&row, "missing"), None);
    }

    #[test]
    fn jdate_variants() {
        let row = serde_json::json!({
            "a": "2026-08-21 00:00:00",
            "b": "2026-08-21",
            "c": null,
            "d": "n/a",
            "num": 20260821,
            "zero": 0,
        });
        assert_eq!(jdate(&row, "a").unwrap().to_string(), "2026-08-21");
        assert_eq!(jdate(&row, "b").unwrap().to_string(), "2026-08-21");
        assert!(jdate(&row, "c").is_none());
        assert!(jdate(&row, "d").is_none());
        assert!(jdate(&row, "missing").is_none());
        assert_eq!(
            jdate_compact(&row, "num").unwrap().to_string(),
            "2026-08-21"
        );
        assert!(jdate_compact(&row, "zero").is_none());
    }

    #[test]
    fn null_tolerant_rows() {
        // 全 null 行不应 panic,字段全部为空/None。
        let row = serde_json::json!({});
        let b = parse_billboard_row(&row);
        assert_eq!(b.code, "");
        assert!(b.trade_date.is_none());
        assert!(b.net_amt.is_none());
        let z = parse_zt_pool_row(&row);
        assert_eq!(z.code, "");
        assert!(z.price.is_none());
        assert_eq!(z.first_lock_time, "");
        assert!(z.limit_stat.is_none());
        let n = parse_notice_row(&row);
        assert_eq!(n.stock_code, "");
        assert_eq!(n.column_name, "");
    }

    #[test]
    fn notice_node_mapping() {
        assert_eq!(NoticeNode::All.f_node(), "0");
        assert_eq!(NoticeNode::Financial.f_node(), "1");
        assert_eq!(NoticeNode::HoldingChange.f_node(), "7");
    }

    #[test]
    fn board_kind_fs() {
        assert_eq!(BoardKind::Industry.fs(), "m:90+t:2");
        assert_eq!(BoardKind::Concept.fs(), "m:90+t:3");
    }

    // --- 实盘冒烟(默认跳过;`cargo test -p astock-market-data -- --ignored` 运行) ---

    fn live_dc() -> EmDataCenter {
        EmDataCenter::new(Arc::new(HttpClient::new()), Arc::new(TtlCache::default()))
    }

    fn today() -> NaiveDate {
        astock_core::time::utc_now().date_naive()
    }

    #[tokio::test]
    #[ignore = "live smoke: hits datacenter-web.eastmoney.com"]
    async fn live_billboard_detail() {
        let dc = live_dc();
        let end = today();
        let start = end - chrono::Duration::days(14);
        let out = dc.billboard_detail(start, end, 4).await.expect("billboard");
        assert!(
            !out.data.is_empty(),
            "billboard should have rows in the last 14 days"
        );
        let row = &out.data[0];
        assert_eq!(row.code.len(), 6);
        assert!(row.trade_date.is_some());
        // 交叉校验抽样:净买额 = 买入 - 卖出。
        let checked = out
            .data
            .iter()
            .filter_map(|r| match (r.net_amt, r.buy_amt, r.sell_amt) {
                (Some(n), Some(b), Some(s)) => Some((n, b, s)),
                _ => None,
            })
            .take(20)
            .all(|(n, b, s)| (b - s - n).abs() < 0.01);
        assert!(checked, "net_amt != buy_amt - sell_amt");
        println!("billboard rows: {}", out.data.len());
    }

    #[tokio::test]
    #[ignore = "live smoke: hits push2ex.eastmoney.com"]
    async fn live_zt_pool() {
        let dc = live_dc();
        // 周末/节假日回溯最多 10 天找一个有涨停池的交易日。
        let mut pool = Vec::new();
        for back in 0..10 {
            let date = today() - chrono::Duration::days(back);
            pool = dc.zt_pool(date).await.expect("zt_pool").data;
            if !pool.is_empty() {
                println!("zt_pool date={date} rows={}", pool.len());
                break;
            }
        }
        assert!(!pool.is_empty(), "no zt pool rows in the last 10 days");
        for row in pool.iter().take(20) {
            assert_eq!(row.code.len(), 6);
            let price = row.price.expect("price");
            assert!(price > 0.0 && price < 10_000.0, "price sanity: {price}");
            assert!(
                row.pct.unwrap_or(0.0) > 0.0,
                "zt pool pct should be positive"
            );
        }
        // 涨停池的行按定义接近涨停:涨幅至少 4%(ST 5%/主板 10%/创业科创 20%/北交 30%)。
        let min_pct = pool
            .iter()
            .filter_map(|r| r.pct)
            .fold(f64::INFINITY, f64::min);
        assert!(min_pct > 4.0, "min pct in zt pool: {min_pct}");
    }

    #[tokio::test]
    #[ignore = "live smoke: hits push2 clist"]
    async fn live_board_list_and_cons() {
        let dc = live_dc();
        let industry = dc.board_list(BoardKind::Industry).await.expect("industry");
        assert!(
            industry.data.len() > 50,
            "industry boards: {}",
            industry.data.len()
        );
        for row in industry.data.iter().take(10) {
            assert!(row.code.starts_with("BK"), "board code: {}", row.code);
            assert!(!row.name.is_empty());
        }
        let concept = dc.board_list(BoardKind::Concept).await.expect("concept");
        assert!(
            concept.data.len() > 50,
            "concept boards: {}",
            concept.data.len()
        );
        // 用第一个行业板块代码取成分股。
        let bk = &industry.data[0].code;
        let cons = dc.board_cons(bk, 4).await.expect("board cons");
        assert!(!cons.data.is_empty(), "cons of {bk}");
        for row in cons.data.iter().take(5) {
            assert_eq!(row.code.len(), 6);
            assert!(!row.name.is_empty());
        }
        println!(
            "industry={} concept={} cons({bk})={}",
            industry.data.len(),
            concept.data.len(),
            cons.data.len()
        );
    }
}
