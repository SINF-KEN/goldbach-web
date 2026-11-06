// M2 test: the WebGPU tile sieve vs the exact CPU reference restricted to the
// same primes [0, split), on full segments.
//   node test/tile_test.mjs [seg_start] [segment] [adapter]
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
import { loadCore } from "./core.mjs";

const M = await loadCore();
const limit = 5000000000000000000n;
if (!M._gb_init(limit)) throw new Error(M.UTF8ToString(M._gb_error()));

const segStart = BigInt(process.argv.slice(2)[0] ?? "4000000000000000000");
const segment = BigInt(process.argv.slice(2)[1] ?? "467500000");
const pref = process.argv.slice(2)[2] ?? "high-performance";
const MAXGB = 9781n;
const qFirst = segStart - MAXGB;
const segEnd = segStart + segment - 2n;
const qOdds = (segEnd - 3n - qFirst) / 2n + 1n;

const u32 = (ptr, n) => new Uint32Array(M.HEAPU32.buffer, ptr, n).slice();
const geoP = M._gb_malloc(64);
M._gb_geo30(qFirst, qOdds, geoP);
const geo = u32(geoP, 16);
const nmax = Math.max(...geo.slice(8, 16));
const TILE_BITS = 131072;
const tpc = Math.ceil(nmax / TILE_BITS);
const cw32 = tpc * 4096;

const split = Math.min(36000, M._gb_prime_count());
const maskCount = M._gb_prime_index(64);
const ownEnd = M._gb_prime_index(512);
const primesAll = new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), split).slice();
const fbP = M._gb_malloc(8 * split * 4);
if (!M._gb_tile_first_bits(qFirst, qOdds, split, fbP)) throw new Error(M.UTF8ToString(M._gb_error()));
const fb = u32(fbP, 8 * split);
const gwP = M._gb_malloc(1 << 20), gdP = M._gb_malloc(64);
const ng = M._gb_mask_groups(4096, gwP, (1 << 20) / 4, gdP);
if (!ng) throw new Error(M.UTF8ToString(M._gb_error()));
const gdesc = u32(gdP, 2 * ng);
const gwords = u32(gwP, 2 * (gdesc[2 * (ng - 1)] + gdesc[2 * ng - 1]));
const P = new Uint32Array(96);
P.set([cw32, tpc, ng, split, ownEnd, maskCount]);
for (let c = 0; c < 8; c++) P[8 + c] = geo[8 + c];
P.set(gdesc, 16);
const yP = M._gb_malloc(64);
for (let c = 0; c < 8; c++) {
  const Qc = qFirst + 2n * BigInt(geo[c]);
  const n = M._gb_group_offsets(Qc, 4096, yP);
  if (n !== ng) throw new Error("group count mismatch");
  P.set(u32(yP, n), 32 + 8 * c);
}
console.log(`segment ${segStart} +${segment}: q_odds ${qOdds}, tpc ${tpc}, split ${split}, masks ${maskCount}, own_end ${ownEnd}, groups ${ng}`);

// ---- GPU ----
const adapter = await navigator.gpu.requestAdapter({ powerPreference: pref });
if (!adapter) throw new Error("no adapter");
console.log("adapter:", adapter.info.description);
const device = await adapter.requestDevice({ requiredLimits: { maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize } });
const code = await readFile(new URL("../kernels/tile.wgsl", import.meta.url), "utf8");
const mod = device.createShaderModule({ code });
const info = await mod.getCompilationInfo();
for (const m of info.messages) console.log(m.type, m.lineNum, m.message);
const pipe = device.createComputePipeline({ layout: "auto", compute: { module: mod, entryPoint: "tile_sieve" } });
const buf = (data, usage) => {
  const size = typeof data === "number" ? data : data.byteLength;
  const b = device.createBuffer({ size: Math.max(16, Math.ceil(size / 4) * 4), usage, mappedAtCreation: typeof data !== "number" });
  if (typeof data !== "number") { new Uint32Array(b.getMappedRange()).set(data); b.unmap(); }
  return b;
};
const S = GPUBufferUsage.STORAGE;
const mapBuf = buf(8 * cw32 * 4, S | GPUBufferUsage.COPY_SRC);
const bg = device.createBindGroup({
  layout: pipe.getBindGroupLayout(0),
  entries: [mapBuf, buf(primesAll, S), buf(fb, S), buf(gwords, S), buf(P, S)].map((b, i) => ({ binding: i, resource: { buffer: b } })),
});
const nwg = 8 * tpc;
const read = device.createBuffer({ size: 8 * cw32 * 4, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
let t0 = performance.now();
const reps = 20;
for (let r = 0; r < reps; r++) {
  const enc = device.createCommandEncoder();
  const pass = enc.beginComputePass();
  pass.setPipeline(pipe); pass.setBindGroup(0, bg); pass.dispatchWorkgroups(Math.min(nwg, 65535), Math.ceil(nwg / 65535));
  pass.end();
  if (r === reps - 1) enc.copyBufferToBuffer(mapBuf, 0, read, 0, 8 * cw32 * 4);
  device.queue.submit([enc.finish()]);
}
await read.mapAsync(GPUMapMode.READ);
const ms = (performance.now() - t0) / reps;
const gpu = new Uint32Array(read.getMappedRange()).slice();
console.log(`tile sieve: ${ms.toFixed(3)} ms per segment (wall, ${reps} reps)`);

// ---- reference ----
const refP = M._gb_malloc(8 * cw32 * 4);
t0 = performance.now();
if (!M._gb_ref_class_map_range(qFirst, qOdds, refP, cw32, 0, split)) throw new Error(M.UTF8ToString(M._gb_error()));
const ref = new Uint32Array(M.HEAPU32.buffer, refP, 8 * cw32);
console.log(`reference: ${((performance.now() - t0) / 1000).toFixed(2)} s`);
let diff = 0, first = -1;
for (let i = 0; i < ref.length; i++) if (ref[i] !== gpu[i]) { if (first < 0) first = i; diff++; }
let pop = 0; for (const w of gpu) { let x = w; while (x) { x &= x - 1; pop++; } }
console.log(diff === 0 ? `MAP IDENTICAL (${ref.length} words, ${pop} bits set)` : `MAP DIFFERS in ${diff} words, first at word ${first} (class ${Math.floor(first / cw32)}): gpu ${gpu[first]?.toString(16)} ref ${ref[first]?.toString(16)}`);
if (diff) process.exit(1);
