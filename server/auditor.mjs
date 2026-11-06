// goldbach-web trusted auditor: recomputes browser-verified slices on this
// machine and reports them with the admin token. A slice whose accepted result
// differs from the audit is reopened and the devices that confirmed it are banned.
//
//   node server/auditor.mjs
// env: GOLDBACH_URL (default https://goldbach.net01.io)
//      GOLDBACH_TOKEN_FILE (default ~/.config/goldbach/token)
//      AUDIT_GPU = low-power | high-performance  (default low-power: the iGPU, not the B580)
//      AUDIT_HOURS = "always" | "0-8" (local hours [from, to) when audits may run)
import "../test/gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "../test/core.mjs";
import { DenseEngine } from "../client/engine.js";
import { selfTest } from "../client/selftest.js";

const URL_ = process.env["GOLDBACH_URL"] ?? "https://goldbach.net01.io";
const tokenFile = process.env["GOLDBACH_TOKEN_FILE"] ?? `${process.env["HOME"]}/.config/goldbach/token`;
const token = (await readFile(tokenFile, "utf8")).trim();
const pref = process.env["AUDIT_GPU"] ?? "low-power";
const hours = process.env["AUDIT_HOURS"] ?? "always";
const log = (...a) => console.log(new Date().toISOString(), ...a);

function allowedNow() {
  if (hours === "always") return true;
  const [a, b] = hours.split("-").map(Number);
  const h = new Date().getHours();
  return a <= b ? h >= a && h < b : h >= a || h < b;
}
async function post(path, body) {
  const r = await fetch(URL_ + path, { method: "POST", headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body, (_, v) => (typeof v === "bigint" ? v.toString() : v)) });
  if (!r.ok) throw new Error(`${path}: HTTP ${r.status}`);
  return r.json();
}
const sleep = (s) => new Promise((r) => setTimeout(r, s * 1000));

const M = await loadCore();
const adapter = await navigator.gpu.requestAdapter({ powerPreference: pref });
if (!adapter) throw new Error("no WebGPU adapter");
const L = adapter.limits;
const lite = L.maxStorageBufferBindingSize < 640 * 2 ** 20;
if (!(lite ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error("core init");
const device = await adapter.requestDevice({ requiredLimits: {
  maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"),
  check: await rd("check.wgsl"), count30: await rd("count30.wgsl"),
  largeLite: await rd("large_lite.wgsl"), harvestLite: await rd("harvest_lite.wgsl") };
log(`auditor on ${adapter.info.description}${lite ? " (lite)" : ""}, hours ${hours}, server ${URL_}`);

// Known answer, checked before every audit: a wrong audit would reopen good
// slices and ban honest devices (client/selftest.js; Mesa RADV on some RDNA2
// iGPUs computes wrong maps, a Mesa RADV driver bug).
async function selfCheck() {
  const r = await selfTest(M, device, src, lite ? { K: 16, listBytes: 96 << 20, lite: true } : { K: 32, listBytes: 768 << 20 },
    lite ? 128000000n : 467500000n);
  if (!r.ok) throw new Error(`self-test FAILED on this device (${r.hash}): refusing to audit`);
}
await selfCheck();
log("self-test passed");

for (;;) {
  try {
    if (!allowedNow()) { await sleep(600); continue; }
    const job = await post("/api/audit/next", { token });
    if (job.wait) { await sleep(job.wait); continue; }
    await selfCheck();
    const t0 = performance.now();
    const eng = new DenseEngine(M, device, src, lite ? { K: 16, listBytes: 96 << 20, lite: true } : { K: 32, listBytes: 768 << 20 });
    let r;
    try {
      await eng.prepare(BigInt(job.start), BigInt(job.limit), lite ? 128000000n : 467500000n);
      await eng.runAll();
      r = eng.finish();
    } finally {
      eng.destroy();
    }
    const res = await post("/api/audit/result", { token, slice: job.slice, result: { ...r, pass: true } });
    log(`slice #${job.slice} [${job.start}, ${job.limit}] audited in ${((performance.now() - t0) / 1000).toFixed(0)} s: ${res.status}${res.banned ? `, ${res.banned} device(s) banned` : ""}`);
  } catch (e) {
    log("error:", e.message);
    if (/self-test FAILED/.test(e.message)) process.exit(2);
    await sleep(60);
  }
}
