// GPU timeline of a pipelined run: where the time between the GPU passes goes.
//
// test/prof.mjs and the benchmark's pass sums time each pass in isolation (one
// segment at a time); the slice wall time is above that sum. This tool runs a
// window the way the worker does (DenseEngine.runAll: batches of segments in
// flight) and reports the wall time per segment. A "stamped" run also writes a
// timestamp at the start and end of every compute pass on one GPU clock and
// splits the GPU span into
//   * busy time: inside the passes,
//   * gaps inside a segment (between its passes),
//   * gaps between segments of one submit, and between submits (clears,
//     staging and readback copies, barriers, submit overhead).
//   node test/timeline.mjs [START LIMIT]
//   env: LITE=1 phone mode; SEG segment size (default: the mode's first);
//        CFG = JSON engine config overrides;
//        SEQ = comma-separated runs, each on a new engine (default
//              "plain,stamped"): plain | stamped, optionally suffixed
//              +noread (readback copies except the 16-byte header),
//              +smallread (every readback copy cut to 256 bytes), +nolists (no
//              ridge and leftover copies), +nohist (no histogram and first-
//              occurrence copies), +nostage (no staging copies),
//              +noclear (no per-segment clears): ablations for timing only,
//              their results are not checked (the kernels reuse the inputs
//              staged by the warm-up segment);
//        FRESH=1 a new adapter and device per run (as the benchmark CI).
// Exit status 1 if a full (non-ablated) run's results differ from the first one's.
// The production machine's GPU: run through tools/gpu_slot.sh.
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { initPrimes } from "./primes.mjs";
import { DenseEngine, engineDefaults } from "../client/engine.js";

const lite = process.env["LITE"] === "1";
const [s0, l0] = process.argv.slice(2);
const start = BigInt(s0 ?? "4000000000000000000"), limit = BigInt(l0 ?? "4000000029919999998");
const d = engineDefaults(lite);
const cfg = { ...d.cfg, ...JSON.parse(process.env["CFG"] ?? "{}") };
const seg = BigInt(process.env["SEG"] ?? d.segments[0]);
const seq = (process.env["SEQ"] ?? "plain,stamped").split(",");
const M = await loadCore();
await initPrimes(M, lite);
const names = { tile: "tile", large: "large", harvest: "harvest", check: "check", count30: "count30",
  largeLite: "large_lite", harvestLite: "harvest_lite" };
const src = {};
for (const [k, f] of Object.entries(names)) src[k] = await readFile(new URL(`../kernels/${f}.wgsl`, import.meta.url), "utf8");

