import { useEffect, useState } from "react";
import { errMsg, getMarketBreadth, getQuote, type MarketBreadth, type Quote } from "../lib/api";
import { fmtNum, fmtPct, fmtVolume, fmtYiWan, pctClass } from "../lib/format";
import { useResearchContext } from "./store";

export default function QuoteInspector() {
  const symbol = useResearchContext((state) => state.symbol);
  const [quote, setQuote] = useState<Quote | null>(null);
  const [breadth, setBreadth] = useState<MarketBreadth | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    const load = () => {
      if (symbol) {
        getQuote(symbol).then((value) => { if (live) { setQuote(value); setError(null); } }).catch((cause) => live && setError(errMsg(cause)));
      } else {
        getMarketBreadth().then((value) => { if (live) { setBreadth(value); setError(null); } }).catch((cause) => live && setError(errMsg(cause)));
      }
    };
    load();
    const timer = window.setInterval(load, symbol ? 3_000 : 30_000);
    return () => { live = false; window.clearInterval(timer); };
  }, [symbol]);

  if (!symbol) return <div className="quote-inspector-body">
    <section className="quote-summary"><span className="eyebrow">市场温度</span><h2>A股全市场</h2>{breadth ? <><div className="breadth-numbers"><b className="text-up">{breadth.up}</b><span>上涨</span><b className="text-down">{breadth.down}</b><span>下跌</span></div><div className="breadth-track"><i style={{ width: `${breadth.total ? breadth.up / breadth.total * 100 : 50}%` }} /></div><dl><dt>平盘</dt><dd>{breadth.flat}</dd><dt>统计证券</dt><dd>{breadth.total}</dd><dt>上涨占比</dt><dd>{fmtPct(breadth.breadth_ratio * 100)}</dd></dl></> : <p className="muted">正在同步市场宽度…</p>}</section>
    {error && <p className="inspector-error">{error}</p>}
    <section><h3>快速开始</h3><ol className="quick-start"><li>在顶部输入股票代码或名称</li><li>回车打开个股 K 线与行情</li><li>从自选列表快速切换证券</li></ol></section>
    <section className="inspector-policy"><h3>研究边界</h3><p>只提供研究结论与人工审阅计划，不连接交易账户，不自动下单。</p></section>
  </div>;

  return <div className="quote-inspector-body">
    <section className="quote-summary"><span className="eyebrow">实时行情</span>{quote ? <><h2>{quote.name || symbol}<small>{symbol}</small></h2><div className={`quote-price ${pctClass(quote.pct)}`}>{fmtNum(quote.price)}</div><div className={`quote-change ${pctClass(quote.pct)}`}>{quote.change >= 0 ? "+" : ""}{fmtNum(quote.change)}&nbsp;&nbsp;{fmtPct(quote.pct)}</div><dl><dt>今开</dt><dd>{fmtNum(quote.open)}</dd><dt>最高</dt><dd className="text-up">{fmtNum(quote.high)}</dd><dt>最低</dt><dd className="text-down">{fmtNum(quote.low)}</dd><dt>昨收</dt><dd>{fmtNum(quote.pre_close)}</dd><dt>成交量</dt><dd>{fmtVolume(quote.volume)}</dd><dt>成交额</dt><dd>{fmtYiWan(quote.amount)}</dd><dt>换手</dt><dd>{fmtPct(quote.turnover, 2, false)}</dd></dl></> : <p className="muted">正在读取 {symbol} 行情…</p>}</section>
    {error && <p className="inspector-error">{error}</p>}
    <section><div className="inspector-actions"><button>加入自选</button><button>个股研究</button></div></section>
    <section><h3>盘口说明</h3><p>逐笔与五档盘口将在对应 Engine 服务迁移完成后显示；缺失数据不会用 0 伪装。</p></section>
  </div>;
}
