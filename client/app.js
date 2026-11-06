// goldbach-web page: keeps the server-rendered board live (one small poll,
// /api/live, that carries only what changed) and drives this device's
// verifier worker (start / stop).
const $ = (id) => document.getElementById(id);
const store = { get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
                set: (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } } };
const sci = (x) => (x < 1e4 ? String(Math.round(x)) : `${(x / 10 ** Math.floor(Math.log10(x))).toFixed(2)}e${Math.floor(Math.log10(x))}`);

// Layout depends only on the CSS viewport width, never on the device or the
// engine. The same breakpoint is the compact-control media query in index.html;
// the server cannot know the width, so the page carries both boards (desktop shown).
const LAYOUT_BREAKPOINT = 600;
function narrow() {
  return window.matchMedia
    ? window.matchMedia(`(max-width: ${LAYOUT_BREAKPOINT}px)`).matches
    : document.documentElement.clientWidth <= LAYOUT_BREAKPOINT;
}
const numberText = (v) => {
  if (!Number.isFinite(v) || v <= 0) return "0";
  if (v >= 1e9) return `${(v / 1e9).toFixed(2)} G`;
  if (v >= 1e6) return `${(v / 1e6).toFixed(2)} M`;
  if (v >= 1e3) return `${(v / 1e3).toFixed(2)} k`;
  return String(Math.round(v));
};
// ---- the server's own CPU verifier, per deployed version --------------------
let srv = null;               // { versions: [{ c, s, from, n, mean }] oldest first, last: { t, rate } }
const pct = (a, b) => `${a >= b ? "+" : ""}${((a / b - 1) * 100).toFixed(1)}%`;
// one <pre>: a summary line, then a row per version; re-rendered on every poll
// (only the "ago" moves between slices)
function renderServer() {
  if (!srv) return;
  const all = srv.versions;
  const wide = !narrow();                              // phones: no change subjects
  const lines = all.slice().reverse().map((g, i, newest) => {
    const prev = newest[i + 1];
    const name = g.c === "untagged" ? "(older)" : g.c;
    return `${name.padEnd(13)} ${new Date(g.from * 1000).toISOString().slice(0, 10)}  ${String(g.n).padStart(6)}  ` +
      `${(numberText(g.mean) + " evens/s").padStart(15)}  ${(prev ? pct(g.mean, prev.mean) : "").padStart(7)}` +
      `${wide ? "  " + (g.s ?? "").slice(0, 24) : ""}`;
  });
  if (!lines.length) { $("srv-table").textContent = "No server slices yet."; return; }
  const latest = all[all.length - 1], prev = all[all.length - 2];
  const ago = Math.max(0, Date.now() / 1000 - srv.last.t);
  $("srv-table").textContent =
    `now ${numberText(latest.mean)} evens/s${prev ? ` (${pct(latest.mean, prev.mean)} vs previous version)` : ""}\n` +
    `last slice ${numberText(srv.last.rate)} evens/s, ${ago < 3600 ? `${Math.round(ago / 60)} min` : `${(ago / 3600).toFixed(1)} h`} ago\n\n` +
    `version       deployed    slices        avg speed  vs prev${wide ? "  change" : ""}\n${lines.join("\n")}`;
}

// ---- this device -------------------------------------------------------------
$("name").value = store.get("gb_name") || "";
$("name").addEventListener("change", () => store.set("gb_name", $("name").value.trim()));
let worker = null, running = false, done = Number(store.get("gb_done") || 0);
$("d-count").textContent = done;

function status(t, err = false) { $("d-status").textContent = t; $("d-status").classList.toggle("err", err); }
function setRunning(on) {
  running = on; $("go").textContent = on ? "stop" : "start"; $("go").disabled = false;
  setFinishing(false); $("finish").hidden = !on;
}
// "stop after slice": the current slice runs to the end (its result counts),
// then the worker stops instead of leasing the next one. Pressed again, it keeps going.
let finishing = false;
function setFinishing(on) {
  finishing = on;
  $("finish").textContent = on ? "keep going" : "stop after slice";
  $("finish").setAttribute("aria-pressed", String(on));
}

