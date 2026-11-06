// The per-run states on the GPU (kernels/harvest.wgsl, entry point states) vs
// the WASM core's definition (core.cpp gb_packed_state, gb_harvest_state),
// written here once more in BigInt:
//   packed: t / 2 for the smallest t >= 0 with q_first + t an odd multiple of p;
//   hit list: X = (max(p^2, first odd multiple >= q_first) - q_first) / 2 as
//             (X / delta, X mod delta), or (j_far, 0) when X / delta >= j_far.
// 1. the BigInt reference equals the core on real prime ranges (both ends of
//    the hit-list primes, the packed primes), for several q_first;
// 2. the kernel equals the reference on random and edge inputs: q_first at the
//    ends of the range and around p^2, r = 0, p near 2^32 and 36000, deltas
//    from 1 (parking) to 2^31 - 1.
//   node test/states_test.mjs [cases per configuration]   (CI: lavapipe)
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";
import { initPrimes } from "./primes.mjs";

const N = Number(process.argv[2] ?? 1 << 14);
const J_FAR = 0xF0000000;
let failures = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) failures++; };

const packedRef = (q, p) => {
  const r = q % p;
  let t = r ? p - r : 0n;
  if (t & 1n) t += p;
  return Number(t >> 1n);
};
const hitRef = (q, p, delta) => {
  let first = p * p;
  if (first < q) {
    const r = q % p;
    first = q + (r ? p - r : 0n);
    if ((first & 1n) === 0n) first += p;
  }
  const X = (first - q) >> 1n;
  const j = X / delta;
  return j >= BigInt(J_FAR) ? [J_FAR, 0] : [Number(j), Number(X % delta)];
};

// ---- 1. the reference is the core's definition ----
const M = await loadCore();
await initPrimes(M, false);
const np = M._gb_prime_count() >>> 0;
const primes = new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), np).slice();
const pIdx = (x) => M._gb_prime_index(x) >>> 0;
const tmp = M._gb_malloc(2 * 4096 * 4);
for (const q of [4000000000000000001n, 4123456789012345679n, 4999999999999999999n]) {
  const delta = 233750000;
  const ranges = [["packed", pIdx(60000), pIdx(60000) + 4096], ["hit list, first", pIdx(233750000) + 1, pIdx(233750000) + 4097],
    ["hit list, p^2 >= q_first", np - 4096, np]];
  for (const [what, a, b] of ranges) {
    const packed = what === "packed";
    if (!(packed ? M._gb_packed_state(q, a, b, tmp) : M._gb_harvest_state(q, delta, a, b, tmp))) throw new Error(M.UTF8ToString(M._gb_error()));
    const core = new Uint32Array(M.HEAPU32.buffer, tmp, (packed ? 1 : 2) * (b - a));
    let bad = 0;
    for (let i = a; i < b; i++) {
      const p = BigInt(primes[i]);
      if (packed) { if (core[i - a] !== packedRef(q, p)) bad++; }
      else { const [j, off] = hitRef(q, p, BigInt(delta)); if (core[2 * (i - a)] !== j || core[2 * (i - a) + 1] !== off) bad++; }
    }
    check(bad === 0, `reference = core: ${what}, q_first ${q} (${b - a} primes)`);
  }
}
M._gb_free(tmp);

// ---- 2. the kernel equals the reference ----
const src = await readFile(new URL("../kernels/harvest.wgsl", import.meta.url), "utf8");
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const device = await adapter.requestDevice();
const mod = device.createShaderModule({ code: src });
const info = await mod.getCompilationInfo();
if (info.messages.some((m) => m.type === "error")) throw new Error(info.messages.map((m) => `${m.lineNum}: ${m.message}`).join("\n"));
const pipe = device.createComputePipeline({ layout: "auto", compute: { module: mod, entryPoint: "states" } });
const rnd = (lo, hi) => lo + Math.floor(Math.random() * (hi - lo));   // [lo, hi)
const rndBig = (lo, hi) => lo + BigInt(Math.floor(Math.random() * Number(hi - lo)));
const odd = (x) => (x | 1) >>> 0;

