import { randomUUID } from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { performance } from "node:perf_hooks";
import { fileURLToPath } from "node:url";
import { CdpClient, waitForCdpPage } from "./lib/cdp-client.mjs";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  args.set(process.argv[index], process.argv[index + 1]);
}
const application = path.resolve(args.get("--application") ?? "");
const skeleton = path.resolve(args.get("--skeleton") ?? "");
const commit = args.get("--commit") ?? "";
const output = path.resolve(args.get("--output") ?? "");
const runRoot = path.resolve(args.get("--run-root") ?? "");
const scriptRoot = path.dirname(fileURLToPath(import.meta.url));
if (![application, skeleton].every((file) => fs.existsSync(file)) || !/^[a-f0-9]{40}$/i.test(commit) || !output || !runRoot) {
  throw new Error("usage: node performance-cdp.mjs --application <exe> --skeleton <exe> --commit <sha> --output <json> --run-root <dir>");
}
fs.mkdirSync(runRoot, { recursive: true });

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function reservePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

function waitForExit(child, timeoutMs) {
  if (child.exitCode != null) return Promise.resolve(true);
  return new Promise((resolve) => {
    const timer = setTimeout(() => { cleanup(); resolve(false); }, timeoutMs);
    const onExit = () => { cleanup(); resolve(true); };
    const cleanup = () => { clearTimeout(timer); child.off("exit", onExit); };
    child.once("exit", onExit);
  });
}

async function stopProcess(child, cdp) {
  if (cdp) {
    await cdp.call("Browser.close").catch(() => {});
    cdp.close();
  }
  if (await waitForExit(child, 5_000)) return;
  if (process.platform === "win32" && child.pid) {
    await new Promise((resolve) => {
      const killer = spawn("taskkill.exe", ["/PID", String(child.pid), "/T", "/F"], {
        windowsHide: true,
        stdio: "ignore",
      });
      killer.once("exit", resolve);
      killer.once("error", resolve);
    });
  } else {
    child.kill("SIGKILL");
  }
  await waitForExit(child, 5_000);
}

async function waitForValue(cdp, expression, timeoutMs = 45_000) {
  const deadline = Date.now() + timeoutMs;
  let value;
  while (Date.now() < deadline) {
    value = await cdp.evaluate(expression).catch(() => null);
    if (value) return value;
    await delay(25);
  }
  throw new Error(`packaged renderer condition timed out: ${expression.slice(0, 120)}`);
}

async function processTreeSamples(pid, samples, intervalMs) {
  const script = path.join(scriptRoot, "process-tree-sample.ps1");
  const executable = process.platform === "win32" ? "powershell.exe" : "pwsh";
  const child = spawn(executable, [
    "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script,
    "-RootProcessId", String(pid), "-Samples", String(samples), "-IntervalMilliseconds", String(intervalMs),
  ], { windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => { stdout += chunk; if (stdout.length > 2_000_000) child.kill(); });
  child.stderr.on("data", (chunk) => { stderr += chunk; if (stderr.length > 100_000) child.kill(); });
  const code = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", resolve);
  });
  if (code !== 0) throw new Error(`process-tree sampler failed: ${stderr.slice(0, 500)}`);
  return JSON.parse(stdout);
}

async function launchMeasured(executable, label, iteration) {
  const port = await reservePort();
  const profileRoot = path.join(runRoot, `${label}-${iteration}-${randomUUID()}`);
  const local = path.join(profileRoot, "local");
  const roaming = path.join(profileRoot, "roaming");
  const data = path.join(profileRoot, "data");
  for (const directory of [local, roaming, data]) fs.mkdirSync(directory, { recursive: true });
  const started = performance.now();
  const child = spawn(executable, [], {
    cwd: path.dirname(executable),
    windowsHide: false,
    stdio: "ignore",
    env: {
      ...process.env,
      ASTOCK_RELEASE_TEST_CDP: "1",
      PROTON_REMOTE_DEBUGGING_PORT: String(port),
      LOCALAPPDATA: local,
      APPDATA: roaming,
      ASTOCK_DATA_DIR: data,
    },
  });
  let cdp;
  try {
    const target = await waitForCdpPage(port, { timeoutMs: 45_000 });
    cdp = await CdpClient.connect(target.webSocketDebuggerUrl);
    await Promise.all([cdp.call("Page.enable"), cdp.call("Runtime.enable")]);
    if (label === "application") {
      await waitForValue(cdp, `(() => document.readyState === "complete"
        && Boolean(document.querySelector("nav[aria-label='主功能导航']"))
        && document.querySelector(".runtime-indicator")?.textContent?.includes("桌面服务就绪"))()`);
    } else {
      await waitForValue(cdp, `(() => document.readyState === "complete"
        && document.querySelector("[data-proton-skeleton]")?.getAttribute("data-proton-skeleton") === "0.2.1")()`);
    }
    const readyMs = performance.now() - started;
    const tree = await processTreeSamples(child.pid, 1, 250);
    const memoryBytes = tree.samples.at(-1)?.working_set_bytes;
    if (!Number.isFinite(memoryBytes) || memoryBytes <= 0) throw new Error(`${label} memory sample was invalid`);
    return { child, cdp, readyMs, memoryBytes, port, profileRoot };
  } catch (error) {
    await stopProcess(child, cdp);
    throw error;
  }
}