let wake = null;
async function keepAwake(on) {
  try {
    if (on && !wake && "wakeLock" in navigator && document.visibilityState === "visible") {
      wake = await navigator.wakeLock.request("screen");
      wake.addEventListener("release", () => { wake = null; });
    } else if (!on && wake) { await wake.release(); wake = null; }
  } catch { /* not granted */ }
}

async function clientId() {
  let id = store.get("gb_client");
  if (id) return id;
  const r = await fetch("/api/register", { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: $("name").value.trim() || "device", gpu: navigator.userAgent.slice(0, 100) }) });
  id = (await r.json()).client;
  store.set("gb_client", id);
  return id;
}
let worker2 = null, noSecond = new URLSearchParams(location.search).has("one");
async function clientId2() {
  let id = store.get("gb_client2");
  if (id) return id;
  const r = await fetch("/api/register", { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: `${$("name").value.trim() || "device"} (2nd GPU)`, gpu: navigator.userAgent.slice(0, 100) }) });
  id = (await r.json()).client;
  store.set("gb_client2", id);
  return id;
}
// A second worker only for a second, different GPU (an integrated one beside a
// discrete one), checked here first: on a one-GPU device no worker is started
// (and none of its modules, WASM or shaders downloaded) just to find that out.
const adapterKey = (i) => [i?.vendor, i?.architecture, i?.device, i?.description].map((x) => x ?? "").join("|");
async function hasSecondGpu() {
  try {
    const [main, low] = await Promise.all([navigator.gpu.requestAdapter({ powerPreference: "high-performance" }),
                                           navigator.gpu.requestAdapter({ powerPreference: "low-power" })]);
    const k = adapterKey(low?.info);
    return !!main && !!low && k.replaceAll("|", "") !== "" && k !== adapterKey(main.info);
  } catch { return false; }
}
async function startSecond() {
  if (noSecond || worker2 || !running) return;
  if (!(await hasSecondGpu())) { noSecond = true; return; }
  if (worker2 || !running) return;
  const id = await clientId2();
  worker2 = new Worker(`worker.js?v=${versions.engine}`, { type: "module" });
  worker2.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === "nosecond") { noSecond = true; worker2?.terminate(); worker2 = null; return; }
    if (m.type === "device") $("d-gpu2").textContent = m.gpu;
    if (m.type === "progress") $("d-gpu2").textContent = `${$("d-gpu2").textContent.split(" · ")[0]} · ${sci(m.rate)} evens/s`;
    if (m.type === "error") { $("d-gpu2").textContent = `off: ${m.text.slice(0, 120)}`; noSecond = true; worker2?.terminate(); worker2 = null; }
    if (m.type === "done") { done++; store.set("gb_done", String(done)); $("d-count").textContent = done; poll(); }
  };
  worker2.postMessage({ type: "start", client: id, secondary: true });
}
function stopSecond() { worker2?.postMessage({ type: "stop" }); }
function finishSecond(on) { worker2?.postMessage({ type: "finish", on }); }