async function newDevice() {
  const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
  const L = adapter.limits;
  return adapter.requestDevice({ requiredFeatures: ["timestamp-query"], requiredLimits: {
    maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
    maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
    maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
}
const fresh = process.env["FRESH"] === "1";
let shared = null;
const canon = (r) => JSON.stringify(r, (_, v) => (typeof v === "bigint" ? v.toString() : v));
let reference = null, differ = false;

/// One run of the window on a new engine; `spec` is plain|stamped[+ablation...].
async function run(spec) {
  const [kind, ...ablate] = spec.split("+");
  const stamped = kind === "stamped";
  const device = fresh ? await newDevice() : (shared ??= await newDevice());
  const eng = new DenseEngine(M, device, src, cfg);
  await eng.prepare(start, limit, seg);
  const nseg = eng.geo.segs.length;
  const e = eng;
  const MAXQ = 4096;
  const qs = stamped ? device.createQuerySet({ type: "timestamp", count: MAXQ }) : null;
  const stamps = [];
  let curSeg = -1, curSubmit = -1, armed = false;
  // ablations: an encoder that drops the chosen copies and clears (after the warm-up)
  const rbs = new Set(e.rbs);
  const filter = (enc) => (!armed || ablate.length === 0) ? enc : new Proxy(enc, {
    get(t, p) {
      if (p === "copyBufferToBuffer") return (s, so, dst, ...r) => {
        // the 16-byte io header stays: without a copy into it, a readback
        // buffer's mapAsync would not wait for the segment's work
        if (ablate.includes("noread") && rbs.has(dst) && r[1] > 16) return;
        if (ablate.includes("smallread") && rbs.has(dst)) { t.copyBufferToBuffer(s, so, dst, r[0], Math.min(r[1], 256)); return; }
        if (ablate.includes("nolists") && rbs.has(dst) && s === e.bIO && r[1] > 16) return;
        if (ablate.includes("nohist") && rbs.has(dst) && (s === e.bHist || s === e.bFirst)) return;
        if (ablate.includes("nostage") && s === e.bStage) return;
        if (ablate.includes("noclear") && s === e.bFirstInit) return;
        t.copyBufferToBuffer(s, so, dst, ...r);
      };
      if (p === "clearBuffer" && ablate.includes("noclear")) return () => {};
      const v = Reflect.get(t, p);
      return typeof v === "function" ? v.bind(t) : v;
    },
  });
  const encodeMap = e.encodeMap.bind(eng), encodeCheck = e.encodeCheck.bind(eng), submit = e.submit.bind(eng);
  e.encodeMap = (enc, k) => { curSeg = k; encodeMap(filter(enc), k); };
  e.encodeCheck = (enc, k) => { curSeg = k; encodeCheck(filter(enc), k); };
  e.submit = (k, k1) => { curSubmit++; submit(k, k1); };
  if (qs) e.pass = (enc, name) => {
    const i = stamps.length;
    if (2 * i + 2 > MAXQ) return enc.beginComputePass();   // out of queries: unstamped
    stamps.push({ name, seg: curSeg, submit: curSubmit });
    return enc.beginComputePass({ timestampWrites: { querySet: qs, beginningOfPassWriteIndex: 2 * i, endOfPassWriteIndex: 2 * i + 1 } });
  };
  if (ablate.length) {
    // the results of an ablated run are meaningless: only wait for the GPU
    e.collect = async (k) => { await e.maps[k % e.depth]; e.rbs[k % e.depth].unmap(); };
  }
  await eng.runAll((k) => k < 1);                  // warm-up: clocks up, caches, inputs staged
  eng.resetTotals();
  stamps.length = 0;
  armed = true;
  const t0 = performance.now();
  await eng.runAll();
  const wallMs = (performance.now() - t0) / nseg;
  let note = "";
  if (!ablate.length) {
    const r = canon(eng.finish());
    reference ??= r;
    if (r !== reference) { differ = true; note = "  RESULTS DIFFER"; }
  } else note = "  (ablation: results not checked)";
  console.log(`${spec.padEnd(24)} ${nseg} segments  wall ${wallMs.toFixed(3)} ms/segment${note}`);
  if (qs && stamps.length) {
    const nq = 2 * stamps.length;
    const bq = device.createBuffer({ size: nq * 8, usage: GPUBufferUsage.QUERY_RESOLVE | GPUBufferUsage.COPY_SRC });
    const rb = device.createBuffer({ size: nq * 8, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    const enc = device.createCommandEncoder();
    enc.resolveQuerySet(qs, 0, nq, bq, 0);
    enc.copyBufferToBuffer(bq, 0, rb, 0, nq * 8);
    device.queue.submit([enc.finish()]);
    await rb.mapAsync(GPUMapMode.READ);
    const t = new BigUint64Array(rb.getMappedRange()).slice();
    rb.unmap(); rb.destroy(); bq.destroy(); qs.destroy();
    report(stamps, t, nseg);
  }
  eng.destroy();
  if (fresh) device.destroy();
}

/// Splits a stamped timeline into busy time and the three kinds of gaps (ms per segment).
function report(stamps, t, nseg) {
  const ms = (i) => Number(t[i] - t[0]) / 1e6;
  const busy = {};
  const gapAfter = {};
  let inSeg = 0, betweenSeg = 0, betweenSubmit = 0, overlap = 0;
  for (let i = 0; i < stamps.length; i++) {
    const s = stamps[i], b = ms(2 * i), en = ms(2 * i + 1);
    busy[s.name] = (busy[s.name] ?? 0) + (en - b);
    if (i === 0) continue;
    const p = stamps[i - 1], g = b - ms(2 * i - 1);
    if (g < 0) { overlap -= g; continue; }
    if (p.submit !== s.submit) betweenSubmit += g;
    else if (p.seg !== s.seg) betweenSeg += g;
    else { inSeg += g; gapAfter[`${p.name}->${s.name}`] = (gapAfter[`${p.name}->${s.name}`] ?? 0) + g; }
  }
  const per = (x) => (x / nseg).toFixed(3);
  const busySum = Object.values(busy).reduce((a, b) => a + b, 0);
  console.log(`  GPU span                ${per(ms(t.length - 1))}`);
  console.log(`  busy (in passes)        ${per(busySum)}  ` + Object.entries(busy).map(([k, v]) => `${k} ${per(v)}`).join(", "));
  console.log(`  gaps inside a segment   ${per(inSeg)}  ` + Object.entries(gapAfter).sort((a, b) => b[1] - a[1]).map(([k, v]) => `${k} ${per(v)}`).join(", "));
  console.log(`  gaps between segments   ${per(betweenSeg)}  (same submit)`);
  console.log(`  gaps between submits    ${per(betweenSubmit)}`);
  if (overlap > 0) console.log(`  overlapping passes      ${per(overlap)}  (negative gaps)`);
}

console.log(`${lite ? "phone" : "full"} mode, segment ${seg}, window [${start}, ${limit}], ${fresh ? "a new device per run" : "one device"}`);
for (const spec of seq) await run(spec);
if (differ) process.exit(1);
