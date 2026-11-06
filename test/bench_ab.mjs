// Benchmark CI core: two complete checkouts (A = base, B = candidate: their own
// WASM core, engine and kernels) in ONE process, whole windows in ABBA order on
// the same GPU, one engine resident at a time, repeated. GPU time per
// pass from timestamp queries, and slice wall time as the worker runs slices;
// every repetition's results must be identical.
//   node test/bench_ab.mjs A_ROOT B_ROOT MODE START LIMIT REPS OUT.json
// MODE full | lite. Timestamps from Dawn (test/gpu.mjs) are in ns.
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { loadCore } from "./core.mjs";
import { readFile, writeFile, realpath } from "node:fs/promises";
import { initPrimes } from "./primes.mjs";
const [aRoot, bRoot, mode, s0, l0, reps, out] = process.argv.slice(2);
const lite = mode === "lite";
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
// a fresh device per run: Dawn frees a destroyed engine's ~2 GB lazily, and the
// next engine on the same device landed in slower memory (+10 % for the side
// that ran second); destroying the device releases everything
// (Dawn: an adapter creates one device, so a fresh adapter each time)
const newDevice = async () => (await navigator.gpu.requestAdapter({ powerPreference: "high-performance" })).requestDevice({ requiredFeatures: ["timestamp-query"], requiredLimits: {
  maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const names = { tile: "tile", large: "large", harvest: "harvest", check: "check", count30: "count30", gaps30: "gaps30",
  largeLite: "large_lite", harvestLite: "harvest_lite" };

async function side(root) {
  const abs = await realpath(root);
  const eng = await import(`file://${abs}/client/engine.js`);
  const { DenseEngine } = eng;
  // each side runs with its own defaults (engineDefaults), so configuration changes are measured
  const d = eng.engineDefaults ? eng.engineDefaults(lite)
    : { cfg: lite ? { K: 16, listBytes: 96 << 20, lite: true } : { K: 32, listBytes: 768 << 20 }, segments: [lite ? 128000000n : 467500000n] };
  const M = await loadCore(`${abs}/client`);
  await initPrimes(M, lite, abs);          // GB_PRIMES_CACHE: loaded in ~0.1 s, checked against the known primes
  const src = {};
  // gaps30: a side from before the gap walk moved into count30 (2.5) still has it
  for (const [k, f] of Object.entries(names)) {
    try { src[k] = await readFile(`${abs}/kernels/${f}.wgsl`, "utf8"); } catch (e) { if (f !== "gaps30") throw e; }
  }
  return { M, DenseEngine, SliceRunner: eng.SliceRunner, src, cfg: d.cfg, seg: d.segments[0] };
}
const A = await side(aRoot), B = await side(bRoot);
const canon = (r) => {
  const { pending: _p, leftovers: _l, ...c } = r;
  return JSON.stringify(c, (_, v) => (typeof v === "bigint" ? v.toString() : v));
};

// One engine resident at a time: two full engines (~2 GB each) side by side put
// the second one's buffers in slower memory (an A/A test showed +64 % in the
// large pass for identical code). Each repetition runs one side's whole window,
// then the other's, in ABBA order (linear clock drift cancels).
const sum = (p) => Object.values(p).reduce((x, y) => x + y, 0);
const unit = 233750000;                                 // ms per 233.75M evens (one 467.5M segment)
async function runSide(S) {
  const device = await newDevice();
  const e = new S.DenseEngine(S.M, device, S.src, { ...S.cfg, profile: true });
  await e.prepare(BigInt(s0), BigInt(l0), S.seg);
  const n = e.geo.segs.length;
  await e.runSegment(0);                                // warm-up (not counted): clocks up, caches
  e.prof = {};
  for (let k = 1; k < n; k++) await e.runSegment(k);
  const evens = Number(e.res.evens) - Number(e.geo.segs[0].segEvens);
  const p = Object.fromEntries(Object.entries(e.prof)
    .filter(([k]) => k !== "host" && k !== "wall").map(([k, v]) => [k, v / (evens / unit)]));
  const r = canon(e.finish());
  e.destroy();
  device.destroy();
  return { p, r, n };
}
// Slice wall time, as the worker runs slices (SliceRunner: setup per slice,
// pipelined segments, no timestamp queries): WALL_SLICES consecutive slices of
// WALL_LEN integers; the first one warms up. Converted to a real slice
// (server SLICE = 1e12 integers): steady setup + its segments at the measured
// rate, as ms per 233.75M evens. Catches what the GPU pass sums cannot see:
// per-slice setup, uploads, gaps between passes and submits, host work.
const SLICE = 1000000000000n;
// slice windows of a fixed length (both sides verify the same evens, whatever
// their segment size): 100 x 467.5M (full), 150 x 256M (phone)
const WALL_SLICES = 3, WALL_LEN = lite ? 38400000000n : 46750000000n;
async function wallSide(S) {
  const device = await newDevice();
  const fallback = {   // a base without SliceRunner: what its worker did (an engine per slice)
    async open(s, l) { const e = new S.DenseEngine(S.M, device, S.src, S.cfg); return await e.prepare(s, l, S.seg); },
    close(e) { e.destroy(); }, destroy() {},
  };
  const run = S.SliceRunner ? new S.SliceRunner(S.M, device, S.src, S.cfg, [S.seg]) : fallback;
  let prep = 0, segT = 0, segs = 0;
  const rs = [];
  const s0n = BigInt(s0);
  for (let i = 0; i < WALL_SLICES; i++) {
    const a = s0n + BigInt(i) * WALL_LEN, b = a + WALL_LEN - 2n;
    const t0 = performance.now();
    const e = await run.open(a, b);
    if (e.geo.segment !== S.seg) throw new Error("wall: unexpected segment size");
    const t1 = performance.now();
    await e.runAll();
    const t2 = performance.now();
    rs.push(canon(e.finish()));
    run.close(e);
    if (i > 0) { prep += t1 - t0; segT += t2 - t1; segs += e.geo.segs.length; }
  }
  await device.queue.onSubmittedWorkDone();
  run.destroy();
  device.destroy();
  const realSegs = Number((SLICE + S.seg - 1n) / S.seg);
  const sliceMs = prep / (WALL_SLICES - 1) + realSegs * segT / segs;
  return { w: sliceMs / (Number(SLICE / 2n) / unit), r: rs.join("\n"), prep: prep / (WALL_SLICES - 1), perSeg: segT / segs };
}
const runs = [];
for (let rep = 0; rep < Number(reps); rep++) {
  const abFirst = [0, 3].includes(rep % 4);             // A B B A A B B A ...
  const first = await runSide(abFirst ? A : B), second = await runSide(abFirst ? B : A);
  const ra = abFirst ? first : second, rb = abFirst ? second : first;
  const wf = await wallSide(abFirst ? A : B), ws = await wallSide(abFirst ? B : A);
  const wa = abFirst ? wf : ws, wb = abFirst ? ws : wf;
  const identical = ra.r === rb.r && wa.r === wb.r;
  runs.push({ rep, segments: ra.n, identical, a: ra.p, b: rb.p, aTotal: sum(ra.p), bTotal: sum(rb.p),
              aWall: wa.w, bWall: wb.w, aPrep: wa.prep, bPrep: wb.prep, aSegWall: wa.perSeg, bSegWall: wb.perSeg,
              evensPerSeg: unit, order: abFirst ? "AB" : "BA" });
  console.log(`${mode} rep ${rep} (${abFirst ? "AB" : "BA"}): A ${sum(ra.p).toFixed(3)}, B ${sum(rb.p).toFixed(3)} GPU ms per 233.75M evens; ` +
    `slice wall A ${wa.w.toFixed(3)} (setup ${wa.prep.toFixed(0)} ms, ${wa.perSeg.toFixed(3)} ms/seg), B ${wb.w.toFixed(3)} (setup ${wb.prep.toFixed(0)} ms, ${wb.perSeg.toFixed(3)} ms/seg); ` +
    `${identical ? "identical" : "RESULTS DIFFER"}`);
}
await writeFile(out, JSON.stringify({ mode, start: s0, limit: l0, gpu: adapter.info.description, runs }, null, 1));
