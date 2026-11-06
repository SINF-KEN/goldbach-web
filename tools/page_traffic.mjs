#!/usr/bin/env node
// First-party network traffic of the dashboard, measured in a real (headless)
// Chromium over the DevTools protocol. No dependencies (Node's own WebSocket).
//
//   node tools/page_traffic.mjs URL [--browser brave] [--watch 180] [--hidden 120] [--json out.json] [--eval EXPR]
//
// Scenarios, one fresh profile (no extensions, so no chrome-extension:// traffic;
// any such request would be listed apart and never counted):
//   cold    first visit, empty cache
//   warm    a second visit (normal navigation, not a reload) with the cache warm
//   watch   the open page left visible for --watch s: its polling
//   hidden  the page in a background tab for --hidden s
//   worker  (--worker s) press start and record the verifier's downloads until its
//           GPU self-test passes (or s seconds). WebGPU runs on SwiftShader (the
//           CPU): the tool refuses to start on any other adapter, so it never
//           touches a real GPU. Point it at a local server only (it leases slices).
// Requests of the page, its workers and their fetches are all counted; the
// browser's own background traffic (other targets) is not.
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const opt = (k, d) => { const i = args.indexOf(`--${k}`); return i >= 0 ? args[i + 1] : d; };
const url = args.find((a) => /^https?:/.test(a));
if (!url) { console.error("usage: page_traffic.mjs URL [--browser brave] [--watch s] [--hidden s] [--json file]"); process.exit(2); }
const browser = opt("browser", "brave");
const watchS = Number(opt("watch", 180)), hiddenS = Number(opt("hidden", 120)), workerS = Number(opt("worker", 0));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const profile = mkdtempSync(join(process.env.TMPDIR || tmpdir(), "gb-traffic-"));
const proc = spawn(browser, ["--headless=new", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
  "--no-first-run", "--no-default-browser-check", "--disable-extensions", "--disable-component-update",
  "--disable-background-networking", "--window-size=1200,900",
  ...(workerS ? ["--enable-unsafe-webgpu", "--use-webgpu-adapter=swiftshader", "--use-angle=swiftshader", "--disable-vulkan-surface"] : ["--disable-gpu"]),
  "about:blank"], { stdio: "ignore" });
// never leave the browser behind, whatever happens
const cleanup = () => { try { proc.kill(); } catch { /* gone */ } };
process.on("exit", cleanup);
process.on("uncaughtException", (e) => { console.error(e); cleanup(); process.exit(1); });
process.on("unhandledRejection", (e) => { console.error(e); cleanup(); process.exit(1); });
for (const sig of ["SIGINT", "SIGTERM"]) process.on(sig, () => { cleanup(); process.exit(1); });
let port = null;
for (let i = 0; i < 100 && !port; i++) {
  await sleep(100);
  const f = join(profile, "DevToolsActivePort");
  if (existsSync(f)) port = readFileSync(f, "utf8").split("\n")[0];
}
if (!port) throw new Error("browser did not start");
const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener("open", r, { once: true }));
let nextId = 1;
const pending = new Map(), handlers = [];
ws.addEventListener("message", (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { const p = pending.get(m.id); pending.delete(m.id); m.error ? p.reject(new Error(m.error.message)) : p.resolve(m.result); }
  else for (const h of handlers) h(m);
});
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = nextId++;
  pending.set(id, { resolve, reject });
  ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
});

// every request of the page and of the workers it starts
const reqs = new Map();
let phase = "setup";
const ext = [];
handlers.push((m) => {
  const p = m.params || {};
  if (m.method === "Network.requestWillBeSent") {
    if (p.request.url.startsWith("chrome-extension://")) { ext.push(p.request.url); return; }
    if (!/^https?:/.test(p.request.url)) return;
    reqs.set(p.requestId, { phase, url: p.request.url, method: p.request.method, t: p.timestamp, status: 0, bytes: 0, body: 0, cache: false });
  } else if (m.method === "Network.responseReceived") {
    const r = reqs.get(p.requestId);
    if (r) { r.status = p.response.status; r.cache = !!(p.response.fromDiskCache || p.response.fromMemoryCache || p.response.fromServiceWorker);
             r.type = p.type; r.cc = p.response.headers["cache-control"] || p.response.headers["Cache-Control"] || ""; }
  } else if (m.method === "Network.dataReceived") {
    const r = reqs.get(p.requestId);
    if (r) r.body += p.dataLength;
  } else if (m.method === "Network.loadingFinished") {
    const r = reqs.get(p.requestId);
    if (r) r.bytes = p.encodedDataLength;
  } else if (m.method === "Network.requestServedFromCache") {
    const r = reqs.get(p.requestId);
    if (r) r.cache = true;
  }
});
const sessions = new Set();
handlers.push(async (m) => {
  if (m.method === "Target.attachedToTarget") {
    const sid = m.params.sessionId;
    sessions.add(sid);
    await send("Network.enable", {}, sid).catch(() => {});
    await send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sid).catch(() => {});
    await send("Runtime.runIfWaitingForDebugger", {}, sid).catch(() => {});
  }
});

