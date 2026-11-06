// Fingerprint of the exact run result (sha256 of the canonical JSON of finish()),
// to check that an optimization leaves every statistic bit-identical.
//   node test/golden.mjs START LIMIT [SEGMENT] [lite]
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";
import { initPrimes } from "./primes.mjs";
const [s0, l0, sg, mode] = process.argv.slice(2);
const lite = mode === "lite";
const M = await loadCore();
await initPrimes(M, lite);                 // GB_PRIMES_CACHE: generated once, checked, then loaded
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredLimits: { maxStorageBufferBindingSize: L.maxStorageBufferBindingSize,
  maxBufferSize: L.maxBufferSize, maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"), check: await rd("check.wgsl"),
  count30: await rd("count30.wgsl"), largeLite: await rd("large_lite.wgsl"), harvestLite: await rd("harvest_lite.wgsl") };
const listMB = Number(process.env["GB_LIST_MB"] ?? (lite ? 96 : 768));
// GB_CFG: JSON engine-config overrides (goldbach-tools crosscheck cpu --fuzz: results must not depend on them)
const extra = JSON.parse(process.env["GB_CFG"] ?? "{}");
const eng = new DenseEngine(M, device, src, { ...(lite ? { K: 16, listBytes: listMB << 20, lite: true } : { K: 32, listBytes: listMB << 20 }), ...extra });
await eng.prepare(BigInt(s0), BigInt(l0), BigInt(sg ?? "467500000"));
const t0 = performance.now();
await eng.runAll();
const s = (performance.now() - t0) / 1000;
const r = eng.finish();
const { pending: _p, leftovers: _l, ...canon } = r;   // configuration-dependent (not in the server hash)
const json = JSON.stringify(canon, (_, v) => (typeof v === "bigint" ? v.toString() : v));
const h = Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(json))), (b) => b.toString(16).padStart(2, "0")).join("");
console.log(`${h.slice(0, 16)} ${s0} ${l0} seg=${sg ?? "467500000"}${lite ? " lite" : ""} evens=${r.evens} primes=${r.primes} tail=${r.tailEvens} pmax=${r.pmax} ridges=${r.ridges.length} pending=${r.pending} left=${r.leftovers} ${(Number(r.evens) / s / 1e9).toFixed(2)} G/s`);
