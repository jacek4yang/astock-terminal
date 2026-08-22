# Generate the synthetic minute-level golden fixture with the legacy engine.
import json
import math
import sys

sys.path.insert(0, "../../../../../legacy-reference")
sys.path.insert(0, "../../../../../legacy-reference/libs")

from analysis.chanlun_minute import analyze_chanlun_minute, signals_to_dict

# Two trading sessions (morning 09:30-11:29, afternoon 13:00-14:59) of
# 1-minute bars; prices follow a deterministic multi-swing zigzag so that
# fractals, strokes and divergences actually form.
times = []
prices = []
volumes = []


def session(start_h, start_m, count):
    base = start_h * 60 + start_m
    return [f"{(base + i) // 60:02d}:{(base + i) % 60:02d}" for i in range(count)]


def wave(t):
    # Layered swings: slow trend + medium swings + small wiggle.
    return (
        10.0
        + 3.0 * math.sin(t / 17.0)
        + 1.2 * math.sin(t / 6.5 + 1.0)
        + 0.3 * math.sin(t / 2.3)
    )


t = 0
for sess in (session(9, 30, 120), session(13, 0, 120)):
    for ts in sess:
        times.append(ts)
        prices.append(round(wave(t), 3))
        volumes.append(1000.0 + (t % 7) * 100.0)
        t += 1

result = analyze_chanlun_minute(times, prices, volumes)
out = {
    "inputs": {"times": times, "prices": prices, "volumes": volumes},
    "expected": signals_to_dict(result),
}
with open(
    "minute_synthetic.json"  # run from this directory,
    "w",
    encoding="utf-8",
) as f:
    json.dump(out, f, ensure_ascii=False)
print("kline_count", result.kline_count)
print("fractal_count", result.fractal_count)
print("stroke_count", result.stroke_count)
print("signals", len(result.signals))
print("state", result.current_state)