const { targetId } = await send("Target.createTarget", { url: "about:blank" });
const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
await send("Network.enable", {}, sessionId);
await send("Page.enable", {}, sessionId);
await send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: false, flatten: true }, sessionId);

async function visit(name) {
  phase = name;
  const loaded = new Promise((r) => { const h = (m) => { if (m.method === "Page.loadEventFired" && m.sessionId === sessionId) { handlers.splice(handlers.indexOf(h), 1); r(); } }; handlers.push(h); });
  const t0 = performance.now();
  await send("Page.navigate", { url }, sessionId);
  await loaded;
  const loadMs = performance.now() - t0;
  const nav = JSON.parse((await send("Runtime.evaluate", { expression:
    "JSON.stringify((({domContentLoadedEventEnd:d,loadEventEnd:l,responseEnd:r})=>({dcl:d,load:l,html:r}))(performance.getEntriesByType('navigation')[0]))",
    returnByValue: true }, sessionId)).result.value);
  await sleep(5000);                                   // whatever the page fetches right after load
  return { loadMs: Math.round(loadMs), ...Object.fromEntries(Object.entries(nav).map(([k, v]) => [k, Math.round(v)])) };
}
const out = { url, browser: version.Browser, scenarios: {} };
out.scenarios.cold = { timing: await visit("cold") };
if (opt("eval")) out.eval = (await send("Runtime.evaluate", { expression: opt("eval"), returnByValue: true, awaitPromise: true }, sessionId)).result.value;
await send("Page.navigate", { url: "about:blank" }, sessionId);
await sleep(500);
out.scenarios.warm = { timing: await visit("warm") };
phase = "watch";
await sleep(watchS * 1000);
out.scenarios.watch = { seconds: watchS };
if (hiddenS > 0) {
  const other = await send("Target.createTarget", { url: "about:blank" });   // a new foreground tab
  await send("Target.activateTarget", { targetId: other.targetId });
  await sleep(1000);
  const vis = (await send("Runtime.evaluate", { expression: "document.visibilityState", returnByValue: true }, sessionId)).result.value;
  phase = "hidden";
  await sleep(hiddenS * 1000);
  out.scenarios.hidden = { seconds: hiddenS, visibility: vis };
}

if (workerS > 0) {
  phase = "setup";
  await send("Target.activateTarget", { targetId });
  const ev = async (e) => (await send("Runtime.evaluate", { expression: e, returnByValue: true, awaitPromise: true }, sessionId)).result.value;
  const adapter = await ev("navigator.gpu?.requestAdapter().then((a) => a ? JSON.stringify(Object.fromEntries(['vendor', 'architecture', 'device', 'description'].map((k) => [k, a.info[k]]))) : 'none')");
  if (!/swiftshader/i.test(adapter || "")) throw new Error(`not SwiftShader (${adapter}): refusing to start a verifier on a real GPU`);
  phase = "worker";
  const t0 = performance.now();
  await ev("document.getElementById('go').click()");
  let st = "";
  // done once the self-test passed and the first slice runs (its status is
  // overwritten within seconds, so the slice field is the signal), or on an error
  let slice = "-";
  while (performance.now() - t0 < workerS * 1000 && slice === "-" && !/wrong answer|error|not available/i.test(st)) {
    await sleep(2000);
    [st, slice] = JSON.parse(await ev("JSON.stringify([document.getElementById('d-status').textContent, document.getElementById('d-slice').textContent])"));
  }
  out.scenarios.worker = { adapter: JSON.parse(adapter), seconds: Math.round((performance.now() - t0) / 1000), status: st, slice,
                           gpu: await ev("document.getElementById('d-gpu').textContent") };
  await ev("document.getElementById('go').click()");          // stop: the lease goes back
  await sleep(2000);
}

const origin = new URL(url).origin;
for (const [name, sc] of Object.entries(out.scenarios)) {
  const rs = [...reqs.values()].filter((r) => r.phase === name);
  const first = rs.filter((r) => r.url.startsWith(origin)), third = rs.filter((r) => !r.url.startsWith(origin));
  sc.requests = first.length;
  sc.network = first.filter((r) => !r.cache).length;     // went to the server (200 or 304)
  sc.notModified = first.filter((r) => r.status === 304).length;
  sc.transferred = first.reduce((a, r) => a + (r.cache ? 0 : r.bytes), 0);
  sc.decoded = first.reduce((a, r) => a + r.body, 0);
  sc.thirdParty = third.map((r) => r.url);
  sc.list = first.map((r) => `${String(r.status).padStart(3)} ${r.cache ? "cache" : String(r.bytes).padStart(5)} ${r.url.slice(origin.length)}`);
}
out.extensionRequests = ext.length;
console.log(JSON.stringify(out, null, 1));
if (opt("json")) writeFileSync(opt("json"), JSON.stringify(out, null, 1));
ws.close();
proc.kill();
await sleep(300);
rmSync(profile, { recursive: true, force: true });
