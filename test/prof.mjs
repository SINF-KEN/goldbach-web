// Profile of the web engine: per-pass GPU ms per segment (timestamp queries), host encode, wall.
//   node test/prof.mjs START LIMIT [SEGMENT]
//   env: CFG = JSON engine config overrides; SRC_<kernel> = alternative kernel file;
//        LITE=1 phone mode; GPU=low-power for the integrated GPU
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";
const [s0, l0, sg] = process.argv.slice(2);
const M = await loadCore();
const lite = process.env["LITE"] === "1";
if (!(lite ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error("init");
const adapter = await navigator.gpu.requestAdapter({ powerPreference: process.env["GPU"] ?? "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredFeatures: ["timestamp-query"], requiredLimits: {
  maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage), maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const rd = (f) => readFile(process.env[`SRC_${f.split(".")[0]}`] ?? new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"), check: await rd("check.wgsl"), count30: await rd("count30.wgsl"),
  largeLite: await rd("large_lite.wgsl"), harvestLite: await rd("harvest_lite.wgsl") };
const eng = new DenseEngine(M, device, src, { ...(lite ? { K: 16, listBytes: 96 << 20, lite: true } : { K: 32, listBytes: 768 << 20 }), profile: true, ...JSON.parse(process.env["CFG"] ?? "{}") });
await eng.prepare(BigInt(s0 ?? "4000000000000000000"), BigInt(l0 ?? "4000000020000000000"), BigInt(sg ?? (lite ? "128000000" : "467500000")));
const n = eng.geo.segs.length;
for (let k = 0; k < n; k++) await eng.runSegment(k);
const r = eng.finish();
let gpu = 0;
const rows = [];
for (const [k, v] of Object.entries(eng.prof)) {
  const ms = v / n;
  if (k !== "host" && k !== "wall") gpu += ms;
  rows.push(`${k}=${ms.toFixed(3)}`);
}
console.log(`gpu ${gpu.toFixed(3)} ms/seg (${(Number(r.evens) / n / gpu / 1e6).toFixed(2)} G evens/s GPU-bound)  ${rows.join(" ")}  fp=${r.primes}/${r.tailEvens}/${r.pmax}/${r.histByP.slice(0, 2).map((x) => x[1]).join(",")}`);
