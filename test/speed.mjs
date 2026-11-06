// Throughput of the pipelined engine for several in-flight depths.
//   node test/speed.mjs [START] [LIMIT] [SEGMENT] [depths...]   (env K: batch, default 32)
// test/prof.mjs gives the GPU time per pass (timestamp queries).
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";
const start = BigInt(process.argv.slice(2)[0] ?? "4000000000000000000");
const limit = BigInt(process.argv.slice(2)[1] ?? "4000000100000000000");
const segment = BigInt(process.argv.slice(2)[2] ?? "467500000");
const depths = process.argv.slice(2).length > 3 ? process.argv.slice(2).slice(3).map(Number) : [1, 2, 3];
const M = await loadCore(); M._gb_init(5000000000000000000n);
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredLimits: { maxStorageBufferBindingSize: L.maxStorageBufferBindingSize,
  maxBufferSize: L.maxBufferSize, maxStorageBuffersPerShaderStage: Math.min(10, L.maxStorageBuffersPerShaderStage), maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"), check: await rd("check.wgsl"), count30: await rd("count30.wgsl") };
let ref = "";
for (const depth of depths) {
  const eng = new DenseEngine(M, device, src, { K: Number(process.env["K"] ?? 32), listBytes: 768 << 20, depth });
  await eng.prepare(start, limit, segment);
  const t0 = performance.now();
  await eng.runAll();
  const s = (performance.now() - t0) / 1000;
  const r = eng.finish();
  const fp = `${r.primes}/${r.twins}/${r.tailEvens}/${r.pmax}`;
  if (!ref) ref = fp;
  console.log(`depth ${depth}: ${(Number(r.evens) / s / 1e9).toFixed(2)} G evens/s (${s.toFixed(2)} s, ${eng.geo.segs.length} segments) ${fp === ref ? "same result" : "RESULT DIFFERS " + fp}`);
  eng.destroy();
}
