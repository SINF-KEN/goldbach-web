// Test of the WASM core: base primes, Miller-Rabin, reference class map.
//   node test/core_test.mjs [q_first] [q_odds]
import { loadCore } from "./core.mjs";

const M = await loadCore();
const limit = 5000000000000000000n;
let t0 = performance.now();
const np = M._gb_init(limit);
if (!np) throw new Error(M.UTF8ToString(M._gb_error()));
console.log(`base primes: ${np} in ${((performance.now() - t0) / 1000).toFixed(2)} s`);

const qFirst = BigInt(process.argv.slice(2)[0] ?? "3999999999999990219");   // seg_start 4e18 - 9781
const qOdds = BigInt(process.argv.slice(2)[1] ?? "4000000");
const geo = M._gb_malloc(64);
M._gb_geo30(qFirst, qOdds, geo);
const g = new Uint32Array(M.HEAPU32.buffer, geo, 16).slice();
const nmax = Math.max(...g.slice(8, 16));
const cw32 = Math.ceil(nmax / 32);
const map = M._gb_malloc(8 * cw32 * 4);
t0 = performance.now();
if (!M._gb_ref_class_map(qFirst, qOdds, map, cw32)) throw new Error(M.UTF8ToString(M._gb_error()));
console.log(`ref map: ${qOdds} odd q, ${((performance.now() - t0) / 1000).toFixed(2)} s`);
const words = new Uint32Array(M.HEAPU32.buffer, map, 8 * cw32);

// popcount + Miller-Rabin spot checks (every class, random bits)
const RES = [1, 7, 11, 13, 17, 19, 23, 29];
let primes = 0;
for (const w of words) { let x = w; while (x) { x &= x - 1; primes++; } }
let bad = 0, checked = 0;
for (let c = 0; c < 8; c++) {
  const phi = BigInt(g[c]), n = g[8 + c];
  const Q = qFirst + 2n * phi;
  if (Number((Q % 30n)) !== RES[c]) throw new Error(`class ${c} residue mismatch`);
  for (let k = 0; k < 4000; k++) {
    const t = Math.floor(Math.random() * n);
    const bit = (words[c * cw32 + (t >> 5)] >>> (t & 31)) & 1;
    const q = Q + 30n * BigInt(t);
    if (bit !== M._gb_is_prime(q)) bad++;
    checked++;
  }
}
console.log(`primes in map: ${primes}; MR spot checks: ${checked}, mismatches: ${bad}`);
if (bad) process.exit(1);
