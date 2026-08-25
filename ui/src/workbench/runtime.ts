import { useEffect, useState } from "react";
import { isProton, requestNative } from "../bridge";

export interface WorkerRuntimeState {
  name: string;
  pid: number;
  request_count: number;
  failure_count: number;
  status: "ready" | "degraded" | string;
}

export interface HostRuntimeState {
  status: "ready" | string;
  host_version: string;
  engine: WorkerRuntimeState;
  agent: WorkerRuntimeState;
  queue_limit: number;
}

export function useRuntimeStatus() {
  const [status, setStatus] = useState<HostRuntimeState | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isProton()) return;
    let live = true;
    const poll = () => requestNative<HostRuntimeState>("host", "diagnostics.status")
      .then((next) => { if (live) { setStatus(next); setError(null); } })
      .catch((cause) => { if (live) setError(cause instanceof Error ? cause.message : String(cause)); });
    poll();
    const timer = window.setInterval(poll, 2_000);
    return () => { live = false; window.clearInterval(timer); };
  }, []);

  return { status, error, desktop: isProton() };
}
