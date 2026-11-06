// Prime cache: cached tables identical to generated ones (lite and full), a
// damaged cache is rejected and regenerated. No GPU.
//   node test/pcache_test.mjs
import { loadCore } from "./core.mjs";
import { mkdtemp, open, rm, truncate } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileStore, loadPrimes } from "../client/pcache.js";
const path = join(await mkdtemp(join(tmpdir(), "gbweb-pcache-")), "primes.bin");
const store = fileStore(path);
const tables = (M, lite) => {
  const n = M._gb_prime_count() >>> 0;
  if (!lite) return { n, d: new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), n).slice() };
  return { n, d: new Uint8Array(M.HEAPU8.buffer, M._gb_gaps(), n).slice(), b: new Uint32Array(M.HEAPU32.buffer, M._gb_bases(), M._gb_base_count()).slice(),
           s: new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), M._gb_small_count()).slice() };
};
const same = (a, b) => a.n === b.n && ["d", "b", "s"].every((k) => (a[k] === undefined && b[k] === undefined) ||
  (a[k].length === b[k].length && a[k].every((v, i) => v === b[k][i])));
let fail = 0;
const check = (ok, what) => { console.log(`${ok ? "ok  " : "FAIL"} ${what}`); if (!ok) fail++; };
for (const lite of [true, false]) {
  const G = await loadCore();                 // reference: generated directly
  if (!(lite ? G._gb_init_lite(5000000000000000000n) : G._gb_init(5000000000000000000n))) throw new Error("init");
  if (lite) check((await loadPrimes(await loadCore(), lite, store)).source === "generated", "lite: first load generates and caches");
  const C = await loadCore();
  const c = await loadPrimes(C, lite, store);
  check(c.source === "cache", `${lite ? "lite" : "full"}: second load from cache (${c.seconds.toFixed(2)} s; the cache is shared by both modes)`);
  check(same(tables(G, lite), tables(C, lite)), `${lite ? "lite" : "full"}: cached tables identical`);
  check(C._gb_prime_at(123456789 % (C._gb_prime_count() >>> 0)) === G._gb_prime_at(123456789 % (G._gb_prime_count() >>> 0)), "prime_at agrees");
}
// damage one byte in the middle of the gaps: rejected, regenerated, rewritten
const f = await open(path, "r+");
const b = new Uint8Array(1); await f.read(b, 0, 1, 50_000_000); await f.write(new Uint8Array([b[0] ^ 1]), 0, 1, 50_000_000); await f.close();
const D = await loadCore();
const notes = [];
const d = await loadPrimes(D, true, store, (s) => notes.push(s));
check(d.source === "generated" && notes.some((s) => /check mismatch/.test(s)), `damaged cache rejected (${notes[0]})`);
const E = await loadCore();
check((await loadPrimes(E, true, store)).source === "cache", "rewritten cache loads again");
// truncated file: rejected
await truncate(path, 1000);
check((await loadPrimes(await loadCore(), true, store)).source === "generated", "truncated cache rejected");
await rm(dirname(path), { recursive: true, force: true });
if (fail) { console.log(`${fail} FAILED`); process.exit(1); }
console.log("PRIME CACHE OK");
