# -*- coding: utf-8 -*-
"""Replay the legacy pipeline offline from a golden fixture's inputs and diff
against outputs.signal. Used to debug the Rust port.

Usage: python replay_check.py <fixture.json> [--probe]
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
LEGACY = os.path.abspath(os.path.join(HERE, "..", "..", "legacy-reference"))
sys.path.insert(0, os.path.join(LEGACY, "libs"))
sys.path.insert(0, LEGACY)

from data.kline_fetcher import Kline, Quote, FundFlow
from analysis.signal_engine import run_analysis
import app as legacy_app


def to_kline(d):
    return Kline(date=d["date"], open=d["open"], close=d["close"], high=d["high"],
                 low=d["low"], volume=d["volume"], amount=d.get("amount", 0.0),
                 pct=d.get("pct", 0.0), turnover=d.get("turnover", 0.0))


def to_quote(d):
    return Quote(symbol=d["symbol"], name=d["name"], price=d["price"], pct=d["pct"],
                 change=d["change"], high=d["high"], low=d["low"], open=d["open"],
                 pre_close=d["pre_close"], volume=d["volume"], amount=d["amount"],
                 turnover=d["turnover"], timestamp=d.get("timestamp", ""))


def to_flow(d):
    return FundFlow(date=d["date"], main_net=d["main_net"],
                    super_large_net=d["super_large_net"], large_net=d["large_net"],
                    medium_net=d["medium_net"], small_net=d["small_net"],
                    main_pct=d.get("main_pct", 0.0))


def replay(fx):
    inp = fx["inputs"]
    klines = [to_kline(k) for k in inp["klines"]]
    quote = to_quote(inp["quote"]) if inp.get("quote") else None
    flows = [to_flow(f) for f in inp["flows"]] if inp.get("flows") else None
    index_klines = [to_kline(k) for k in inp["index_klines"]] if inp.get("index_klines") else None
    breadth = inp.get("breadth")

    result = run_analysis(klines, quote, flows, index_klines)
    signal_data = legacy_app.signal_to_dict(result)
    if breadth and signal_data.get("canslim") and breadth.get("total", 0) >= 50:
        br = breadth.get("breadth_ratio", 0.5)
        bonus = 15 if br >= 0.7 else 10 if br >= 0.6 else 5 if br >= 0.5 else -5 if br >= 0.4 else -10 if br >= 0.3 else -15
        signal_data["canslim"]["m_score"] = max(0, min(100, signal_data["canslim"]["m_score"] + bonus))
    signal_data = legacy_app._apply_signal_optimization(signal_data, klines, quote)
    return signal_data


def diff(exp, act, path=""):
    out = []
    if isinstance(exp, dict):
        if not isinstance(act, dict):
            return [f"{path}: expected dict, got {type(act).__name__}"]
        for k in exp:
            if k not in act:
                out.append(f"{path}.{k}: MISSING in actual")
            else:
                out.extend(diff(exp[k], act[k], f"{path}.{k}"))
        for k in act:
            if k not in exp:
                out.append(f"{path}.{k}: EXTRA in actual ({act[k]!r})")
    elif isinstance(exp, list):
        if not isinstance(act, list) or len(exp) != len(act):
            return [f"{path}: list len exp={len(exp)} act={len(act) if isinstance(act, list) else type(act).__name__}"]
        for i, (e, a) in enumerate(zip(exp, act)):
            out.extend(diff(e, a, f"{path}[{i}]"))
    elif isinstance(exp, float) or isinstance(act, float):
        try:
            e, a = float(exp), float(act)
        except (TypeError, ValueError):
            return [f"{path}: exp={exp!r} act={act!r}"]
        if abs(e - a) > 1e-4 * max(1.0, abs(e), abs(a)):
            out.append(f"{path}: exp={e!r} act={a!r}")
    else:
        if exp != act:
            out.append(f"{path}: exp={exp!r} act={act!r}")
    return out


def main():
    path = sys.argv[1]
    with open(path, encoding="utf-8") as f:
        fx = json.load(f)
    got = replay(fx)
    exp = fx["outputs"]["signal"]
    d = diff(exp, got, "signal")
    if d:
        print(f"{os.path.basename(path)}: {len(d)} diffs")
        for line in d[:60]:
            print("  " + line)
        sys.exit(1)
    print(f"{os.path.basename(path)}: OK")


if __name__ == "__main__":
    main()
