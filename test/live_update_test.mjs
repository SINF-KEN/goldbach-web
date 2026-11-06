// Live updates in the page (client/app.js), without a browser: stubs for the
// DOM, Worker, fetch and storage. A running page must (1) on a new engine: ask
// the worker to finish its slice, then start a fresh worker on the new engine
// without reloading; (2) on a new page: the same, then reload itself.
//   node test/live_update_test.mjs
let viewportWidth = 1200;
const layoutListeners = [];
const matchMedia = (query) => ({
  matches: query.includes("max-width: 600px") && viewportWidth <= 600,
  addEventListener: (_type, listener) => { layoutListeners.push(listener); },
});
const el = () => ({ textContent: "", value: "", disabled: false, hidden: false, className: "", clientWidth: 600, style: {},
  classList: { toggle() {} }, on: {},
  addEventListener(type, f) { this.on[type] = f; }, setAttribute() {}, replaceChildren() {}, append() {}, remove() {},
  getBoundingClientRect: () => ({ width: 78 }) });
const els = {};
let version = { engine: "e1", page: "p1" };
els["live-init"] = { ...el(), textContent: JSON.stringify({ t: 1791820000, bh: "b1", narrow: false, v: version,
  speed: { 300: [[1791819900, 12345]] }, srv: { versions: [{ c: "aaa1111", s: "x", from: 1791800000, n: 2, mean: 5e7 }],
  last: { t: 1791810000, rate: 5e7 } } }) };
globalThis.document = {
  getElementById: (id) => (els[id] ??= el()), createElement: () => el(),
  querySelectorAll: () => [],
  documentElement: { clientWidth: viewportWidth },
  addEventListener() {}, visibilityState: "visible",
};
globalThis.addEventListener = () => {};
globalThis.window = { matchMedia };
const store = new Map([["gb_running", "1"], ["gb_client", "c1"]]);
Object.defineProperty(globalThis, "localStorage", { configurable: true,
  value: { getItem: (k) => store.get(k) ?? null, setItem: (k, v) => { store.set(k, v); } } });
let reloaded = false;
globalThis.location = { search: "", reload: () => { reloaded = true; } };
Object.defineProperty(globalThis, "navigator", { value: { gpu: {}, userAgent: "test" }, configurable: true });
const workers = [];
globalThis.Worker = class {
  url; msgs = []; terminated = false; onmessage = null;
  constructor(url) { this.url = url; workers.push(this); }
  postMessage(m) { this.msgs.push(m); }
  terminate() { this.terminated = true; }
};
const urls = [];
globalThis.fetch = async (url) => {
  urls.push(url);
  const match = url.match(/[?&]narrow=(\d)/);
  const body = url.startsWith("/api/live") ? JSON.stringify({
    t: 1791820030, bh: match?.[1] === "1" ? "bn" : "b1",
    board: match?.[1] === "1" ? "compact board" : "desktop board",
    speed: {}, v: version,
  }) : "{}";
  return { ok: true, json: async () => JSON.parse(body), text: async () => body };
};
// the page's poll timer (30 s): captured, fired by the test
const realTimeout = setTimeout;
let pollTimer = null;
globalThis.setTimeout = (f, ms) => { if (ms >= 30000) { pollTimer = f; return 0; } return realTimeout(f, ms); };
globalThis.clearTimeout = () => {};
const tick = () => new Promise((r) => realTimeout(r, 20));

globalThis.onunhandledrejection = (e) => { console.log("UNHANDLED", e.reason?.stack ?? e.reason); e.preventDefault?.(); };
await import("../client/app.js");
await tick();
let fail = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) fail++; };
const checkVersion = async () => { const f = pollTimer; pollTimer = null; await f?.(); };
check(urls.length === 0, "no request on load: the page carries its first data");
check(workers.length === 1 && workers[0].msgs[0]?.type === "start", "resumes on load: worker started");
check(workers[0].url === "worker.js?v=e1", "the worker is loaded by its engine hash");
check(urls.length === 0, "a 1200px desktop viewport does not request the compact board");
check(workers[0].msgs[0]?.lite === false, "the layout does not pick the engine: only ?lite asks the worker for lite");
await checkVersion(); await tick();
check(urls.length === 1 && urls[0].includes("bh=b1") && urls[0].includes("runs=1791810000"),
  "a poll names what the page has");