/// Runs the kernel on primes ps (the first npk packed) for one q_first and delta.
async function gpu(q, delta, ps, npk) {
  const S = device.createBuffer({ size: 32, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
  device.queue.writeBuffer(S, 0, new Uint32Array([Number(q >> 32n), Number(q & 0xFFFFFFFFn), delta, 0, npk, ps.length, J_FAR, 0]));
  const bp = device.createBuffer({ size: ps.byteLength, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
  device.queue.writeBuffer(bp, 0, ps);
  const nh = ps.length - npk;
  const bh = device.createBuffer({ size: Math.max(16, 8 * nh), usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
  const bk = device.createBuffer({ size: Math.max(16, 4 * npk), usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
  const rb = device.createBuffer({ size: 8 * nh + 4 * npk, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
  const bg = device.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: [[0, bp], [1, bh], [7, bk], [8, S]]
    .map(([i, b]) => ({ binding: i, resource: { buffer: b } })) });
  const enc = device.createCommandEncoder();
  const pass = enc.beginComputePass(); pass.setPipeline(pipe); pass.setBindGroup(0, bg);
  const groups = Math.ceil(ps.length / 256); pass.dispatchWorkgroups(Math.min(groups, 65535), Math.ceil(groups / 65535)); pass.end();
  if (nh) enc.copyBufferToBuffer(bh, 0, rb, 0, 8 * nh);
  if (npk) enc.copyBufferToBuffer(bk, 0, rb, 8 * nh, 4 * npk);
  device.queue.submit([enc.finish()]);
  await rb.mapAsync(GPUMapMode.READ);
  const out = new Uint32Array(rb.getMappedRange().slice(0));
  rb.unmap();
  for (const b of [S, bp, bh, bk, rb]) b.destroy();
  return { hit: out.subarray(0, 2 * nh), packed: out.subarray(2 * nh) };
}

const LO = 4000000000000000000n, HI = 5000000000000000000n;
const configs = [
  ["random q_first, delta 233750000", () => rndBig(LO, HI) | 1n, 233750000],
  ["q_first = 4e18 + 1, delta 233750000", () => LO + 1n, 233750000],
  ["q_first = 5e18 - 1, delta 250000000", () => HI - 1n, 250000000],
  ["random q_first, delta 2^31 - 1", () => rndBig(LO, HI) | 1n, 2 ** 31 - 1],
  ["random q_first, random delta", () => rndBig(LO, HI) | 1n, rnd(1, 2 ** 31)],
  ["random q_first, delta 1 (parking)", () => rndBig(LO, HI) | 1n, 1],
  ["random q_first, delta 3", () => rndBig(LO, HI) | 1n, 3],
];
for (const [what, qf, delta] of configs) {
  const q = qf();
  // packed: odd p >= 36000 with p^2 < q_first; hit list: odd p in [36000, 2^32),
  // with p^2 just below / at / above q_first, r = 0 cases and the extremes
  const sq = Number(BigInt(Math.floor(Math.sqrt(Number(q)))));
  const npk = N / 4;
  const ps = new Uint32Array(N);
  for (let i = 0; i < N; i++) {
    const k = i % 8;
    let p = i < npk ? odd(k === 0 ? 36001 : k === 1 ? rnd(36001, 100000) : rnd(36001, sq - 2))
      : odd(k === 0 ? rnd(sq - 64, sq + 64) : k === 1 ? 0xFFFFFFFF - 2 * rnd(0, 500) : k === 2 ? 36001 + 2 * rnd(0, 500) : rnd(36001, 2 ** 32));
    if (p < 36001) p = 36001;
    ps[i] = p;
  }
  // r = 0: q_first itself a multiple of some p (odd q, odd p)
  for (let i = npk; i < npk + 8; i++) {
    for (const p of [3n * 5n * 7n * 11n * 13n * 17n, 36003n]) if (q % p === 0n) ps[i] = Number(p);
  }
  const g = await gpu(q, delta, ps, npk);
  let bad = 0, first = "";
  for (let i = 0; i < npk; i++) {
    const want = packedRef(q, BigInt(ps[i]));
    if (g.packed[i] !== want) { if (!bad++) first = `packed p=${ps[i]}: ${g.packed[i]} != ${want}`; }
  }
  let parked = 0, sqPath = 0;
  for (let i = npk; i < N; i++) {
    const p = BigInt(ps[i]);
    const [j, off] = hitRef(q, p, BigInt(delta));
    if (j === J_FAR) parked++;
    if (p * p >= q) sqPath++;
    if (g.hit[2 * (i - npk)] !== j || g.hit[2 * (i - npk) + 1] !== off) { if (!bad++) first = `hit p=${p}: (${g.hit[2 * (i - npk)]}, ${g.hit[2 * (i - npk) + 1]}) != (${j}, ${off})`; }
  }
  check(bad === 0, `kernel = reference: ${what} (q_first ${q}; ${npk} packed, ${N - npk} hit, ${sqPath} on the p^2 path, ${parked} parked)${bad ? `: ${bad} wrong, first ${first}` : ""}`);
}
if (failures) { console.log(`STATES FAILED (${failures})`); process.exit(1); }
console.log("STATES OK");
