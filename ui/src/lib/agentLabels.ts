/** User-facing Chinese labels. Protocol/tool identifiers must never leak into the retail UI. */
const TOOL_NAMES: Record<string, string> = {
  get_quote: "实时行情获取",
  get_kline: "多周期 K 线加载",
  compute_indicators: "技术指标计算",
  run_full_analysis: "综合交易信号分析",
  run_chanlun: "缠论结构分析",
  get_fund_flow: "资金流向分析",
  get_market_breadth: "市场涨跌概况分析",
  search_stock: "股票资料检索",
  compare_stocks: "同类股票对比",
  scan_market: "全市场机会扫描",
  scan_stock: "候选股票筛选",
  get_cached_detail: "候选股票明细核验",
  get_watchlist: "自选股组合检查",
  get_fundamentals: "基本面质量分析",
  get_valuation: "估值区间分析",
  run_valuation: "估值区间分析",
  get_industry_chain: "产业链位置分析",
  run_supply_chain_shock: "产业链冲击推演",
  build_relationship_graph: "股票关系网络分析",
  run_backtest: "历史回测验证",
  iterate_strategy: "策略迭代与稳健性检验",
  run_joinquant_research: "聚宽研究数据核验",
  search_web: "联网检索权威资料",
  fetch_source_document: "打开并核验原始来源",
  read_document: "读取原文证据位置",
  compare_source_evidence: "多来源字段对账",
  research_news: "财经新闻与公告研究",
  research_disclosures: "正式披露与修订核验",
  research_global_transmission: "海外一级来源与 A 股传导核验",
  analyze_event_price_in: "结构化事件与市场定价核验",
  get_market_regime: "市场环境识别",
  market_regime: "市场环境识别",
  industry_chain: "产业链位置分析",
  relationship_graph: "股票关系网络分析",
};

const ARG_NAMES: Record<string, string> = {
  symbol: "股票代码",
  symbols: "股票范围",
  query: "检索内容",
  question: "分析问题",
  period: "K 线周期",
  periods: "K 线周期",
  adjust: "复权方式",
  limit: "数据条数",
  top_n: "候选数量",
  start: "开始日期",
  end: "结束日期",
  start_date: "开始日期",
  end_date: "结束日期",
  strategy: "策略类型",
  params: "策略条件",
  initial_cash: "初始资金",
  event: "冲击事件",
  event_keywords: "事件关键词",
  max_depth: "分析层级",
  threshold: "筛选门槛",
  risk_budget: "风险预算",
  study: "研究项目",
  stock: "股票",
  sources: "新闻来源",
  keyword: "新闻关键词",
  url: "原始来源网址",
  source_version_id: "原文版本",
  source_version_ids: "原文版本范围",
  page_number: "PDF 页码",
  paragraph_index: "段落序号",
  important_only: "仅看重要快讯",
  date: "截面日期",
  index: "指数代码",
  spec: "可审计策略条件",
  root_entity_id: "海外实体",
  provider_id: "海外官方来源",
  as_of_utc: "历史截面时间",
  revision_id: "来源修订",
  security_code: "关联股票",
  structured_impact_bps: "经营影响估计",
  consensus_impact_bps: "市场一致预期",
};

const VALUE_NAMES: Record<string, string> = {
  day: "日线",
  week: "周线",
  month: "月线",
  minute: "分时",
  qfq: "前复权",
  hfq: "后复权",
  none: "不复权",
  ma_cross: "均线交叉策略",
  turtle: "海龟突破策略",
  buy_hold: "买入并持有",
  zscore_mean_reversion: "均值回归策略",
  min_corr_etf_rotation: "低相关基金轮动策略",
  formula_dsl: "AI 公式策略",
  daily: "历史日线",
  valuation: "历史估值截面",
  index_components: "指数成分",
  macro_cpi: "宏观居民消费价格指数",
};

export function toolDisplayName(name: string | null | undefined): string {
  return (name && TOOL_NAMES[name]) || "扩展分析步骤";
}

export function sourceDisplayName(source: string | null | undefined): string {
  if (!source?.trim()) return "已配置数据源";
  const s = source.trim();
  const lower = s.toLowerCase();
  if (lower.includes("tdx") && lower.includes("eastmoney")) return "通达信与东方财富联合数据";
  if (lower.includes("eastmoney_f10") || lower.includes("eastmoney f10")) return "东方财富公司资料";
  if (lower.includes("eastmoney_quote")) return "东方财富实时行情";
  if (lower.includes("eastmoney")) return "东方财富市场数据";
  if (lower.includes("tdx")) return "通达信行情数据";
  if (lower.includes("joinquant")) return "聚宽研究数据";
  if (lower.includes("minimax_web_search")) return "MiniMax 联网检索";
  if (lower.includes("finance_news") && lower.includes("iwencai")) return "财经快讯与问财事件数据";
  if (lower.includes("graph")) return "本地产业关系图谱";
  if (lower.includes("technical")) return "本地技术分析引擎";
  if (lower.includes("cache") || lower.includes("storage")) return "本地数据快照";
  if (/^[\x00-\x7f]+$/.test(s)) return "已配置研究数据源";
  return s;
}

export function fetchedAtDisplay(value: string | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : date.toLocaleString("zh-CN", { hour12: false });
}

export interface DisplayArgument {
  label: string;
  value: string;
}

function displayValue(value: unknown): string {
  if (value == null) return "未指定";
  if (typeof value === "boolean") return value ? "是" : "否";
  if (typeof value === "string") return VALUE_NAMES[value] ?? value;
  if (Array.isArray(value)) return value.map(displayValue).join("、");
  if (typeof value === "object") {
    return Object.entries(value as Record<string, unknown>)
      .map(([key, item]) => `${ARG_NAMES[key] ?? "条件"}：${displayValue(item)}`)
      .join("；");
  }
  return String(value);
}

export function toolArgumentsDisplay(raw: string | null | undefined): DisplayArgument[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return [{ label: "分析条件", value: displayValue(parsed) }];
    }
    return Object.entries(parsed as Record<string, unknown>).map(([key, value]) => ({
      label: ARG_NAMES[key] ?? "其他分析条件",
      value: displayValue(value),
    }));
  } catch {
    return [{ label: "分析条件", value: "已由智能助手自动配置" }];
  }
}
