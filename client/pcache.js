// goldbach-web: base primes generated once per device, then cached.
// The core writes and validates the blob (gb_cache_*: header + 64-bit check);
// a store moves its bytes between disk and WASM memory in chunks, so loading
// never holds a second full copy (phones). Stores: OPFS in browser workers,
// a file in Node.js (tests, benchmark, auditor).

const LIMIT = 5000000000000000000n;
const NAME = "primes-5e18.v1.bin";
const CHUNK = 16 << 20;

/// Loads the primes into the core M (lite: phone form), from the cache when it
/// is valid, otherwise by generating them (and caching the result).
/// @return { source: "cache" | "generated", seconds }
export async function loadPrimes(M, lite, store, onStatus = () => {}) {
  const t0 = performance.now();
  const sec = () => (performance.now() - t0) / 1000;
  if (store) {
    try {
      if (await fromCache(M, lite, store)) return { source: "cache", seconds: sec() };
    } catch (e) { onStatus(`prime cache unusable (${e?.message ?? e}); generating`); }
    await store.remove().catch(() => {});
  }
  onStatus("generating");
  if (!(lite ? M._gb_init_lite(LIMIT) : M._gb_init(LIMIT))) throw new Error(M.UTF8ToString(M._gb_error()));
  if (store) {
    try { await toCache(M, store); } catch (e) { onStatus(`could not cache the primes (${e?.message ?? e})`); await store.remove().catch(() => {}); }
  }
  return { source: "generated", seconds: sec() };
}

async function fromCache(M, lite, store) {
  const size = await store.size();
  if (size < 32) return false;
  const hp = M._gb_malloc(32);
  try {
    await store.read(0, 32, (n) => new Uint8Array(M.HEAPU8.buffer, hp, n));
    if (!M._gb_cache_open(LIMIT, hp)) return false;
    const h = new Uint32Array(M.HEAPU8.buffer, hp, 8);
    const count = h[4], nbase = h[5];
    if (size !== 32 + 4 * nbase + count) return false;
    await store.read(32, 4 * nbase, (n, o) => new Uint8Array(M.HEAPU8.buffer, M._gb_bases() + o, n));
    await store.read(32 + 4 * nbase, count, (n, o) => new Uint8Array(M.HEAPU8.buffer, M._gb_gaps() + o, n));
    if (!M._gb_init_cached(LIMIT, hp, lite ? 1 : 0)) throw new Error(M.UTF8ToString(M._gb_error()));
    return true;
  } finally {
    M._gb_free(hp);
  }
}

async function toCache(M, store) {
  const hp = M._gb_malloc(32);
  try {
    M._gb_cache_header(hp);
    const h = new Uint32Array(M.HEAPU8.buffer, hp, 8).slice();
    const count = h[4], nbase = h[5];
    await store.write([
      { n: 32, view: () => new Uint8Array(h.buffer) },
      { n: 4 * nbase, view: (n, o) => new Uint8Array(M.HEAPU8.buffer, M._gb_bases() + o, n) },
      { n: count, view: (n, o) => new Uint8Array(M.HEAPU8.buffer, M._gb_gaps() + o, n) },
    ]);
  } finally {
    M._gb_cache_done();
    M._gb_free(hp);
  }
}

// Views are re-created per chunk (WASM memory may grow and detach old views).
async function eachChunk(n, fn) {
  for (let o = 0; o < n; o += CHUNK) await fn(Math.min(CHUNK, n - o), o);
}

/// OPFS store (dedicated workers): sync access handles where available.
export async function opfsStore() {
  if (!navigator.storage?.getDirectory) return null;
  const dir = await navigator.storage.getDirectory();
  const file = () => dir.getFileHandle(NAME, { create: true });
  return {
    async size() {
      try { return (await (await dir.getFileHandle(NAME)).getFile()).size; } catch { return 0; }
    },
    async read(at, n, view) {
      const f = await (await dir.getFileHandle(NAME)).getFile();
      await eachChunk(n, async (k, o) => {
        view(k, o).set(new Uint8Array(await f.slice(at + o, at + o + k).arrayBuffer()));
      });
    },
    async write(parts) {
      const fh = await file();
      if (fh.createSyncAccessHandle) {
        const h = await fh.createSyncAccessHandle();
        try {
          h.truncate(0);
          let at = 0;
          for (const p of parts) await eachChunk(p.n, async (k, o) => { h.write(p.view(k, o), { at: at + o }); });
          for (const p of parts) at += p.n;
          h.flush();
        } finally { h.close(); }
      } else {
        const w = await fh.createWritable();
        try { for (const p of parts) await eachChunk(p.n, async (k, o) => { await w.write(p.view(k, o).slice()); }); }
        finally { await w.close(); }
      }
    },
    async remove() { await dir.removeEntry(NAME); },
  };
}

/// File store (Node.js: the tests, the benchmark, the auditor). node:fs is
/// imported on first use, so browsers never load it.
export function fileStore(path) {
  const fs = () => import("node:fs/promises");
  return {
    async size() { try { return (await (await fs()).stat(path)).size; } catch { return 0; } },
    async read(at, n, view) {
      const f = await (await fs()).open(path, "r");
      try {
        await eachChunk(n, async (k, o) => {
          const v = view(k, o);
          let got = 0;
          while (got < k) {
            const { bytesRead } = await f.read(v, got, k - got, at + o + got);
            if (bytesRead === 0) throw new Error("short read");
            got += bytesRead;
          }
        });
      } finally { await f.close(); }
    },
    async write(parts) {
      const tmp = path + ".tmp";
      const f = await (await fs()).open(tmp, "w");
      try {
        let at = 0;
        for (const p of parts) {
          await eachChunk(p.n, async (k, o) => { const v = p.view(k, o); let w = 0; while (w < k) w += (await f.write(v, w, k - w, at + o + w)).bytesWritten; });
          at += p.n;
        }
      } finally { await f.close(); }
      await (await fs()).rename(tmp, path);
    },
    async remove() { await (await fs()).rm(path); },
  };
}
