# -*- coding: utf-8 -*-
"""Generate golden-test fixtures from the legacy Python implementation.

Replays the legacy analysis pipeline for 8 benchmark stocks x {day, week}
and dumps {inputs, outputs} JSON so the Rust port can be verified offline.
Run:  python gen_golden.py
Output: ./golden/{symbol}_{period}.json
"""
import json
import os
import sys
import dataclasses

HERE = os.path.dirname(os.path.abspath(__file__))
LEGACY = os.path.abspath(os.path.join(HERE, "..", "..", "legacy-reference"))
sys.path.insert(0, os.path.join(LEGACY, "libs"))
sys.path.insert(0, LEGACY)

OUT = os.path.join(HERE, "golden")
os.makedirs(OUT, exist_ok=True)

SYMBOLS = ["600519", "000001", "600036", "300750", "601318", "000858", "600900", "002594"]
PERIODS = ["day", "week"]

from data.kline_fetcher import (
    fetch_kline, fetch_quote, fetch_fund_flow, fetch_index_kline, fetch_market_breadth,
)
from analysis.signal_engine import run_analysis
from analysis.chanlun_daily import analyze_chanlun_daily, daily_result_to_dict
import app as legacy_app


def to_jsonable(o):
    if dataclasses.is_dataclass(o) and not isinstance(o, type):
        return {k: to_jsonable(v) for k, v in dataclasses.asdict(o).items()}
    if isinstance(o, dict):
        return {k: to_jsonable(v) for k, v in o.items()}
    if isinstance(o, (list, tuple)):
        return [to_jsonable(v) for v in o]
    if isinstance(o, float):
        # stable serialization; Rust side compares with tolerance
        return round(o, 6)
    return o


def main():
    index_klines = fetch_index_kline("000001", 60)
    breadth = fetch_market_breadth()
    print(f"index klines: {len(index_klines) if index_klines else 0}, breadth: {breadth}")

    for symbol in SYMBOLS:
        quote = fetch_quote(symbol)
        flows = fetch_fund_flow(symbol, days=30)
        for period in PERIODS:
            klines = fetch_kline(symbol, count=250, period=period)
            if len(klines) < 30:
                print(f"SKIP {symbol} {period}: only {len(klines)} klines")
                continue

            result = run_analysis(klines, quote, flows, index_klines)
            signal_data = legacy_app.signal_to_dict(result)
            # replicate app.py breadth M-score adjustment
            if breadth and signal_data.get("canslim") and breadth.get("total", 0) >= 50:
                br = breadth.get("breadth_ratio", 0.5)
                bonus = 15 if br >= 0.7 else 10 if br >= 0.6 else 5 if br >= 0.5 else -5 if br >= 0.4 else -10 if br >= 0.3 else -15
                signal_data["canslim"]["m_score"] = max(0, min(100, signal_data["canslim"]["m_score"] + bonus))
            signal_data = legacy_app._apply_signal_optimization(signal_data, klines, quote)

            dates = [k.date for k in klines]
            chanlun = analyze_chanlun_daily(
                dates,
                [k.open for k in klines],
                [k.close for k in klines],
                [k.high for k in klines],
                [k.low for k in klines],
                [k.volume for k in klines],
            )
            chanlun_dict = daily_result_to_dict(chanlun)

            fixture = {
                "meta": {"symbol": symbol, "period": period, "adjust": "qfq", "count": 250},
                "inputs": {
                    "klines": to_jsonable(klines),
                    "quote": to_jsonable(quote) if quote else None,
                    "flows": to_jsonable(flows) if flows else None,
                    "index_klines": to_jsonable(index_klines) if index_klines else None,
                    "breadth": to_jsonable(breadth) if breadth else None,
                },
                "outputs": {
                    "signal": to_jsonable(signal_data),
                    "chanlun_daily": to_jsonable(chanlun_dict),
                },
            }
            path = os.path.join(OUT, f"{symbol}_{period}.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump(fixture, f, ensure_ascii=False)
            print(f"OK {path} klines={len(klines)} score={signal_data.get('score')} action={signal_data.get('action')}")

    print("DONE")


if __name__ == "__main__":
    main()