async function start() {
  if (!("gpu" in navigator)) {
    status("WebGPU is not available in this browser. iPhone: iOS 26 or later (Safari).\nComputers: a recent Chrome, Edge or Safari.", true);
    return;
  }
  $("go").disabled = true;
  const id = await clientId();
  worker ??= new Worker(`worker.js?v=${versions.engine}`, { type: "module" });
  worker.onmessage = (ev) => {
    const m = ev.data;
    if (m.type === "status") status(m.text, !!m.error);
    if (m.type === "error") { status(m.text, true); store.set("gb_running", "0"); }
    if (m.type === "device") { $("d-gpu").textContent = m.gpu; startSecond(); }
    if (m.type === "progress") {
      $("d-slice").textContent = `#${m.slice}`;
      $("d-prog").textContent = `${((100 * m.done) / m.total).toFixed(1)} %`;
      $("d-rate").textContent = `${sci(m.rate)} evens/s`;
    }
    if (m.type === "done") {
      done++; store.set("gb_done", String(done));
      $("d-count").textContent = done;
      status(`slice #${m.slice} finished in ${m.elapsed.toFixed(0)} s: ${m.status}`);
      poll();
    }
    if (m.type === "stopped" && m.upgrade) { handOver(); return; }
    if (m.type === "stopped") {
      const second = finishing;                        // the 2nd GPU finishes its own slice too
      setRunning(false); keepAwake(false); $("d-prog").textContent = "-";
      if (!second) stopSecond();
      if (second) status("Stopped after the slice.");
      schedule();
    }
  };
  worker.postMessage({ type: "start", client: id, lite: new URLSearchParams(location.search).has("lite") });
  if (worker2) {                                     // the 2nd GPU may still be finishing a slice: keep it going
    finishSecond(false);
    worker2.postMessage({ type: "start", client: await clientId2(), secondary: true });
  }
  store.set("gb_running", "1");
  setRunning(true);
  keepAwake(true);
  schedule();
}
$("go").addEventListener("click", () => {
  if (running) { store.set("gb_running", "0"); $("go").disabled = true; worker.postMessage({ type: "stop" }); stopSecond(); return; }
  start();
});
$("finish").addEventListener("click", () => {
  if (!running) return;
  const on = !finishing;
  setFinishing(on);
  store.set("gb_running", on ? "0" : "1");          // a reload does not resume a run that is ending
  worker.postMessage({ type: "finish", on });
  finishSecond(on);
});

// ---- live data ------------------------------------------------------------------
// The page arrives with its first data (live-init); then one poll every 30 s while
// it is visible. Hidden, it polls only while verifying (every 2 min: engine
// updates) and not at all otherwise. Each poll names what the page already has
// (board hash, newest server slice), so unchanged parts are not sent again, and
// asks for no speed buckets (tiers= empty: the page has no chart).
const POLL_MS = 30000, POLL_HIDDEN_MS = 120000;
const init = JSON.parse($("live-init").textContent);
let versions = init.v, next = null;
let shownNarrow = init.narrow, boardHash = "", timer = 0, inFlight = null, lastPoll = Date.now();
function apply(live, n) {
  if (live.board !== undefined) { $("board").textContent = live.board; shownNarrow = n; }
  boardHash = live.bh;
  if (live.srv) srv = live.srv;
  renderServer();
}
function poll() {
  if (inFlight) return inFlight;
  clearTimeout(timer);
  const n = narrow();
  inFlight = (async () => {
    try {
      const r = await fetch(`/api/live?narrow=${n ? 1 : 0}&bh=${boardHash}&tiers=${srv ? `&runs=${srv.last.t}` : ""}`,
                            { cache: "no-store" });
      if (r.ok) {
        const live = await r.json();
        apply(live, n);
        checkVersion(live.v);
      }
    } catch { /* offline: keep what is shown */ }
    lastPoll = Date.now();
    inFlight = null;
    schedule();
  })();
  return inFlight;
}
function schedule() {
  clearTimeout(timer);
  if (document.visibilityState === "visible") timer = setTimeout(poll, POLL_MS);
  else if (running) timer = setTimeout(poll, POLL_HIDDEN_MS);
}
document.addEventListener("visibilitychange", () => {
  if (running) keepAwake(true);
  if (document.visibilityState === "visible" && Date.now() - lastPoll >= POLL_MS) poll();
  else schedule();
});
let resizeTimer = 0;
addEventListener("resize", () => { clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { if (narrow() !== shownNarrow) poll(); }, 200); });
// Keep the server-rendered layout in sync with the viewport immediately.
// A resize poll alone may miss this when width changes without a resize event.
const onLayoutChange = () => {
  renderServer();                                      // the change column comes and goes
  if (narrow() !== shownNarrow) poll();
};
if (window.matchMedia) {
  const layoutQuery = window.matchMedia(`(max-width: ${LAYOUT_BREAKPOINT}px)`);
  layoutQuery.addEventListener?.("change", onLayoutChange);
}

