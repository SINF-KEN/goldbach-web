// Base primes for the GPU tests and the benchmark. With GB_PRIMES_CACHE set, the
// first run generates them with the given checkout's core into that file (the
// benchmark CI uses a fresh file per job: the candidate's own generator) and every
// later run loads it in ~0.1 s instead of ~6.6 s; the cache's content must match
// the known fingerprint of the correct primes for 5e18 (count + the 64-bit check
// of client/pcache.js's blob), or the run fails. Without GB_PRIMES_CACHE: generated
// in memory as before.
import { open, realpath, rm } from "node:fs/promises";
import { pathToFileURL, fileURLToPath } from "node:url";
export const LIMIT = 5000000000000000000n;
export const KNOWN_PRIMES = { count: 109215731, checkLo: 0x1c5288da, checkHi: 0x2423cbee };

export async function initPrimes(M, lite, root = fileURLToPath(new URL("..", import.meta.url))) {
  const path = process.env["GB_PRIMES_CACHE"];
  if (!path) {
    if (!(lite ? M._gb_init_lite(LIMIT) : M._gb_init(LIMIT))) throw new Error("core init: " + M.UTF8ToString(M._gb_error()));
    return;
  }
  const { loadPrimes, fileStore } = await import(pathToFileURL(`${await realpath(root)}/client/pcache.js`).href);
  await loadPrimes(M, lite, fileStore(path));
  const f = await open(path, "r");
  const h = new Uint32Array(8);
  try {
    if ((await f.read(new Uint8Array(h.buffer), 0, 32, 0)).bytesRead !== 32) throw new Error("short header");
  } finally { await f.close(); }
  if (h[4] !== KNOWN_PRIMES.count || h[6] !== KNOWN_PRIMES.checkLo || h[7] !== KNOWN_PRIMES.checkHi) {
    await rm(path, { force: true });
    throw new Error(`base primes differ from the known ones (count ${h[4]}, check ${h[7].toString(16)}${h[6].toString(16).padStart(8, "0")})`);
  }
  if (M._gb_prime_count() >>> 0 !== KNOWN_PRIMES.count) throw new Error("core prime count differs from the known one");
}
