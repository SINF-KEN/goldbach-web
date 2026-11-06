// Prime gaps on WebGPU (count30.wgsl's walk) vs the core's CPU walk (core.cpp
// gb_gap_walk, the CPU verifier's code) on the same segment maps: first / last
// prime, the histogram, every gap >= 700 with its position, the largest gap
// and its first position. The maps themselves are map_test.mjs's business.
//   node test/gaps_test.mjs START LIMIT [SEGMENT]      env: LITE=1 phone mode
import "./gpu.mjs";
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";

const [a0, a1, a2] = process.argv.slice(2);
const start = BigInt(a0 ?? "1425172824437600000"), limit = BigInt(a1 ?? "1425172824439600000");
const segment = BigInt(a2 ?? "20000000");
const LITE = process.env["LITE"] === "1";
const M = await loadCore();
if (!(LITE ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error(M.UTF8ToString(M._gb_error()));
const adapter = await navigator.gpu.requestAdapter();
const device = await adapter.requestDevice({ requiredLimits: {
  maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize, maxBufferSize: adapter.limits.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(10, adapter.limits.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: adapter.limits.maxComputeWorkgroupStorageSize } });
let gpuErr = "";
device.addEventListener?.("uncapturederror", (e) => { gpuErr = e.error?.message; console.error("GPU error:", gpuErr); });
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"),
              largeLite: await rd("large_lite.wgsl"), harvestLite: await rd("harvest_lite.wgsl"), check: await rd("check.wgsl"),
              count30: await rd("count30.wgsl") };
const eng = new DenseEngine(M, device, src, { lite: LITE });
await eng.prepare(start, limit, segment);
const HB = 1024, TC = 1024, GW = 4 + HB + 2 * TC;
const words = 8 * eng.cw32;
const rb = device.createBuffer({ size: (words + GW + 2 * eng.gapGroups) * 4, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
let bad = 0, gapsSeen = 0, tails = 0;
for (let k = 0; k < eng.geo.segs.length; k++) {
  const seg = eng.geo.segs[k];
  const g = eng.geo30(seg);
  const lp = M._gb_malloc(128);
  if (!M._gb_count30_limits(seg.segStart, seg.segEnd, seg.qFirst, seg.qOdds, lp)) throw new Error("limits");
  const lim = new Uint32Array(M.HEAPU32.buffer, lp, 32).slice();
  M._gb_free(lp);
  const enc = device.createCommandEncoder();
  eng.encodeMap(enc, k);
  device.queue.writeBuffer(eng.bLim, 0, lim);
  device.queue.writeBuffer(eng.bGP, 0, eng.gapParams(k, g, lim));
  enc.clearBuffer(eng.bGap); enc.clearBuffer(eng.bCnt);
  const pass = enc.beginComputePass();
  pass.setPipeline(eng.pCnt); pass.setBindGroup(0, eng.bgCnt);
  DenseEngine.dispatch2(pass, eng.gapGroups);
  pass.end();
  enc.copyBufferToBuffer(eng.bMap, 0, rb, 0, words * 4);
  enc.copyBufferToBuffer(eng.bGap, 0, rb, words * 4, GW * 4);
  enc.copyBufferToBuffer(eng.bGR, 0, rb, (words + GW) * 4, 2 * eng.gapGroups * 4);
  device.queue.submit([enc.finish()]);
  await rb.mapAsync(GPUMapMode.READ);
  const all = new Uint32Array(rb.getMappedRange()).slice();
  rb.unmap();
  const map = all.subarray(0, words), G = all.subarray(words, words + GW), Rw = all.subarray(words + GW);
  // CPU walk of the same map
  const mp = M._gb_malloc(words * 4), op = M._gb_malloc((5 + HB + 2 * 65536) * 4);
  new Uint32Array(M.HEAPU32.buffer, mp, words).set(map);
  const minUpper = eng.gapParams(k, g, lim)[4];
  if (!M._gb_gap_walk(mp, eng.cw32, seg.segStart, seg.segEnd, seg.qFirst, seg.qOdds, minUpper, 65536, op)) throw new Error("cpu walk");
  const C = new Uint32Array(M.HEAPU32.buffer, op, 5 + HB + 2 * 65536).slice();
  M._gb_free(mp); M._gb_free(op);
  let gmax = 0, gpos = 0;
  for (let i = 0; i < Rw.length; i += 2) if (Rw[i] > gmax || (Rw[i] === gmax && Rw[i] && Rw[i + 1] < gpos)) { gmax = Rw[i]; gpos = Rw[i + 1]; }
  const first = G[0] ? ~G[0] >>> 0 : 0xFFFFFFFF, last = G[1] ? G[1] - 1 : 0;
  const gt = new Set(), ct = new Set();
  for (let i = 0; i < G[3]; i++) gt.add(`${G[4 + HB + 2 * i]}:${G[5 + HB + 2 * i]}`);
  for (let i = 0; i < C[4]; i++) ct.add(`${C[5 + HB + 2 * i]}:${C[6 + HB + 2 * i]}`);
  let hdiff = 0, n = 0;
  for (let i = 0; i < HB; i++) { if (G[4 + i] !== C[5 + i]) hdiff++; n += C[5 + i]; }
  const same = !G[2] && first === C[0] && last === C[1] && !hdiff && gmax === C[2] && gpos === C[3] &&
               gt.size === ct.size && [...gt].every((x) => ct.has(x));
  gapsSeen += n; tails += ct.size;
  console.log(`seg ${k}: ${same ? "identical" : "DIFFERS"} gaps ${n} max ${C[2]} tail ${ct.size}` +
              (same ? "" : ` | gpu err ${G[2]} first ${first}/${C[0]} last ${last}/${C[1]} hist ${hdiff} max ${gmax}@${gpos}/${C[2]}@${C[3]} tail ${gt.size}/${ct.size}`));
  if (!same) bad++;
}
if (gpuErr) bad++;
console.log(bad ? `FAIL: ${bad} segment(s)` : `ALL SEGMENTS IDENTICAL (${gapsSeen} gaps, ${tails} >= 700)`);
process.exit(bad ? 1 : 0);
