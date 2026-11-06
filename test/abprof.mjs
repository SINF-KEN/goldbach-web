// A/B kernel profile in one process: engines A and B alternate segment by
// segment on the same device (GPU clock drift hits both alike), per-pass GPU
// time from timestamp queries, and both results must be identical.
//   node test/abprof.mjs START LIMIT [SEGMENT]
// env: A_DIR / B_DIR = kernel directories (default: git HEAD's kernels -> A,
//      the working tree -> B); LITE=1 phone mode; CFG_A / CFG_B = JSON config;
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";
const [s0, l0, sg] = process.argv.slice(2);
const lite = process.env["LITE"] === "1";
const M = await loadCore();
if (!(lite ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error("init");
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredFeatures: ["timestamp-query"], requiredLimits: {
  maxStorageBufferBindingSize: L.maxStorageBufferBindingSize, maxBufferSize: L.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage), maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
// gaps30: engines before the gap walk moved into count30 (2.5) still need it
const names = ["tile", "large", "harvest", "check", "count30", "gaps30", "large_lite", "harvest_lite"];
async function kernels(dir) {
  const src = {};
  for (const n of names) {
    const k = n === "large_lite" ? "largeLite" : n === "harvest_lite" ? "harvestLite" : n;
    try {
      src[k] = dir ? await readFile(`${dir}/${n}.wgsl`, "utf8")
        : execFileSync("git", ["show", `HEAD:kernels/${n}.wgsl`], { cwd: fileURLToPath(new URL("..", import.meta.url)), encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
    } catch (e) { if (n !== "gaps30") throw e; }
  }
  return src;
}
const here = new URL("../kernels", import.meta.url).pathname;
const srcA = await kernels(process.env["A_DIR"]);
const srcB = await kernels(process.env["B_DIR"] ?? here);
const base = lite ? { K: 16, listBytes: 96 << 20, lite: true } : { K: 32, listBytes: 768 << 20 };
const mk = async (src, cfg) => {
  const e = new DenseEngine(M, device, src, { ...base, profile: true, ...JSON.parse(cfg ?? "{}") });
  await e.prepare(BigInt(s0 ?? "4000000000000000000"), BigInt(l0 ?? "4000000020000000000"), BigInt(sg ?? (lite ? "128000000" : "467500000")));
  return e;
};
const A = await mk(srcA, process.env["CFG_A"]), B = await mk(srcB, process.env["CFG_B"]);
const n = A.geo.segs.length;
for (let k = 0; k < n; k++) { await A.runSegment(k); await B.runSegment(k); }
const ra = A.finish(), rb = B.finish();
const fp = (r) => JSON.stringify(r, (_, v) => (typeof v === "bigint" ? v.toString() : v)).replace(/"(pending|leftovers)":\d+,?/g, "");
const pass = Object.keys(B.prof).filter((k) => k !== "host" && k !== "wall");
let ta = 0, tb = 0;
const rows = pass.map((k) => {
  const a = (A.prof[k] ?? 0) / n, b = (B.prof[k] ?? 0) / n;
  ta += a; tb += b;
  return `${k}=${a.toFixed(3)}->${b.toFixed(3)}`;
});
console.log(`${lite ? "lite" : "full"} ${n} segments: A ${ta.toFixed(3)} ms/seg, B ${tb.toFixed(3)} ms/seg (${((ta / tb - 1) * 100).toFixed(1)}% faster)  ${rows.join(" ")}  results ${fp(ra) === fp(rb) ? "IDENTICAL" : "DIFFER"}`);