check(/[?&]tiers=(&|$)/.test(urls[0]), "a poll asks for no speed buckets (no chart)");
// Each resize must request exactly the board for its side of the 600 CSS-pixel
// breakpoint (count only the requests it caused), and nothing when it stays on
// the same side.
const resizeTo = async (width) => {
  const before = urls.length;
  viewportWidth = width;
  layoutListeners.forEach((listener) => listener());
  await tick();
  return urls.slice(before).filter((url) => url.startsWith("/api/live"));
};
const asks = (got, narrow) => got.length === 1 && got[0].includes(`narrow=${narrow}`);
check(asks(await resizeTo(390), 1), "a phone-width viewport (390px) requests the compact board");
check(asks(await resizeTo(1200), 0), "resizing back to a desktop viewport (1200px) requests the desktop board");
check(asks(await resizeTo(600), 1), "the 600px breakpoint itself selects the compact board");
check(asks(await resizeTo(601), 0), "601px selects the desktop board");
check((await resizeTo(1440)).length === 0, "a resize that stays desktop requests nothing");
check(workers.length === 1 && !workers[0].terminated, "layout changes never restart the worker or change its engine");

// "stop after slice": the slice runs to the end, then the worker stops; no new lease
const click = (id) => els[id].on.click();
check(!els.finish.hidden && els.finish.textContent === "stop after slice", "running: the stop-after-slice button is shown");
click("finish"); await tick();
check(workers[0].msgs.at(-1)?.type === "finish" && workers[0].msgs.at(-1)?.on === true, "stop after slice: the worker is told to finish");
check(!workers[0].msgs.some((m) => m.type === "stop") && !workers[0].terminated, "the current slice is not interrupted");
check(store.get("gb_running") === "0" && els.finish.textContent === "keep going", "a reload will not resume; the button can undo it");
click("finish"); await tick();
check(workers[0].msgs.at(-1)?.on === false && store.get("gb_running") === "1", "keep going: the worker is told to continue");
click("finish"); await tick();
workers[0].onmessage({ data: { type: "stopped", upgrade: false } }); await tick();
check(els.go.textContent === "start" && els.finish.hidden && !workers[0].terminated, "after the slice: stopped, button hidden");
click("go"); await tick();
check(workers.length === 1 && workers[0].msgs.at(-1)?.type === "start" && store.get("gb_running") === "1", "start again: the same worker resumes");
check(els.finish.textContent === "stop after slice", "a new run starts without stop-after-slice");

// (1) new engine while running
version = { engine: "e2", page: "p1" };
await checkVersion(); await tick();
check(workers[0].msgs.some((m) => m.type === "upgrade"), "new engine: worker asked to finish its slice");
check(!workers[0].terminated, "the current slice is not interrupted");
workers[0].onmessage({ data: { type: "stopped", upgrade: true } }); await tick();
check(workers[0].terminated && workers.length === 2, "after the slice: old worker gone, a fresh one created");
check(workers[1].url.includes("v=e2") && workers[1].msgs[0]?.type === "start", "the fresh worker runs the new engine");
check(!reloaded, "no page reload for an engine update");

// (2) new page while running
version = { engine: "e2", page: "p2" };
await checkVersion(); await tick();
check(workers[1].msgs.some((m) => m.type === "upgrade"), "new page: worker asked to finish its slice");
workers[1].onmessage({ data: { type: "stopped", upgrade: true } }); await tick();
check(reloaded && store.get("gb_running") === "1", "after the slice: page reloads and will resume");

if (fail) { console.log(`${fail} FAILED`); process.exit(1); }
console.log("LIVE UPDATES OK");
process.exit(0);