async function coldSamples(executable, label, count) {
  const starts = [];
  const memory = [];
  for (let index = 0; index < count; index += 1) {
    const run = await launchMeasured(executable, label, index);
    starts.push(run.readyMs);
    memory.push(run.memoryBytes);
    await stopProcess(run.child, run.cdp);
    await delay(300);
  }
  return { starts, memory };
}

async function workspaceRestoreSamples(cdp, count) {
  await cdp.evaluate(`(() => {
    const button = [...document.querySelectorAll("nav[aria-label='主功能导航'] button")]
      .find((item) => item.textContent?.includes("Agent 智研"));
    if (!button) throw new Error("Agent navigation is missing");
    button.click();
    return true;
  })()`);
  await waitForValue(cdp, `(() => Boolean(document.querySelector(".persistent-agent-surface.active .agent-console")))()`);
  const samples = [];
  for (let index = 0; index < count; index += 1) {
    const started = performance.now();
    await cdp.call("Page.reload", { ignoreCache: false });
    await waitForValue(cdp, `(() => document.readyState === "complete"
      && Boolean(document.querySelector(".persistent-agent-surface.active .agent-console"))
      && document.querySelector(".runtime-indicator")?.textContent?.includes("桌面服务就绪"))()`);
    samples.push(performance.now() - started);
  }
  return samples;
}

async function commandFeedbackSamples(cdp, count) {
  const labels = ["今日市场", "Agent 智研", "配置"];
  const samples = [];
  for (let index = 0; index < count; index += 1) {
    const label = labels[index % labels.length];
    const duration = await cdp.evaluate(`(() => new Promise((resolve, reject) => {
      const button = [...document.querySelectorAll("nav[aria-label='主功能导航'] button")]
        .find((item) => item.textContent?.includes(${JSON.stringify(label)}));
      if (!button) { reject(new Error("navigation command missing")); return; }
      const started = performance.now();
      button.click();
      const deadline = started + 2000;
      const check = () => {
        if (button.classList.contains("active")) {
          requestAnimationFrame(() => resolve(performance.now() - started));
        } else if (performance.now() > deadline) reject(new Error("navigation feedback timed out"));
        else requestAnimationFrame(check);
      };
      check();
    }))()`);
    if (!Number.isFinite(duration)) throw new Error("command feedback sample was invalid");
    samples.push(duration);
  }
  return samples;
}

async function enterPerformanceHarness(cdp) {
  await cdp.evaluate(`(() => { history.replaceState(null, "", location.pathname + "?releasePerf=1"); location.reload(); return true; })()`).catch(() => {});
  return await waitForValue(cdp, `(() => {
    const root = document.querySelector("[data-release-performance-harness='v1']");
    return root && Number(root.getAttribute("data-logical-rows")) === 100000 ? {
      logicalRows: Number(root.getAttribute("data-logical-rows")),
      renderedRows: Number(root.getAttribute("data-rendered-rows")),
    } : null;
  })()`);
}

