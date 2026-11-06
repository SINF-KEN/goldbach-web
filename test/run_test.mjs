// M4 test: a WebGPU run vs a native goldbach_l0 run of the same window.
// Compares every exact statistic of the native GBSTATS log line: primes30,
// twins30, tail histogram (p >= 3000), first occurrences, pmax, beyond.
//   node test/run_test.mjs START LIMIT [SEGMENT] [K] [adapter]
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadCore } from "./core.mjs";
import { DenseEngine } from "../client/engine.js";

const start = BigInt(process.argv.slice(2)[0] ?? "4000000000000000000");
const limit = BigInt(process.argv.slice(2)[1] ?? "4000000020000000000");
const segment = BigInt(process.argv.slice(2)[2] ?? "467500000");
const K = Number(process.argv.slice(2)[3] ?? "64");
const pref = process.argv.slice(2)[4] ?? "high-performance";
// native goldbach_l0 checkout to compare against (GOLDBACH_L0_DIR, default ../goldbach-l0)
const root = (process.env["GOLDBACH_L0_DIR"] ?? new URL("../../goldbach-l0/", import.meta.url).pathname).replace(/\/?$/, "/");

// ---- native run, isolated statistics files ----
const tmp = await mkdtemp(join(tmpdir(), "gbweb-"));
const env = { GB_STATS_STATE: `${tmp}/s.state`, GB_STATS_LOG: `${tmp}/s.log`, GB_RIDGE_FILE: `${tmp}/r.csv`,
              GB_RECORDS_FILE: `${tmp}/rec.txt`, GB_STATS_LIVE: `${tmp}/live.json`, GB_RECORD_HOOK: "" };
// isolated statistics files: env adds to (and overrides) the inherited environment
const nat = spawnSync(`${root}build/goldbach_l0`, ["--start", String(start), "--limit", String(limit), "--csv"],
  { cwd: root, env: { ...process.env, ...env }, encoding: "utf8", maxBuffer: 1 << 26 });
const csv = nat.stdout ?? "";
if (nat.status !== 0 || !csv.includes("result=PASS")) throw new Error("native run failed: " + nat.stderr);
const kv = (line) => Object.fromEntries(line.trim().split(/\s+/).filter((x) => x.includes("=")).map((x) => x.split("=")));
const nc = kv(csv);
const nlog = kv((await readFile(`${tmp}/s.log`, "utf8")).trim().split("\n").pop());
await rm(tmp, { recursive: true, force: true });
console.log(`native: ${nc.gpu_s} s gpu, primes ${nc.primes} twins ${nc.twins} tail ${nc.stats_tail} pmax ${nc.stats_pmax}`);

// ---- web run ----
const M = await loadCore();
const LITE = process.env["LITE"] === "1";
if (!(LITE ? M._gb_init_lite(5000000000000000000n) : M._gb_init(5000000000000000000n))) throw new Error(M.UTF8ToString(M._gb_error()));
const adapter = await navigator.gpu.requestAdapter({ powerPreference: pref });
if (!adapter) throw new Error("no adapter");
const device = await adapter.requestDevice({ requiredLimits: {
  maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize, maxBufferSize: adapter.limits.maxBufferSize,
  maxStorageBuffersPerShaderStage: Math.min(10, adapter.limits.maxStorageBuffersPerShaderStage) } });
const rd = (f) => readFile(new URL(`../kernels/${f}`, import.meta.url), "utf8");
const src = { tile: await rd("tile.wgsl"), large: await rd("large.wgsl"), harvest: await rd("harvest.wgsl"),
              check: process.env["CHECK_WGSL"] ? await readFile(process.env["CHECK_WGSL"], "utf8") : await rd("check.wgsl"), count30: await rd("count30.wgsl"),
              largeLite: await rd("large_lite.wgsl"), harvestLite: await rd("harvest_lite.wgsl") };
const eng = new DenseEngine(M, device, src, { K, lite: LITE });
await eng.prepare(start, limit, segment);
let t0 = performance.now();
for (let k = 0; k < eng.geo.segs.length; k++) await eng.runSegment(k);
const r = eng.finish();
const webS = (performance.now() - t0) / 1000;
console.log(`web (${adapter.info.description}): ${eng.geo.segs.length} segments in ${webS.toFixed(2)} s, KA ${eng.KA}, pending ${r.pending}, leftovers ${r.leftovers}`);

// ---- compare ----
let bad = 0;
const cmp = (name, a, b) => {
  const ok = a === b;
  if (!ok) bad++;
  console.log(`${ok ? "  ok  " : "  DIFF"} ${name}: native ${a.length > 90 ? a.slice(0, 90) + "..." : a}${ok ? "" : `  web ${b.length > 90 ? b.slice(0, 90) + "..." : b}`}`);
};
cmp("evens", nlog.evens, String(r.evens));
cmp("primes", nlog.primes, String(r.primes));
cmp("twins", nlog.twins, String(r.twins));
cmp("primes30", nlog.primes30, r.primes30.join(","));
cmp("twins30", nlog.twins30, r.twins30.join(","));
cmp("tail_evens", nlog.tail_evens, String(r.tailEvens));
cmp("pmax", nlog.pmax, String(r.pmax));
cmp("first", nlog.new_first, r.first.length ? r.first.map(([p, n]) => `${p}:${n}`).join(";") : "-");
cmp("beyond", nlog.beyond, r.beyond.length ? r.beyond.map((b) => `${b.p}:${b.n}`).join(";") : "-");
// tail: native "P0:c,c,,c" over consecutive odd primes >= tail_t
const odd = [];
for (let p = 3; p <= 9781; p += 2) { let q = true; for (let d = 3; d * d <= p; d += 2) if (p % d === 0) { q = false; break; } if (q) odd.push(p); }
const [p0s, dense] = nlog.tail.split(":");
const t0i = odd.indexOf(Number(p0s));
const natTail = dense.split(",").map((x, i) => [odd[t0i + i], x ? Number(x) : 0]).filter((x) => x[1]).map((x) => x.join(":")).join(";");
const webTail = r.histByP.filter(([p]) => p >= r.tailT).map(([p, v]) => `${p}:${v}`).join(";");
cmp("tail histogram", natTail, webTail);
const hsum = r.histByP.reduce((a, [, v]) => a + v, 0);
cmp("histogram covers every even", String(r.evens), String(hsum));
console.log(bad ? `FAIL: ${bad} difference(s)` : "WEB RUN IDENTICAL TO NATIVE");
process.exit(bad ? 1 : 0);
