// goldbach-web worker: leases slices from the coordinator and verifies them on
// this device's GPU (WebGPU) with the exact WASM core. One slice at a time;
// "stop" finishes nothing half-way: the lease is released and the slice is
// re-assigned to another device. "finish" (stop after this slice) lets the
// current slice run to the end, uploads its result, then stops.
// The page starts worker.js?v=<engine hash>. The modules, the WASM and the
// shader pack are loaded with the same query, so the browser caches them for
// good (a new engine has a new hash) and never mixes two engine versions.
// Loaded on the first start (onmessage below stays synchronous: no message is
// missed while they load).
const V = new URL(import.meta.url).search;
let createCore, SliceRunner, engineDefaults, loadPrimes, opfsStore, selfTest;
async function loadModules() {
  if (createCore) return;
  const [core, engine, pcache, selftest] = await Promise.all([
    import(`./core.js${V}`), import(`./engine.js${V}`), import(`./pcache.js${V}`), import(`./selftest.js${V}`)]);
  ({ SliceRunner, engineDefaults } = engine);
  ({ loadPrimes, opfsStore } = pcache);
  ({ selfTest } = selftest);
  createCore = core.default;
}

let stopping = false;
let finishing = false;       // stop once the current slice is done (no next lease)
let upgrading = false;       // a new engine is on the server: finish this slice, then hand over
let running = false;
let M = null, device = null, adapterInfo = null, src = null;

const post = (type, data = {}) => postMessage({ type, ...data });
const api = async (path, body) => {
  const r = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
  return r.json();
};

const NEED_FULL = 640 * 1024 * 1024;   // full engine: hit-list states (~600 MB), base primes (437 MB)
const NEED_LITE = 128 * 1024 * 1024;   // lite engine: the half-gap stream (109 MB)
let lite = false;
let liteWhy = null;               // selected once with the adapter limits; shown in the device label
let forceLite = false;                 // ?lite on the page: phone mode on any device
// The page may run a second worker on the low-power adapter (an integrated GPU
// beside a discrete one). It runs only if that is a different GPU, always in
// lite mode (integrated GPUs share the system memory).
let power = "high-performance", secondary = false;
class NoSecond extends Error {}
const mib = (bytes) => Math.floor(bytes / 2 ** 20);   // rounded down: never overstate a limit
const adapterKey = (i) => [i?.vendor, i?.architecture, i?.device, i?.description].map((x) => x ?? "").join("|");

// Explain why the lightweight engine is selected; null means the full engine.
// Desktop browsers (macOS Safari included: its UA says "Macintosh") get the full
// engine whenever the adapter can hold its buffers. Lite is chosen only for a
// stated reason, which the device label shows; never call that "phone mode".
function liteReasonFor(L) {
  if (forceLite) return "forced by ?lite";
  if (secondary) return "secondary GPU";
  const ua = navigator.userAgent || "";
  if (/iPhone|iPad|iPod|Android/i.test(ua)) return "mobile browser";
  const mem = navigator.deviceMemory; // Chromium only (GB, capped at 8)
  if (mem !== undefined && mem <= 4) return "device memory ≤ 4 GB";
  if (L.maxBufferSize < NEED_FULL || L.maxStorageBufferBindingSize < NEED_FULL)
    return `WebGPU limits (buffer ${mib(L.maxBufferSize)} MiB, storage binding ${mib(L.maxStorageBufferBindingSize)} MiB; full engine needs ${mib(NEED_FULL)} MiB)`;
  return null;
}
async function setup() {
  await loadModules();
  if (!device) await setupGpu();
  if (!M) {
    post("status", { text: "Loading the verifier core…" });
    M = await createCore({ locateFile: (path, dir) => dir + path + V });
    post("status", { text: `Loading the 109 million base primes${lite ? " (lite mode)" : ""}…` });
    const store = await opfsStore().catch(() => null);
    const got = await loadPrimes(M, lite, store, (s) => {
      if (s === "generating") post("status", { text: `Generating the 109 million base primes (first visit only${lite ? ", lite mode" : ""})…` });
    });
    post("status", { text: `Base primes ready in ${got.seconds.toFixed(1)} s (${got.source === "cache" ? "from this device's cache" : "generated, now cached on this device"})` });
  }
  const name = adapterInfo.description || [adapterInfo.vendor, adapterInfo.architecture].filter(Boolean).join(" ") || "WebGPU device";
  return lite ? `${name} (lite mode: ${liteWhy || "device constraints"})` : name;
}

