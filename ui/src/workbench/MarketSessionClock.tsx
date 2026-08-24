import { useEffect, useMemo, useState } from "react";
import { isProton, requestNative } from "../bridge";

type MarketSession = {
  server_time: string;
  is_trading_day: boolean;
  is_trading: boolean;
  state: string;
  state_label: string;
  next_transition_at: string;
  next_transition_label: string;
  seconds_to_transition: number;
  calendar?: { rules_version?: string; verified_at?: string; source_url?: string };
};

function durationLabel(seconds: number): string {
  const safe = Math.max(0, Math.floor(seconds));
  const days = Math.floor(safe / 86_400);
  const hours = Math.floor((safe % 86_400) / 3_600);
  const minutes = Math.floor((safe % 3_600) / 60);
  const secs = safe % 60;
  if (days) return `${days}天 ${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}`;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

export default function MarketSessionClock() {
  const [session, setSession] = useState<MarketSession | null>(null);
  const [clockOffset, setClockOffset] = useState(0);
  const [tick, setTick] = useState(Date.now());

  useEffect(() => {
    if (!isProton()) return;
    let live = true;
    const refresh = () => {
      const requestedAt = Date.now();
      requestNative<MarketSession>("engine", "market.session")
        .then((next) => {
          if (!live) return;
          setSession(next);
          setClockOffset(new Date(next.server_time).getTime() - requestedAt);
        })
        .catch(() => undefined);
    };
    refresh();
    const poll = window.setInterval(refresh, 60_000);
    const ticker = window.setInterval(() => setTick(Date.now()), 1_000);
    return () => { live = false; window.clearInterval(poll); window.clearInterval(ticker); };
  }, []);

  const view = useMemo(() => {
    const exchangeNow = tick + clockOffset;
    const time = new Intl.DateTimeFormat("zh-CN", {
      timeZone: "Asia/Shanghai", hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false,
    }).format(exchangeNow);
    if (!session) return { time, remain: "连接交易日历…" };
    const remaining = (new Date(session.next_transition_at).getTime() - exchangeNow) / 1_000;
    return { time, remain: `距${session.next_transition_label} ${durationLabel(remaining)}` };
  }, [clockOffset, session, tick]);

  return <div className={`market-clock ${session?.is_trading ? "live" : "closed"}`} title={session ? `交易日历 ${session.calendar?.rules_version ?? "—"} · 核验于 ${session.calendar?.verified_at ?? "—"}` : "正在读取 Engine 交易日历"}>
    <span className="market-clock-dot" />
    <strong>{session?.state_label ?? "交易日历"}</strong>
    <time>{view.time}</time>
    <em>{view.remain}</em>
  </div>;
}
