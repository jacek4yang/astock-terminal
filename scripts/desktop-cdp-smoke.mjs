import { CdpClient, waitForCdpPage } from "./lib/cdp-client.mjs";

const port = Number(process.argv[2]);
const expectedCommit = process.argv[3] ?? "";
if (!Number.isInteger(port) || !/^[a-f0-9]{40}$/i.test(expectedCommit)) {
  throw new Error("usage: node scripts/desktop-cdp-smoke.mjs <port> <full-commit>");
}

const target = await waitForCdpPage(port, { timeoutMs: 45_000 });
const cdp = await CdpClient.connect(target.webSocketDebuggerUrl);
const logErrors = [];
try {
  await Promise.all([
    cdp.call("Page.enable"),
    cdp.call("Runtime.enable"),
    cdp.call("Log.enable"),
  ]);
  cdp.on("Log.entryAdded", (params) => {
    if (["error", "warning"].includes(params.entry?.level)) logErrors.push(params.entry?.text ?? "unknown browser log");
  });
  await cdp.call("Page.reload", { ignoreCache: true });
  const deadline = Date.now() + 30_000;
  let snapshot;
  while (Date.now() < deadline) {
    snapshot = await cdp.evaluate(`(() => ({
      ready: document.readyState === "complete" && Boolean(document.querySelector("nav[aria-label='主功能导航']")),
      title: document.title,
      bodyText: document.body?.innerText?.slice(0, 4000) ?? "",
      nav: [...document.querySelectorAll("nav[aria-label='主功能导航'] button")].map((button) => button.textContent?.trim()),
      buildCommit: document.querySelector('meta[name="astock-build-commit"]')?.getAttribute("content") ?? null,
      canvas2d: Boolean(document.createElement("canvas").getContext("2d")),
    }))()`);
    if (snapshot?.ready) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  if (!snapshot?.ready) throw new Error("packaged workbench did not become ready");
  for (const expected of ["今日市场", "Agent 智研", "配置"]) {
    if (!snapshot.nav?.some((label) => label?.includes(expected))) throw new Error(`primary navigation is missing ${expected}`);
  }
  if (!snapshot.bodyText.includes("只读研究")) throw new Error("manual-research safety boundary is not visible");
  if (!snapshot.canvas2d) throw new Error("CEF canvas rendering is unavailable");
  if (snapshot.buildCommit && snapshot.buildCommit !== expectedCommit) throw new Error("renderer build metadata does not match the expected commit");
  if (logErrors.length) throw new Error(`packaged renderer logged errors: ${logErrors.join(" | ")}`);
  process.stdout.write(`${JSON.stringify({
    ok: true,
    target_url: target.url,
    title: snapshot.title,
    navigation: snapshot.nav,
    expected_commit: expectedCommit,
    console_error_count: logErrors.length,
    canvas_2d_available: snapshot.canvas2d,
  })}\n`);
} finally {
  await cdp.call("Browser.close").catch(() => {});
  cdp.close();
}
