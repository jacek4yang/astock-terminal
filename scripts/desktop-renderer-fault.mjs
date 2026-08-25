import { CdpClient, waitForCdpPage } from "./lib/cdp-client.mjs";

const port = Number(process.argv[2]);
const expectedCommit = process.argv[3] ?? "";
if (!Number.isInteger(port) || !/^[a-f0-9]{40}$/i.test(expectedCommit)) {
  throw new Error("usage: node scripts/desktop-renderer-fault.mjs <port> <full-commit>");
}

const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function connectReady(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let lastError = new Error("no recovered page target");
  while (Date.now() < deadline) {
    let client;
    try {
      const target = await waitForCdpPage(port, { timeoutMs: Math.min(1500, Math.max(100, deadline - Date.now())) });
      client = await CdpClient.connect(target.webSocketDebuggerUrl, { timeoutMs: 1500 });
      await Promise.all([client.call("Page.enable"), client.call("Runtime.enable")]);
      const snapshot = await client.evaluate(`(() => ({
        ready: document.readyState === "complete" && Boolean(document.querySelector("nav[aria-label='主功能导航']")),
        nav: [...document.querySelectorAll("nav[aria-label='主功能导航'] button")].map((button) => button.textContent?.trim()),
        bodyText: document.body?.innerText?.slice(0, 4000) ?? "",
      }))()`);
      if (snapshot?.ready) return { client, target, snapshot };
      lastError = new Error("renderer document was not ready");
    } catch (error) {
      lastError = error instanceof Error ? error : new Error(String(error));
    }
    client?.close();
    await delay(100);
  }
  throw new Error(`renderer did not recover within ${timeoutMs}ms: ${lastError.message}`);
}

const initial = await connectReady(30_000);
let recovered;
try {
  // Page.crash intentionally terminates only CEF's renderer process. The
  // bounded Proton patch must recreate it without restarting Host or Workers.
  await initial.client.call("Page.crash", {}, { timeoutMs: 5000 }).catch(() => {});
  initial.client.close();
  recovered = await connectReady(45_000);
  for (const expected of ["今日市场", "Agent 智研", "配置"]) {
    if (!recovered.snapshot.nav?.some((label) => label?.includes(expected))) throw new Error(`recovered renderer is missing ${expected}`);
  }
  if (!recovered.snapshot.bodyText.includes("只读研究")) throw new Error("recovered renderer lost the manual-research safety boundary");
  process.stdout.write(`${JSON.stringify({
    ok: true,
    expected_commit: expectedCommit,
    renderer_fault_injected: true,
    host_restart_required: false,
    initial_target: initial.target.id,
    recovered_target: recovered.target.id,
    navigation: recovered.snapshot.nav,
  })}\n`);
} finally {
  await recovered?.client.call("Browser.close").catch(() => {});
  recovered?.client.close();
  initial.client.close();
}
