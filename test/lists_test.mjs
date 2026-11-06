// The checker's ridge and leftover lists (engine.js ioLists / readLists): a
// segment's lists live in their own region of the io buffer while it is in
// flight, and collect() fetches only their used part. Ridges (p(n) >= 7000)
// are too rare for a test window, so the test lowers the kernel's report floor
// (cfg.ridgeFloor) to the tail bound: every even with p(n) >= 3000 is then
// reported with its n, many segments in flight at once. Checks:
//   * the result equals the default run's (the host keeps p >= 7000 only),
//     with pipelining (default batch and depth) and one segment at a time;
//   * the reported pairs are exactly the tail evens (count = tailEvens) and
//     each one's p(n) is confirmed by the CPU (WASM, deterministic Miller-Rabin);
//   * a ridge list too small for them fails closed (device error flag).
//   node test/lists_test.mjs [START LIMIT SEGMENT] [lite]   (CI: lavapipe, phone mode)
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { DenseEngine, engineDefaults } from "../client/engine.js";
import { initPrimes } from "./primes.mjs";

const args = process.argv.slice(2);
const lite = args.includes("lite");
const [s0, l0, sg] = args.filter((a) => a !== "lite");
const start = BigInt(s0 ?? "4999999998000000000"), limit = BigInt(l0 ?? "5000000000000000000"), seg = BigInt(sg ?? "64000000");
const M = await loadCore();
await initPrimes(M, lite);
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const L = adapter.limits;
const device = await adapter.requestDevice({ requiredLimits: { maxStorageBufferBindingSize: L.maxStorageBufferBindingSize,
  maxBufferSize: L.maxBufferSize, maxStorageBuffersPerShaderStage: Math.min(8, L.maxStorageBuffersPerShaderStage),
  maxComputeWorkgroupStorageSize: L.maxComputeWorkgroupStorageSize } });
const names = { tile: "tile", large: "large", harvest: "harvest", check: "check", count30: "count30",
  largeLite: "large_lite", harvestLite: "harvest_lite" };
const src = {};
for (const [k, f] of Object.entries(names)) src[k] = await readFile(new URL(`../kernels/${f}.wgsl`, import.meta.url), "utf8");
const base = engineDefaults(lite).cfg;
const canon = (r) => JSON.stringify(r, (_, v) => (typeof v === "bigint" ? v.toString() : v));
let failures = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) failures++; };

/// Runs the window; returns the canonical result, the run totals and every
/// ridge pair the GPU reported (as [n, p]).
async function run(cfg, oneByOne = false) {
  const eng = new DenseEngine(M, device, src, { ...base, ...cfg });
  await eng.prepare(start, limit, seg);
  const pairs = [], left = [];
  const e = eng;
  const readLists = e.readLists.bind(eng);
  e.readLists = async (k, nr, nl) => {
    const lists = await readLists(k, nr, nl);
    const segStart = e.geo.segs[k].segStart;
    for (let i = 0; i < lists.ridge.length; i += 2)
      pairs.push([segStart + 2n * BigInt(lists.ridge[i]), e.cand[lists.ridge[i + 1] >>> 16].p[lists.ridge[i + 1] & 0xFFFF]]);
    for (const x of lists.left) left.push(segStart + 2n * BigInt(x));
    return lists;
  };
  try {
    if (oneByOne) for (let k = 0; k < e.geo.segs.length; k++) await eng.runSegment(k);
    else await eng.runAll();
    const r = eng.finish();
    const { pending: _p, leftovers: _l, ...c } = r;
    return { canon: canon(c), r, pairs, left };
  } finally { eng.destroy(); }
}

const ref = await run({});
console.log(`window [${start}, ${limit}], segment ${seg}${lite ? ", phone mode" : ""}: ${ref.r.tailEvens} evens with p(n) >= ${ref.r.tailT}, ${ref.r.leftovers} leftovers`);
check(ref.r.tailEvens > 0, "the window has tail evens (the test exercises the lists)");
for (const [what, cfg, one] of [["pipelined", {}, false], ["one segment at a time, depth 1", { batch: 1, depth: 1 }, true]]) {
  const low = await run({ ...cfg, ridgeFloor: ref.r.tailT }, one);
  check(low.canon === ref.canon, `${what}: result identical to the default floor's`);
  // tail evens settled on the CPU (leftovers) are not GPU ridge pairs
  const cpuTail = low.left.filter((n) => Number(M._gb_pn_exact(n)) >= ref.r.tailT).length;
  check(low.left.length === ref.r.leftovers, `${what}: ${low.left.length} leftovers read = the run's count`);
  check(low.pairs.length === ref.r.tailEvens - cpuTail, `${what}: ${low.pairs.length} pairs reported = tail evens found on the GPU`);
  check(new Set(low.pairs.map(([n]) => n)).size === low.pairs.length, `${what}: no even reported twice`);
  const bad = low.pairs.filter(([n, p]) => Number(M._gb_pn_exact(n)) !== p);
  check(bad.length === 0, `${what}: every reported p(n) confirmed by the CPU${bad.length ? ` (first wrong: n=${bad[0][0]})` : ""}`);
}
// fail closed: room for 1 pair while every even with p(n) >= 3000 is reported
let msg = "";
try { await run({ ridgeFloor: ref.r.tailT, ridgeCap: 1 }); } catch (err) { msg = String(err); }
check(/device error flags/.test(msg), `ridge list overflow fails closed (${msg.split("\n")[0] || "no error"})`);
if (failures) { console.log(`LISTS FAILED (${failures})`); process.exit(1); }
console.log("LISTS OK");
