// Reproduces the worker's engine configuration on one slice segment with GPU error reporting.
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";
const M = await loadCore(); M._gb_init(5000000000000000000n);
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredLimits: {
  maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(10, L.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
device.onuncapturederror = (e) => console.log("GPU ERROR:", e.error.message.slice(0, 400));
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"), check: await rd("check.wgsl"), count30: await rd("count30.wgsl") };
for (const cfg of [{ K: 64 }, { K: 32, listBytes: 768 << 20 }]) {
  const eng = new DenseEngine(M, device, src, cfg);
  await eng.prepare(4999999000000000002n, 5000000000000000000n, 467500000n);
  try { await eng.runSegment(0); console.log(JSON.stringify(cfg), "OK K", eng.K); } catch (e) { console.log(JSON.stringify(cfg), "FAIL", e.message, "K", eng.K); }
  eng.destroy();
}
