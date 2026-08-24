import { useEffect, useState } from "react";
import { isProton, requestNative } from "../bridge";

type ServiceState = {
  status?: string;
  engine_version?: string;
  agent_version?: string;
  reducer_version?: string;
  protocol_version?: number;
  capabilities?: string[];
};

export default function DiagnosticsPanel() {
  const [engine, setEngine] = useState<ServiceState | null>(null);
  const [agent, setAgent] = useState<ServiceState | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isProton()) return;
    let live = true;
    const load = () => Promise.all([
        requestNative<ServiceState>("engine", "diagnostics.status"),
        requestNative<ServiceState>("agent", "diagnostics.status"),
      ]).then(([engineState, agentState]) => {
        if (live) { setEngine(engineState); setAgent(agentState); setError(null); }
      }).catch((cause) => live && setError(cause instanceof Error ? cause.message : String(cause)));
    void load();
    const timer = window.setInterval(load, 2_000);
    return () => { live = false; window.clearInterval(timer); };
  }, []);

  const rows = [
    ["Renderer", "ready", navigator.userAgent],
    ["Proton bridge", isProton() ? "connected" : "preview", isProton() ? "typed app/request" : "browser-only"],
    ["Rust Engine", engine?.status ?? "unknown", engine?.engine_version ?? "—"],
    ["MoonBit Agent", agent?.status ?? "unknown", agent?.reducer_version ?? agent?.agent_version ?? "—"],
    ["Protocol", engine?.protocol_version ? `v${engine.protocol_version}` : "v1", "8 MiB frame / 256 queue"],
  ];
  return <div className="diagnostics-panel"><div className="panel-heading"><div><span className="eyebrow">RUNTIME</span><h2>诊断与能力</h2></div></div>{error && <p className="inline-warning">{error}</p>}<table><tbody>{rows.map(([name, status, detail]) => <tr key={name}><th>{name}</th><td><span className={`health-dot ${status === "ready" || status === "connected" ? "ok" : ""}`} />{status}</td><td>{detail}</td></tr>)}</tbody></table></div>;
}