function checkVersion(v) {
  if (!v || (v.engine === versions.engine && v.page === versions.page)) return;
  if (next && next.engine === v.engine && next.page === v.page) return;
  next = v;
  if (worker && running) { status("A new version of the verifier is on the server: switching after this slice."); worker.postMessage({ type: "upgrade" }); worker2?.postMessage({ type: "upgrade" }); }
  else handOver();
}
function handOver() {
  const pageChanged = next && next.page !== versions.page;
  worker?.terminate();
  worker = null;
  worker2?.terminate();
  worker2 = null;
  versions = next ?? versions;
  next = null;
  if (pageChanged) { location.reload(); return; }
  if (store.get("gb_running") === "1") { status("Updated to the new verifier."); start(); }
  else if (running) { setRunning(false); keepAwake(false); $("d-prog").textContent = "-"; }   // stopped after the slice
  else setRunning(false);
}

if (narrow() && init.nb !== undefined) Object.assign(init, { board: init.nb, bh: init.nbh, narrow: true });
apply(init, init.narrow);
if (narrow() !== shownNarrow) poll(); else schedule();
if (store.get("gb_running") === "1") start();

// ---- WebMCP: the page's tools for an AI agent driving this browser ----------------
// navigator.modelContext (https://webmachinelearning.github.io/webmcp/); absent in
// most browsers, then nothing happens. The same data is at /mcp for agents without one.
if (navigator.modelContext?.registerTool) {
  const tools = new AbortController();
  const text = (t) => ({ content: [{ type: "text", text: t }] });
  const none = { type: "object", properties: {}, additionalProperties: false };
  for (const tool of [
    { name: "goldbach_board", description: "The live status board: progress of the Goldbach verification from 4e18 to 5e18, speed, record minimal partitions p(n), devices.",
      inputSchema: none, execute: async () => text($("board").textContent) },
    { name: "goldbach_partition", description: "p(n), the smallest prime p with n - p prime, and n = p + q, for an even 4 <= n < 2^64 (decimal string), computed exactly by the server.",
      inputSchema: { type: "object", properties: { n: { type: "string", pattern: "^[0-9]+$" } }, required: ["n"], additionalProperties: false },
      execute: async ({ n }) => {
        const r = await fetch("/mcp", { method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "goldbach_partition", arguments: { n: String(n) } } }) });
        return (await r.json()).result;
      } },
    { name: "goldbach_device", description: "This browser's own verifier: whether it runs, its GPU, current slice, progress, speed and slices done.",
      inputSchema: none,
      execute: async () => text([`running ${running}`, ...["gpu", "gpu2", "slice", "prog", "rate", "count"].map((k) => `${k} ${$(`d-${k}`).textContent}`)].join("\n")) },
    { name: "goldbach_start", description: "Start verifying on this device's GPU (WebGPU), as the start button does. Keeps going until stopped.",
      inputSchema: none, execute: async () => { if (!running && !$("go").disabled) $("go").click(); return text(running ? "running" : "starting"); } },
    { name: "goldbach_stop", description: "Stop verifying. after_slice: finish the current slice first so its work counts (default true).",
      inputSchema: { type: "object", properties: { after_slice: { type: "boolean" } }, additionalProperties: false },
      execute: async ({ after_slice = true } = {}) => {
        if (!running) return text("not running");
        if (after_slice) { if ($("finish").getAttribute("aria-pressed") !== "true") $("finish").click(); return text("stopping after this slice"); }
        $("go").click();
        return text("stopped");
      } },
    { name: "goldbach_open_paper", description: "Open the live paper: the 2014 paper's tables and figures recomputed above 4e18.",
      inputSchema: none, execute: async () => { location.href = "/paper"; return text("opening /paper"); } },
  ]) {
    try { navigator.modelContext.registerTool(tool, { signal: tools.signal }); } catch { /* an older draft of the API */ }
  }
  addEventListener("pagehide", () => tools.abort());
}
