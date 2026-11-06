// M3 test: complete per-segment maps (tile + packed + hit lists) of a WebGPU
// run vs the exact CPU reference (native core via FFI, all base primes).
//   node test/map_test.mjs START LIMIT [SEGMENT] [K] [adapter] [check-every]
// env: LITE=1 phone mode; CFG = JSON engine config overrides; DUMP=n prints the
//      first n differing bits as q values (factor them to see which sieve missed).
// Note: on AMD RDNA2 with Mesa RADV, maps differ at random (a driver bug, see
// a Mesa RADV driver bug); the engine is exact on Intel, lavapipe, Chromium/Dawn on Intel.
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";

const start = BigInt(process.argv.slice(2)[0] ?? "4000000000000000000");
const limit = BigInt(process.argv.slice(2)[1] ?? "4000000002337500000");    // 5 x 467.5M
const segment = BigInt(process.argv.slice(2)[2] ?? "467500000");
const K = Number(process.argv.slice(2)[3] ?? "2");
const pref = process.argv.slice(2)[4] ?? "high-performance";
const every = Number(process.argv.slice(2)[5] ?? "1");

const M = await loadCore();
const LITE = process.env["LITE"] === "1";
if (!(LITE ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error(M.UTF8ToString(M._gb_error()));
// the reference: an independent class map (gb_ref_class_map: every base prime
// sieved on the CPU) from a second core instance with the full u32 primes
const R = await loadCore();
if (!R._gb_init(5000000000000000000n)) throw new Error("reference core init");

const adapter = await navigator.gpu.requestAdapter({ powerPreference: pref });
if (!adapter) throw new Error("no adapter");
const device = await adapter.requestDevice({ requiredLimits: {
  maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize,
  maxBufferSize: adapter.limits.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(10, adapter.limits.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: adapter.limits.maxComputeWorkgroupStorageSize } });
device.addEventListener?.("uncapturederror", (e) => console.error("GPU error:", e.error?.message));
const src = {
  tile: await readFile(new URL("../kernels/tile.wgsl", import.meta.url), "utf8"),
  large: await readFile(process.env["LARGE_WGSL"] ?? new URL("../kernels/large.wgsl", import.meta.url), "utf8"),
  harvest: await readFile(new URL("../kernels/harvest.wgsl", import.meta.url), "utf8"),
  largeLite: await readFile(new URL("../kernels/large_lite.wgsl", import.meta.url), "utf8"),
  check: await readFile(new URL("../kernels/check.wgsl", import.meta.url), "utf8"),
  count30: await readFile(new URL("../kernels/count30.wgsl", import.meta.url), "utf8"),
  harvestLite: await readFile(new URL("../kernels/harvest_lite.wgsl", import.meta.url), "utf8"),
};
const eng = new DenseEngine(M, device, src, { K, lite: LITE, ...JSON.parse(process.env["CFG"] ?? "{}") });
let t0 = performance.now();
await eng.prepare(start, limit, segment);
console.log(`${adapter.info.description}: ${eng.geo.segs.length} segments, split ${eng.split}, packed [${eng.pa}, ${eng.pb}) one-hit from ${eng.pone}, hit lists [${eng.ha}, ${eng.hb}) K ${eng.K} G ${eng.G} capg ${eng.capg}; prepare ${((performance.now() - t0) / 1000).toFixed(1)} s`);

const words = 8 * eng.cw32;
const rb = device.createBuffer({ size: words * 4, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
let refP = 0;
let ref = new Uint32Array(0);
let bad = 0;
for (let k = 0; k < eng.geo.segs.length; k++) {
  const seg = eng.geo.segs[k];
  const enc = device.createCommandEncoder();
  eng.encodeMap(enc, k);
  const check = k % every === 0 || k === eng.geo.segs.length - 1;
  if (check) enc.copyBufferToBuffer(eng.bMap, 0, rb, 0, words * 4);
  t0 = performance.now();
  device.queue.submit([enc.finish()]);
  if (!check) continue;
  await rb.mapAsync(GPUMapMode.READ);
  const gpuMs = performance.now() - t0;
  const gpu = new Uint32Array(rb.getMappedRange()).slice();
  rb.unmap();
  if (!refP) refP = R._gb_malloc(words * 4);
  if (!R._gb_ref_class_map(seg.qFirst, seg.qOdds, refP, eng.cw32)) throw new Error("reference map: " + R.UTF8ToString(R._gb_error()));
  ref = new Uint32Array(R.HEAPU32.buffer, refP, words);
  let diff = 0, first = -1;
  for (let i = 0; i < words; i++) if (gpu[i] !== ref[i]) { if (first < 0) first = i; diff++; }
  if (diff && process.env["DUMP"]) {                 // DUMP=n: the first n differing bits as q values
    const g = eng.geo30(seg);
    let shown = 0;
    for (let i = 0; i < words && shown < Number(process.env["DUMP"]); i++) {
      const x = gpu[i] ^ ref[i];
      for (let b = 0; b < 32 && x; b++) if ((x >>> b) & 1) {
        const c = Math.floor(i / eng.cw32), t = (i - c * eng.cw32) * 32 + b;
        console.log(`  class ${c} t ${t}: gpu ${(gpu[i] >>> b) & 1} ref ${(ref[i] >>> b) & 1} q ${seg.qFirst + 2n * BigInt(g.phi[c]) + 30n * BigInt(t)}`);
        if (++shown >= Number(process.env["DUMP"])) break;
      }
    }
  }
  const err = await eng.errors();
  console.log(`seg ${k} [${seg.segStart}, ${seg.segEnd}] q_odds ${seg.qOdds}: ${diff ? `DIFFERS in ${diff} words (first ${first}, class ${Math.floor(first / eng.cw32)})` : "identical"}, err ${err}, ${gpuMs.toFixed(1)} ms`);
  if (diff || err) bad++;
}
console.log(bad ? `FAIL: ${bad} segment(s)` : "ALL SEGMENTS IDENTICAL");
process.exit(bad ? 1 : 0);
