// goldbach-web: known-answer self-test. Before a device contributes, it
// verifies a fixed window whose exact result is known and compares the sha256
// of the canonical result (the fields the server hashes). A GPU or driver that
// computes wrong maps (e.g. Mesa RADV on some RDNA2 iGPUs loses workgroup
// atomics, a Mesa RADV driver bug) is refused instead of submitting
// wrong results. The answer is configuration-independent: identical in full
// and phone mode, on every segment size and on the CPU verifier.

export const KNOWN = {
  start: 4999999998000000000n, limit: 5000000000000000000n,    // 1e9 evens at the top of the range
  hash: "3672004b8c97290d",                                     // first 16 hex of sha256(canonical JSON)
};

/// Canonical result hash as test/golden.mjs: finish() without the
/// configuration-dependent pending / leftovers.
export async function resultHash(r) {
  const { pending: _p, leftovers: _l, ...canon } = r;
  const json = JSON.stringify(canon, (_, v) => (typeof v === "bigint" ? v.toString() : v));
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(json)));
  return Array.from(d.slice(0, 8), (b) => b.toString(16).padStart(2, "0")).join("");
}

/// Runs the known window with the given engine configuration.
/// @return { ok, hash, seconds }
export async function selfTest(M, device, src, cfg, segment) {
  // the engine module of the same version as this one (worker: ?v=<engine hash>)
  const { DenseEngine } = await import(`./engine.js${new URL(import.meta.url).search}`);
  const t0 = performance.now();
  const eng = new DenseEngine(M, device, src, cfg);
  try {
    await eng.prepare(KNOWN.start, KNOWN.limit, segment);
    await eng.runAll();
    const hash = await resultHash(eng.finish());
    return { ok: hash === KNOWN.hash, hash, seconds: (performance.now() - t0) / 1000 };
  } finally {
    eng.destroy();
  }
}