async function setupGpu() {
  {
    if (!navigator.gpu) throw new Fatal("WebGPU is not available in this browser (try Chrome or Edge 121+).");
    const adapter = await navigator.gpu.requestAdapter({ powerPreference: power });
    if (!adapter) throw secondary ? new NoSecond() : new Fatal("No WebGPU adapter.");
    if (secondary) {
      const main = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
      const k = adapterKey(adapter.info);
      if (!main || k.replaceAll("|", "") === "" || k === adapterKey(main.info)) throw new NoSecond();
    }
    const L = adapter.limits;
    if (L.maxComputeWorkgroupStorageSize < 17408 || L.maxStorageBuffersPerShaderStage < 8)
      throw new Fatal("This GPU's WebGPU limits are too small for the verifier.");
    if (M === null) {
      liteWhy = liteReasonFor(L);
      lite = liteWhy !== null; // decided once, before the primes exist
    }
    const need = lite ? NEED_LITE : NEED_FULL;
    if (L.maxBufferSize < need || L.maxStorageBufferBindingSize < need)
      throw new Fatal(`The ${lite ? "lite" : "full"} WebGPU engine needs ${mib(need)} MiB per buffer; this GPU allows ${mib(L.maxBufferSize)} MiB (maxBufferSize) and ${mib(L.maxStorageBufferBindingSize)} MiB (maxStorageBufferBindingSize).`);
    device = await adapter.requestDevice({ requiredLimits: {
      maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
      maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
      maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
    device.lost.then((i) => { device = null; post("status", { text: `GPU device lost (${i.message}); it will be recreated`, error: true }); });
    device.onuncapturederror = (e) => post("error", { text: `GPU error: ${String(e.error?.message).slice(0, 300)}` });
    adapterInfo = adapter.info ?? {};
    src ??= await kernels(lite);
  }
  return adapterInfo.description || [adapterInfo.vendor, adapterInfo.architecture].filter(Boolean).join(" ") || "WebGPU device";
}

// The WGSL kernels this mode runs (the engine compiles every one it is given):
// one request for the mode's shader pack ({ tile, check, count30, large and
// harvest or largeLite and harvestLite }, built by the server from kernels/*.wgsl),
// fetched once per worker: a recreated device reuses them.
async function kernels(lite) {
  const pack = `kernels/${lite ? "lite" : "full"}.json${V}`;
  const r = await fetch(pack);
  if (!r.ok) throw new Error(`${pack}: HTTP ${r.status}`);
  return r.json();
}

// Segment size and batch: the largest that fits this device (results do not
// depend on them; only speed does).
const segments = () => engineDefaults(lite).segments;

// Engine configuration per mode (the worker and the benchmark CI use it, so a
// branch that changes it is measured): hit-list batch, list memory, segment sizes
// to try (the first that fits the device).
const engineCfg = () => engineDefaults(lite).cfg;

// Known-answer self-test, once per GPU device: a device that computes a wrong
// result never submits one.
let tested = null;
async function checkDevice() {
  if (tested === device) return;
  post("status", { text: "Checking this GPU against a known result (a few seconds)…" });
  let r = null, lastErr = null;
  for (const seg of segments()) {
    try { r = await selfTest(M, device, src, engineCfg(), seg); break; } catch (e) { lastErr = e; }
  }
  if (!r) throw lastErr;
  if (!r.ok) throw new Fatal(`This GPU/driver computed a wrong answer in the self-test (${r.hash}), so it cannot take part. ` +
    "Some Linux AMD drivers (Mesa RADV) have a known bug here; another browser, device or a driver update may help.");
  post("status", { text: `GPU check passed in ${r.seconds.toFixed(1)} s` });
  tested = device;
}

// One runner per GPU device (what it keeps between slices belongs to that device).
let runner = null;
function sliceRunner() {
  if (runner?.dev !== device) {
    runner?.destroy();
    runner = new SliceRunner(M, device, src, engineCfg(), segments());
  }
  return runner;
}

// Lease-ahead: ~AHEAD_S before a slice ends the worker leases its next one
// (the server keeps the current lease active), so the GPU does not wait for
// the network between slices; results are posted in the background.
const AHEAD_S = 30;
let ahead = null;            // promise of the next lease (or null)
let aheadLease = null;       // the next lease once it arrived (heartbeated with the current one)
const pending = new Set();   // result posts in flight (awaited before the worker stops)

function takeAhead() {
  const p = ahead;
  ahead = null;
  aheadLease = null;
  return p ? p.catch(() => null) : Promise.resolve(null);
}
async function releaseAhead(client) {
  const l = await takeAhead();
  if (l && l.lease) await api("/api/release", { client, lease: l.lease }).catch(() => {});
}

async function postResult(client, lease, out) {
  const body = JSON.stringify({ client, lease: lease.lease, elapsed: out.elapsed, result: out.result }, big);
  for (let attempt = 0; ; attempt++) {
    try {
      const r = await fetch("/api/result", { method: "POST", headers: { "Content-Type": "application/json" }, body });
      if (!r.ok) throw new Error(`/api/result: HTTP ${r.status}`);
      const res = await r.json();
      post("done", { slice: lease.slice, status: res.status, elapsed: out.elapsed, pmax: out.result.pmax });
      return;
    } catch (e) {
      if (attempt >= 3) {
        const msg = String(e?.message ?? e);
        api("/api/report", { client, lease: lease.lease, error: `result upload: ${msg}`.slice(0, 500) }).catch(() => {});
        await api("/api/release", { client, lease: lease.lease }).catch(() => {});
        post("status", { text: `Could not upload the result of slice #${lease.slice}: ${msg}`, error: true });
        return;
      }
      await new Promise((ok) => setTimeout(ok, 5000 * (attempt + 1)));
    }
  }
}

async function runSlice(client, lease) {
  const start = BigInt(lease.start), limit = BigInt(lease.limit);
  const run = sliceRunner();
  const eng = await run.open(start, limit);
  const t0 = performance.now();
  let beat = t0, lastPost = 0, done = 0n;
  const total = (limit - start) / 2n + 1n;
  const n = eng.geo.segs.length;
  try {
    // segments in flight (pipelined): the GPU never waits for the host
    let lost = false;
    const all = await eng.runAll(async (k) => {
      if (stopping) return false;
      done += eng.geo.segs[k].segEvens;
      const now = performance.now();
      if (now - lastPost > 250 || k === n - 1) {
        lastPost = now;
        post("progress", { slice: lease.slice, done: Number(done), total: Number(total), rate: Number(done) / ((now - t0) / 1000), segment: Number(eng.geo.segment) });
      }
      // heartbeats without waiting (the GPU keeps its segments in flight); a
      // network error only skips one (the lease expires after 240 s without)
      if (now - beat > 20000) {
        beat = now;
        api("/api/heartbeat", { client, lease: lease.lease, done: String(done) })
          .then((h) => { if (!h.ok) lost = true; }).catch(() => {});
        const al = aheadLease;
        if (al) api("/api/heartbeat", { client, lease: al.lease, done: "0" })
          .then((h) => { if (!h.ok && aheadLease === al) { ahead = null; aheadLease = null; } }).catch(() => {});
      }
      if (lost) return false;                           // lease lost: drop the slice
      // the next slice, ~AHEAD_S before this one ends
      if (!ahead && !stopping && !finishing && !upgrading && done > 0n) {
        const rate = Number(done) / ((now - t0) / 1000);
        if (rate > 0 && Number(total - done) / rate < AHEAD_S) {
          ahead = api("/api/lease", { client, ahead: true })
            .then((l) => (l && l.lease ? l : null)).catch(() => null);
          const p = ahead;
          p.then((l) => { if (ahead === p) aheadLease = l; });
        }
      }
      return true;
    });
    if (!all || lost || stopping) return null;
    const r = eng.finish();
    return { result: { ...r, pass: true }, elapsed: (performance.now() - t0) / 1000 };
  } finally {
    run.close(eng);
  }
}

const big = (_, v) => (typeof v === "bigint" ? v.toString() : v);

// Errors that retrying cannot fix: the loop ends and says why.
class Fatal extends Error {}

async function loop(client) {
  running = true;
  let backoff = 5;
  try {
    while (!stopping && !finishing && !upgrading) {
      let lease = null;
      try {
        const gpu = await setup();              // recreates a lost GPU device
        post("device", { gpu });
        await checkDevice();
        lease = await takeAhead();                // leased ahead during the previous slice
        if (!lease) lease = await api("/api/lease", { client });
        if (lease.wait) {
          post("status", { text: `No slice available (${lease.why}); retrying in ${lease.wait} s` });
          lease = null;
          await sleep(300 * 1000);
          continue;
        }
        post("status", { text: `Verifying slice #${lease.slice}: [${lease.start}, ${lease.limit}]`, lease });
        const out = await runSlice(client, lease);
        if (!out) {                              // stopped, or the lease was lost
          await api("/api/release", { client, lease: lease.lease }).catch(() => {});
          if (stopping || upgrading) await releaseAhead(client);
          continue;
        }
        // the result goes up in the background: the next slice starts at once
        const up = postResult(client, lease, out);
        pending.add(up);
        up.finally(() => pending.delete(up));
        lease = null;
        if (upgrading) { await releaseAhead(client); break; }
        backoff = 5;
      } catch (e) {
        if (e instanceof NoSecond) { post("nosecond"); return; }
        const msg = String(e?.message ?? e);
        if (lease) await api("/api/release", { client, lease: lease.lease }).catch(() => {});
        await releaseAhead(client);
        api("/api/report", { client, lease: lease?.lease ?? 0, error: msg.slice(0, 500) }).catch(() => {});
        if (e instanceof Fatal || /COUNTEREXAMPLE/.test(msg)) throw e;
        if (/device lost|GPU error/i.test(msg)) device = null;
        post("status", { text: `Error: ${msg} · retrying in ${backoff} s`, error: true });
        await sleep(backoff * 1000);
        backoff = Math.min(300, backoff * 2);
      }
    }
  } catch (e) {
    post("error", { text: String(e?.message ?? e) });
  } finally {
    await releaseAhead(client).catch(() => {});  // a next slice leased ahead goes back to the pool
    await Promise.allSettled([...pending]);     // never stop with a result still uploading
    running = false;
    stopping = false;
    finishing = false;
    post("stopped", { upgrade: upgrading });
  }
}
const sleep = (ms) => new Promise((r) => {
  const t0 = Date.now();
  const iv = setInterval(() => { if (stopping || finishing || upgrading || Date.now() - t0 >= ms) { clearInterval(iv); r(); } }, 500);
});

onmessage = (ev) => {
  const m = ev.data;
  if (m.type === "start" && !running) {
    stopping = false;
    finishing = false;
    if (m.lite && M === null) forceLite = true;
    if (m.secondary && M === null) { secondary = true; power = "low-power"; }
    loop(m.client);
  }
  if (m.type === "upgrade") { upgrading = true; if (!running) post("stopped", { upgrade: true }); }
  if (m.type === "finish" && running && !stopping) {
    finishing = !!m.on;
    post("status", { text: finishing ? "Stopping after the current slice…" : "Continuing with the next slice after this one." });
  }
  if (m.type === "stop" && running) { stopping = true; post("status", { text: "Stopping after the current segment…" }); }
};