async function scrollFpsSamples(cdp, count) {
  const samples = [];
  let maximumDomRows = 0;
  for (let index = 0; index < count; index += 1) {
    const result = await cdp.evaluate(`(() => new Promise((resolve, reject) => {
      const root = document.querySelector("[data-release-performance-harness='v1']");
      const scroller = document.querySelector(".release-performance-scroll");
      if (!root || !scroller) { reject(new Error("performance harness is missing")); return; }
      const start = performance.now();
      const duration = 1200;
      const from = scroller.scrollTop;
      const to = from < scroller.scrollHeight / 2 ? scroller.scrollHeight - scroller.clientHeight : 0;
      const frames = [];
      const step = (now) => {
        frames.push(now);
        const progress = Math.min(1, (now - start) / duration);
        scroller.scrollTop = from + ((to - from) * progress);
        if (progress < 1) requestAnimationFrame(step);
        else {
          const elapsed = frames.at(-1) - frames[0];
          resolve({
            fps: elapsed > 0 ? ((frames.length - 1) * 1000) / elapsed : 0,
            renderedRows: Number(root.getAttribute("data-rendered-rows")),
            logicalRows: Number(root.getAttribute("data-logical-rows")),
          });
        }
      };
      requestAnimationFrame(step);
    }))()`, { timeoutMs: 5_000 });
    if (result.logicalRows !== 100_000 || !Number.isFinite(result.fps)) throw new Error("100k scroll sample was invalid");
    maximumDomRows = Math.max(maximumDomRows, result.renderedRows);
    samples.push(result.fps);
  }
  if (maximumDomRows > 200) throw new Error(`virtualized fixture rendered too many DOM rows: ${maximumDomRows}`);
  return { samples, maximumDomRows };
}

async function agentRenderSamples(cdp, count) {
  const samples = [];
  for (let index = 0; index < count; index += 1) {
    const hz = await cdp.evaluate(`(() => new Promise((resolve, reject) => {
      const timestamps = [];
      const listener = (event) => timestamps.push(Number(event.detail?.at) || performance.now());
      window.addEventListener("astock:agent-render-batch", listener);
      let sent = 0;
      const timer = setInterval(() => {
        window.dispatchEvent(new CustomEvent("astock:release-agent-events", { detail: { count: 1 } }));
        sent += 1;
        if (sent >= 120) {
          clearInterval(timer);
          setTimeout(() => {
            window.removeEventListener("astock:agent-render-batch", listener);
            if (timestamps.length < 2) { reject(new Error("too few Agent render batches")); return; }
            let maximum = 0;
            for (const start of timestamps) {
              maximum = Math.max(maximum, timestamps.filter((value) => value >= start && value < start + 1000).length);
            }
            resolve(maximum);
          }, 300);
        }
      }, 10);
    }))()`, { timeoutMs: 5_000 });
    if (!Number.isFinite(hz)) throw new Error("Agent render-rate sample was invalid");
    samples.push(hz);
  }
  return samples;
}

const startedAt = new Date().toISOString();
const skeletonCold = await coldSamples(skeleton, "skeleton", 10);
const applicationCold = await coldSamples(application, "application", 10);
const session = await launchMeasured(application, "application-session", 0);
try {
  const workspaceRestore = await workspaceRestoreSamples(session.cdp, 30);
  const commandFeedback = await commandFeedbackSamples(session.cdp, 30);
  const harness = await enterPerformanceHarness(session.cdp);
  const scroll = await scrollFpsSamples(session.cdp, 10);
  const agentRender = await agentRenderSamples(session.cdp, 10);
  await session.cdp.evaluate(`(() => { history.replaceState(null, "", location.pathname); location.reload(); return true; })()`).catch(() => {});
  await waitForValue(session.cdp, `(() => document.readyState === "complete"
    && Boolean(document.querySelector("nav[aria-label='主功能导航']"))
    && document.querySelector(".runtime-indicator")?.textContent?.includes("桌面服务就绪"))()`);
  const idle = await processTreeSamples(session.child.pid, 60, 1000);
  const result = {
    schema_version: 1,
    commit,
    started_at_utc: startedAt,
    completed_at_utc: new Date().toISOString(),
    runner: { os: `${os.type()} ${os.release()}`, arch: os.arch() },
    samples: {
      workspace_restore_ms: workspaceRestore,
      command_feedback_ms: commandFeedback,
      logical_rows_scroll_fps: scroll.samples,
      agent_render_hz: agentRender,
      idle_cpu_pct: idle.samples.map((item) => item.cpu_pct),
      cold_start_ms: applicationCold.starts,
      skeleton_cold_start_ms: skeletonCold.starts,
      memory_bytes: applicationCold.memory,
      skeleton_memory_bytes: skeletonCold.memory,
    },
    assertions: {
      logical_rows: harness.logicalRows,
      maximum_dom_rows: scroll.maximumDomRows,
      idle_process_count_max: Math.max(...idle.samples.map((item) => item.process_count)),
      release_test_fixture: true,
      packaged_renderer: true,
    },
  };
  fs.writeFileSync(output, `${JSON.stringify(result, null, 2)}\n`, "utf8");
  process.stdout.write(`${JSON.stringify({ ok: true, output, commit })}\n`);
} finally {
  await stopProcess(session.child, session.cdp);
}
