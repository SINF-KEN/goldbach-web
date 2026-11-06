// goldbach-web engine: the dense pipeline on WebGPU. Segments are encoded in
// batches (one submit each), several are in flight, and their results are
// collected in order. Runs unchanged in browsers (module worker) and Node.js (Dawn). The host mathematics
// lives in the WASM core (core.js); every bound the kernels rely on is checked
// here before anything is dispatched, and violations throw (fail closed).

export const MAX_GB_PRIME = 9781n;
const TILE_BITS = 131072;
const TILE_WORDS = 4096;
const HL_MAX_K = 128;
const KA_MAX = 128;        // check.wgsl KA_MAX
const KA_CNT = 96;         // check.wgsl KA_CNT (per-invocation counters)
const HSTRIDE = 1280;      // check.wgsl HSTRIDE
const BLK_A = 64 * 2 * 32; // check.wgsl BLK
const TAIL_T = 3000;       // gstats / check30 tail bound: every even with p(n) >= 3000 reaches phase B
const FIRST_BINS = 4896;   // p >> 1 for p <= 9781
// prime gaps (count30.wgsl): histogram bins (g / 2, g < 2048), listed gaps >= 700
// per segment (~1 per 2e9 integers expected), words of the totals buffer
const GAP_HBINS = 1024, GAP_TCAP = 1024, GAP_WORDS = 4 + GAP_HBINS + 2 * GAP_TCAP;
const RIDGE_FLOOR = 7000;  // every even with p(n) >= 7000 is reported with its n (deterministic)
const HL_J_FAR = 0xF0000000;   // MUST match core.cpp: parked hit-list primes (no multiple in the run)

const u32view = (M, ptr, n) => new Uint32Array(M.HEAPU32.buffer, ptr, n).slice();
function coreCheck(M, ok) {
  if (!ok) throw new Error("core: " + M.UTF8ToString(M._gb_error()));
}

/// Run geometry, identical to goldbach_l0's segment_geometry for a run
/// [start, limit] in segments of `segment` integers (BigInts).
/// @return { start, limit, segment, delta = segment / 2, qOddsFull, segs }:
///         segs[k] = { segStart, segEnd, qFirst, qOdds, segEvens }; the last
///         segment ends at limit (it may be shorter).
/// @throws if start, limit or segment is odd, or limit < start.
export function runGeometry(start, limit, segment) {
  if (start % 2n || limit % 2n || segment % 2n || limit < start) throw new Error("geometry: even start <= limit, even segment");
  const delta = segment / 2n;
  const qOddsFull = (segment - 5n + MAX_GB_PRIME) / 2n + 1n;
  const segs = [];
  for (let s0 = start; ; s0 += segment) {
    const segEnd = (limit - s0 <= segment) ? limit : s0 + segment - 2n;
    const qFirst = s0 - MAX_GB_PRIME;
    const qOdds = (segEnd - 3n - qFirst) / 2n + 1n;
    segs.push({ segStart: s0, segEnd, qFirst, qOdds, segEvens: (segEnd - s0) / 2n + 1n });
    if (segEnd === limit) break;
  }
  return { start, limit, segment, delta, qOddsFull, segs };
}

/// Hit-list region sizing (as the native build): mean m = lam / G per
/// region, capacity m * 1.10 + 10 sqrt(m) + 64 rounded up to a power of two.
function regionSizing(lam, maxGroups = 4096) {
  const need = (m) => m * 1.10 + 10 * Math.sqrt(m) + 64;
  const mmax = (cg) => { let lo = 0, hi = cg; for (let i = 0; i < 60; i++) { const m = (lo + hi) / 2; if (need(m) <= cg) lo = m; else hi = m; } return lo; };
  let capg = 64;
  while (Math.ceil(lam / mmax(capg)) > maxGroups) capg *= 2;
  return { capg, G: Math.max(1, Math.ceil(lam / mmax(capg))) };
}

/// Run configuration per mode (the worker and the benchmark CI use it, so a
/// branch that changes it is measured): hit-list batch, list memory, segment sizes
/// to try (the first that fits the device).
export function engineDefaults(lite) {
  // one key per line: experiment branches that tune different keys merge cleanly
  if (lite) {
    return {
      cfg: {
        K: 32,
        listBytes: 192 << 20,
        lite: true,
        KA: 64,
      },
      segments: [500000000n, 256000000n, 192000000n, 128000000n, 64000000n],
    };
  }
  return {
    cfg: {
      K: 32,
      listBytes: 768 << 20,
      split: 6000,
      KA: 64,
    },
    segments: [467500000n, 233750000n, 128000000n],
  };
}

/// The verifier of one run [start, limit] on one GPU device: exact statistics
/// of every even in the run (see finish()). Use: prepare(), then runAll() (or
/// runSegment(k) for k = 0, 1, ... in order), then finish(); destroy() frees
/// it. An idle engine can be re-targeted to another run with reset(). Every
/// bound the kernels rely on is checked before dispatch, and any device error
/// flag, list overflow or accounting mismatch throws: a run never yields a
/// partial or unchecked result.
export class DenseEngine {
  /// @param M       instantiated WASM core, gb_init(limit) (full) or
  ///                gb_init_lite(limit) (phone mode) done
  /// @param device  GPUDevice
  /// @param src     WGSL sources: { tile, large, harvest, check, count30,
  ///                largeLite, harvestLite } (kernels/*.wgsl)
  /// @param cfg     overrides (engineDefaults gives the tuned ones):
  ///                lite (phone mode), split (tile/large prime split), K (hit-list
  ///                batch in segments), listBytes (hit-list memory cap), hlFactor,
  ///                wgLarge, ppi, wgHarvest (kernel shapes), KA (checker phase A),
  ///                pendCap, ridgeCap, leftCap (list capacities: overflow fails
  ///                closed), batch (segments per submit), depth (segments in
  ///                flight), profile (timestamp queries), ridgeFloor (tests only)
  constructor(M, device, src, cfg = {}) {
    this.M = M;
    this.dev = device;
    // one key per line: experiment branches that tune different keys merge cleanly
    this.cfg = {
      split: 14000,
      K: 64,
      wgLarge: 256,
      ppi: 1,
      wgHarvest: 256,
      batch: 2,
      ...cfg,
    };
    this.src = src;
  }

  /// Prepares a run: geometry, guards, tables, initial states, buffers.
  /// Every resource is created inside a validation error scope: a device that
  /// rejects any of them (buffer limits, shader features) fails here, closed.
  async prepare(start, limit, segment) {
    this.dev.pushErrorScope("validation");
    let ok = false;
    try {
      await this.prepareInner(start, limit, segment);
      ok = true;
    } finally {
      const e = await this.dev.popErrorScope();
      if (e && ok) throw new Error(`the GPU rejected the setup: ${e.message}`);
    }
    return this;
  }

  /// Run layout: the geometry and every size the buffers and pipelines depend
  /// on, after the guards (fail closed). Pure: nothing is assigned.
  layout(start, limit, segment) {
    const { M, dev, cfg } = this;
    const geo = runGeometry(start, limit, segment);
    const np = M._gb_prime_count();
    // lite (cfg.lite, core initialised with gb_init_lite): only the small primes
    // are u32; the rest come from the half-gap stream and no state is kept
    const lite = !!cfg.lite;
    const primes = new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), lite ? M._gb_small_count() : np);
    const pAt = (i) => (lite ? M._gb_prime_at(i) >>> 0 : primes[i]);   // WASM returns u32 as signed i32
    const qof = geo.qOddsFull, delta = geo.delta;
    // ---- guards (fail closed) ----
    if (qof >= 2n ** 32n / 12n) throw new Error("segment too large: 12 q_odds must stay below 2^32");
    const pmax = pAt(np - 1);
    if (delta + BigInt(pmax) >= 2n ** 32n) throw new Error("delta + p_max must stay below 2^32");
    if (geo.segs[0].qFirst <= BigInt(pmax) * 3n) throw new Error("range too low for the web path");
    if (geo.segs.length >= HL_J_FAR / 2) throw new Error("run too long");
    const split = Math.min(cfg.split, np);
    const maskCount = M._gb_prime_index(64);
    const ownEnd = Math.min(M._gb_prime_index(512), split);
    if (ownEnd - maskCount > 256) throw new Error("too many owned primes");
    // hit-list primes: p > hlFactor q_odds_full (default 1: every one-hit prime;
    // native BUCKET_MIN_P_FACTOR = 3 streams 1 B per prime, the web 12 B)
    const thr = BigInt(Math.floor((cfg.hlFactor ?? 1) * Number(qof)));
    const hb0 = thr >= 2n ** 32n ? np : M._gb_prime_index(Number(thr + 1n));
    const pa = split, pb = Math.max(split, hb0);
    const pone = Math.max(pa, Math.min(pb, M._gb_prime_index(Number(qof))));
    if (pb > pa && BigInt(pAt(pb - 1)) ** 2n >= geo.segs[0].qFirst) throw new Error("packed p^2 reaches q_first");
    // modq (large.wgsl packed starts, harvest.wgsl states) needs p >= 36000; the states' division delta < 2^31
    if (!lite && pa < np && pAt(pa) < 36000) throw new Error("modq: packed primes below 36000");
    if (delta >= 2n ** 31n) throw new Error("states: delta must stay below 2^31");
    if (lite && split > primes.length) throw new Error("split beyond the small primes");

    // ---- hit-list sizing ----
    const nh = np - pb;
    let K = 0, G = 0, capg = 0;
    if (nh > 0) {
      const lo = pAt(pb), hi = pmax;
      const lam = Number(qof) * Math.max(0, Math.log(Math.log(hi)) - Math.log(Math.log(lo))) * (5760 / 15015) + 1;
      ({ G, capg } = regionSizing(lam));
      const maxBind = Math.min(dev.limits.maxStorageBufferBindingSize, cfg.listBytes ?? Infinity);
      K = Math.min(cfg.K, HL_MAX_K, Math.floor(maxBind / (G * capg * 4)));
      // harvest_lite keeps batch offsets in u32: (K - 1) delta + q_odds < 2^32
      if (lite) K = Math.min(K, Math.floor(Number(2n ** 32n - 1n - qof) / Number(delta)) + 1);
      if (K < 1) throw new Error("hit lists do not fit the device's buffer limits");
    }
    if (capg & (capg - 1)) throw new Error("hit-list region capacity must be a power of two");
    // map geometry (all segments share the first one's shape)
    const g0 = this.geo30(geo.segs[0]);
    const tpc = Math.ceil(Math.max(...g0.n) / TILE_BITS);
    const cw32 = tpc * TILE_WORDS;
    if (32 * cw32 > 2 ** 24) throw new Error("segment too large: class arrays must stay below 2^24 bits (tile.wgsl fmod)");
    return { geo, lite, np, split, maskCount, ownEnd, pa, pb, pone, ha: pb, hb: np, K, G, capg, tpc, cw32 };
  }

  async prepareInner(start, limit, segment) {
    const { M, dev, cfg } = this;
    const lay = this.layout(start, limit, segment);
    Object.assign(this, lay);
    const { lite, np, split, pa, pb, K, G, capg } = lay;
    const nh = np - pb;
    const primes = new Uint32Array(M.HEAPU32.buffer, M._gb_primes(), lite ? M._gb_small_count() : np);

    // ---- tables ----
    const tabsP = M._gb_malloc(66290 * 4);
    M._gb_wheel_tabs(tabsP);
    const tabs = u32view(M, tabsP, 66290);
    M._gb_free(tabsP);
    const gwP = M._gb_malloc(1 << 20), gdP = M._gb_malloc(64);
    const ng = M._gb_mask_groups(4096, gwP, (1 << 18), gdP);
    coreCheck(M, ng);
    const gdesc = u32view(M, gdP, 2 * ng);
    const gwords = u32view(M, gwP, 2 * (gdesc[2 * (ng - 1)] + gdesc[2 * ng - 1]));
    M._gb_free(gwP); M._gb_free(gdP);
    this.ng = ng; this.gdesc = gdesc;

    // ---- buffers must fit the device's binding limit (fail closed, with the reason) ----
    const maxBind = dev.limits.maxStorageBufferBindingSize;
    const biggest = lite ? Math.ceil(np / 512) * 512 : 4 * np;
    if (biggest > maxBind || (!lite && 8 * (np - pb) > maxBind))
      throw new Error(`this device allows ${Math.round(maxBind / 2 ** 20)} MB per buffer; the ${lite ? "lite" : "full"} engine needs ${Math.round(Math.max(biggest, lite ? 0 : 8 * (np - pb)) / 2 ** 20)} MB${lite ? "" : " (use phone mode)"}`);
    // ---- buffers ----
    const S = GPUBufferUsage.STORAGE, CD = GPUBufferUsage.COPY_DST, CS = GPUBufferUsage.COPY_SRC;
    const mk = (size, usage) => dev.createBuffer({ size: Math.max(16, Math.ceil(size / 16) * 16), usage });
    const up = (data, usage = S) => { const b = mk(data.byteLength, usage | CD); dev.queue.writeBuffer(b, 0, data); return b; };
    // uploads through writeBuffer (Deno's getMappedRange reads a mapped range back
    // into a JS buffer, ~30 MB/s from GPU memory behind the BAR)
    this.bMap = mk(8 * this.cw32 * 4, S | CS);
    this.bPrimes = up(lite ? primes.subarray(0, split) : primes);
    if (lite) {
      // half-gap stream as u32 words (4 primes each), padded to whole 512-prime groups
      const gp = M._gb_gaps(), groups = Math.ceil(np / 512), bytes = groups * 512;
      this.bGaps = mk(bytes, S | CD);
      const whole = np & ~3;
      dev.queue.writeBuffer(this.bGaps, 0, new Uint8Array(M.HEAPU8.buffer, gp, whole));
      if (whole < np) { const t = new Uint8Array(4); t.set(new Uint8Array(M.HEAPU8.buffer, gp + whole, np - whole)); dev.queue.writeBuffer(this.bGaps, whole, t); }
      this.bBases = up(new Uint32Array(M.HEAPU32.buffer, M._gb_bases(), M._gb_base_count()));
    }
    this.bFb = mk(8 * split * 4, S | CD);
    this.bMask = up(gwords);
    this.bP = mk(96 * 4, S | CD);
    this.bTabs = up(tabs);
    this.bL = mk(32 * 4, S | CD);
    this.bH = mk(16 * 4, S | CD);
    this.bErr = mk(16, S | CD | CS);
    dev.queue.writeBuffer(this.bErr, 0, new Uint32Array(4));
    // hit-list states (normal mode only; written per run by writeStates). The
    // packed primes keep none: large.wgsl recomputes their first marks per
    // segment, and the states kernel's packed output (binding 7) stays unused
    if (!lite) this.bPstate = mk(16, S);
    if (nh > 0 && !lite) this.bHstate = mk(nh * 8, S | CD | CS);
    if (!lite) this.bS = mk(8 * 4, S | CD);
    // hit lists, counts
    if (nh > 0) {
      this.bLists = mk(K * G * capg * 4, S);
      this.bGcnt = mk(K * G * 4, S | CD);
    } else {
      this.bHstate = mk(16, S); this.bLists = mk(16, S); this.bGcnt = mk(16, S);
    }

    // ---- shader modules must compile cleanly (fail closed) ----
    for (const [name, code] of Object.entries(this.src)) {
      const info = await dev.createShaderModule({ code }).getCompilationInfo();
      const errs = info.messages.filter((m) => m.type === "error");
      if (errs.length) throw new Error(`kernel ${name}: ${errs[0].lineNum}: ${errs[0].message}`);
    }
    // ---- pipelines ----
    const tileMod = dev.createShaderModule({ code: this.src.tile });
    const largeMod = dev.createShaderModule({ code: lite ? this.src.largeLite : this.src.large });
    const harvMod = dev.createShaderModule({ code: lite ? this.src.harvestLite : this.src.harvest });
    const consts = { WG_LARGE: cfg.wgLarge, PPI: cfg.ppi, WG_HARVEST: cfg.wgHarvest };
    this.pTile = dev.createComputePipeline({ layout: "auto", compute: { module: tileMod, entryPoint: "tile_sieve" } });
    this.pLarge = dev.createComputePipeline({ layout: "auto", compute: lite ? { module: largeMod, entryPoint: "large_sieve" }
      : { module: largeMod, entryPoint: "large_sieve", constants: { WG_LARGE: consts.WG_LARGE, PPI: consts.PPI } } });
    this.pHarv = dev.createComputePipeline({ layout: "auto", compute: lite ? { module: harvMod, entryPoint: "harvest" }
      : { module: harvMod, entryPoint: "harvest", constants: { WG_HARVEST: consts.WG_HARVEST } } });
    const bind = (pipe, bufs) => dev.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: bufs.map((b, i) => ({ binding: i, resource: { buffer: b } })) });
    this.bgTile = bind(this.pTile, [this.bMap, this.bPrimes, this.bFb, this.bMask, this.bP]);
    this.bgLarge = lite
      ? bind(this.pLarge, [this.bMap, this.bGaps, this.bBases, this.bTabs, this.bL, this.bLists, this.bGcnt, this.bErr])
      : dev.createBindGroup({ layout: this.pLarge.getBindGroupLayout(0), entries: [[0, this.bMap], [1, this.bPrimes], [3, this.bTabs], [4, this.bL],
          [5, this.bLists], [6, this.bGcnt], [7, this.bErr]].map(([i, b]) => ({ binding: i, resource: { buffer: b } })) });
    if (nh > 0) this.bgHarv = lite
      ? bind(this.pHarv, [this.bGaps, this.bBases, this.bLists, this.bGcnt, this.bErr, this.bH, this.bTabs])
      : bind(this.pHarv, [this.bPrimes, this.bHstate, this.bLists, this.bGcnt, this.bErr, this.bH, this.bTabs]);
    if (!lite) {
      this.pStates = dev.createComputePipeline({ layout: "auto", compute: { module: harvMod, entryPoint: "states" } });
      this.bgStates = dev.createBindGroup({ layout: this.pStates.getBindGroupLayout(0), entries: [[0, this.bPrimes], [1, this.bHstate], [7, this.bPstate], [8, this.bS]]
        .map(([i, b]) => ({ binding: i, resource: { buffer: b } })) });
    }
    await this.writeStates();
    this.fbP = M._gb_malloc(8 * split * 4);
    this.k = 0;
    this.prepareCheck();
    this.idle = true;
    return this;
  }

  /// Per-run states from the run's q_first (normal mode), computed on the GPU
  /// (harvest.wgsl states, one dispatch over the hit-list primes): their
  /// (segment, offset). The harvest advances them in place during a run, so
  /// every run writes them again. Until 2026-10 the WASM core computed them (gb_packed_state,
  /// gb_harvest_state) and ~0.8 GB went up per slice, ~0.45 s of setup.
  /// Fails closed unless a sample of each kind (both ends and a random block)
  /// equals the core's states exactly (checkStates).
  async writeStates() {
    const { dev, geo, pb, np } = this;
    if (this.lite || np <= pb) return;
    const q = geo.segs[0].qFirst;
    dev.queue.writeBuffer(this.bS, 0, new Uint32Array([Number(q >> 32n), Number(q & 0xFFFFFFFFn), Number(geo.delta),
      pb, pb, np, HL_J_FAR, 0]));
    const enc = dev.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(this.pStates); pass.setBindGroup(0, this.bgStates);
    DenseEngine.dispatch2(pass, Math.ceil((np - pb) / 256));
    pass.end();
    dev.queue.submit([enc.finish()]);
    await this.checkStates();
  }

  /// Compares the GPU's hit-list states with the core's on three blocks of up
  /// to 512 primes: the first, the last (the p^2 > q_first path) and one at a
  /// random position. Throws on any difference.
  async checkStates() {
    const { M, dev, geo, pb, np } = this;
    const B = 512, q = geo.segs[0].qFirst;
    const blocks = (a, b) => {
      if (b - a <= 2 * B) return [[a, b]];
      const r = a + B + Math.floor(Math.random() * (b - a - 3 * B));
      return [[a, a + B], [r, r + B], [b - B, b]];
    };
    const want = [];   // [buffer, byte offset, expected words]
    const tmp = M._gb_malloc(2 * (2 * B) * 4);   // a block: <= 2 B primes, 2 words each
    try {
      for (const [a, b] of np > pb ? blocks(pb, np) : []) {
        coreCheck(M, M._gb_harvest_state(q, Number(geo.delta), a, b, tmp));
        want.push([this.bHstate, (a - pb) * 8, u32view(M, tmp, 2 * (b - a))]);
      }
    } finally { M._gb_free(tmp); }
    const total = want.reduce((n, [, , w]) => n + w.byteLength, 0);
    const rb = dev.createBuffer({ size: Math.max(16, total), usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    try {
      const enc = dev.createCommandEncoder();
      let o = 0;
      for (const [buf, off, w] of want) { enc.copyBufferToBuffer(buf, off, rb, o, w.byteLength); o += w.byteLength; }
      dev.queue.submit([enc.finish()]);
      await rb.mapAsync(GPUMapMode.READ);
      const got = new Uint32Array(rb.getMappedRange().slice(0));
      rb.unmap();
      o = 0;
      for (const [buf, off, w] of want) {
        for (let i = 0; i < w.length; i++)
          if (got[o + i] !== w[i]) throw new Error(`states: GPU hit-list state differs from the core at byte ${off + 4 * i} (${got[o + i]} != ${w[i]})`);
        o += w.length;
      }
    } finally { rb.destroy(); }
  }

  /// Re-targets an idle engine to the run [start, limit] in its segment size:
  /// keeps buffers, pipelines, primes and tables; the geometry, the per-run
  /// states, the error flags and the totals are new. Every per-segment input is
  /// written per segment anyway, so the run is exactly a freshly prepared one.
  /// @return false (engine unchanged) when the run needs another layout.
  async reset(start, limit) {
    if (!this.idle) return false;
    const lay = this.layout(start, limit, this.geo.segment);
    for (const [k, v] of Object.entries(lay)) if (k !== "geo" && this[k] !== v) return false;
    this.geo = lay.geo;
    await this.writeStates();
    this.dev.queue.writeBuffer(this.bErr, 0, new Uint32Array(4));
    this.resetTotals();
    this.k = 0;
    return true;
  }

  /// Checker + statistics: candidate lists, buffers, pipelines, run totals.
  prepareCheck() {
    const { M, dev, cfg } = this;
    const S = GPUBufferUsage.STORAGE, CD = GPUBufferUsage.COPY_DST, CS = GPUBufferUsage.COPY_SRC;
    const mk = (size, usage) => dev.createBuffer({ size: Math.max(16, Math.ceil(size / 16) * 16), usage });
    // candidate lists per class (ascending, useful only)
    const cl = M._gb_malloc(4 * 1280);
    const lists = [];
    let off = 0;
    this.cand = [];
    for (let cr = 0; cr < 15; cr++) {
      const n = M._gb_cand_list(cr, cl);
      const v = u32view(M, cl, n);
      if (n > HSTRIDE) throw new Error("candidate list longer than the histogram stride");
      this.cand.push({ off, n, p: Array.from(v, (x) => x & 0xFFFF), k: Array.from(v, (x) => x >>> 16) });
      lists.push(v);
      off += n;
    }
    M._gb_free(cl);
    const all = new Uint32Array(off);
    let o = 0;
    for (const v of lists) { all.set(v, o); o += v.length; }
    // phase-A length: every phase-A candidate < tail_t (the tail is exact)
    let KA = Math.min(cfg.KA ?? 96, KA_CNT);
    for (const c of this.cand) KA = Math.min(KA, c.p.filter((p) => p < TAIL_T).length);
    KA &= ~3;                // check_a takes candidates 4 at a time
    if (KA < 4) throw new Error("checker: phase A too short");
    this.KA = KA;
    this.pendCap = cfg.pendCap ?? (1 << 20);
    this.ridgeCap = cfg.ridgeCap ?? 8192;
    this.leftCap = cfg.leftCap ?? 4096;
    // the kernel reports p(n) >= ridgeFloor (>= TAIL_T); the host keeps p >= RIDGE_FLOOR.
    // Tests lower it to make the ridge list busy (test/lists_test.mjs)
    this.ridgeFloor = Math.max(TAIL_T, Math.min(RIDGE_FLOOR, cfg.ridgeFloor ?? RIDGE_FLOOR));
    this.bCand = mk(all.byteLength, S | CD); dev.queue.writeBuffer(this.bCand, 0, all);
    this.bC = mk((128 + 15 * KA_MAX) * 4, S | CD);
    this.bHist = mk(15 * HSTRIDE * 4, S | CD | CS);
    // segments per command buffer (cfg.batch): one submit, one encoder; their
    // per-segment inputs go through a staging ring (one slot per segment of a
    // batch) and are copied into place inside the encoder, ahead of their passes
    this.batch = Math.max(1, cfg.batch ?? 1);
    // segments in flight (cfg.depth, default 3 batches): one readback buffer and
    // one ridge/leftover region (see ioLists) each
    this.depth = Math.max(1, cfg.depth ?? Math.max(3, 3 * this.batch));
    // runAll submits whole batches: fewer readback slots than a batch would never
    // submit anything (found by tools/crosscheck.py --fuzz)
    if (this.depth < this.batch) throw new Error(`engine config: depth ${this.depth} < batch ${this.batch}`);
    // io (u32 words): [0, 4) counts + flags, pending from 16, then one region per
    // segment in flight: ridge pairs (2 ridgeCap words), then leftovers (leftCap)
    this.ioListsBase = 16 + this.pendCap;
    this.ioListsWords = 2 * this.ridgeCap + this.leftCap;
    this.bIO = mk((this.ioListsBase + this.depth * this.ioListsWords) * 4, S | CD | CS);
    this.bFirst = mk(FIRST_BINS * 4, S | CD | CS);
    this.bArgs = mk(16, S | GPUBufferUsage.INDIRECT);
    this.bLim = mk(32 * 4, S | CD);
    this.bCnt = mk(16 * 4, S | CD | CS);
    // count30.wgsl (counts and prime gaps): parameters, gap totals + histogram +
    // tail list, one (largest gap, position) row per work-group
    this.gapGroups = Math.ceil(Math.ceil(this.cw32 / 4) / 256);
    this.bGP = mk(16 * 4, S | CD);
    this.bGap = mk(GAP_WORDS * 4, S | CD | CS);
    this.bGR = mk(Math.max(16, 2 * this.gapGroups * 4), S | CS);
    dev.queue.writeBuffer(this.bIO, 0, new Uint32Array(4));
    const chkMod = dev.createShaderModule({ code: this.src.check });
    const cntMod = dev.createShaderModule({ code: this.src.count30 });
    this.pA = dev.createComputePipeline({ layout: "auto", compute: { module: chkMod, entryPoint: "check_a" } });
    this.pB = dev.createComputePipeline({ layout: "auto", compute: { module: chkMod, entryPoint: "check_b" } });
    this.pArgs = dev.createComputePipeline({ layout: "auto", compute: { module: chkMod, entryPoint: "b_args" } });
    this.pCnt = dev.createComputePipeline({ layout: "auto", compute: { module: cntMod, entryPoint: "count30" } });
    const bind = (pipe, entries) => dev.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: entries.map(([i, b]) => ({ binding: i, resource: { buffer: b } })) });
    this.bgA = bind(this.pA, [[0, this.bMap], [2, this.bC], [3, this.bHist], [4, this.bIO]]);
    this.bgB = bind(this.pB, [[0, this.bMap], [1, this.bCand], [2, this.bC], [3, this.bHist], [4, this.bIO], [5, this.bFirst]]);
    this.bgArgs = bind(this.pArgs, [[2, this.bC], [4, this.bIO], [6, this.bArgs]]);
    this.bgCnt = bind(this.pCnt, [[0, this.bMap], [1, this.bLim], [2, this.bCnt], [3, this.bGP], [4, this.bGap], [5, this.bGR]]);
    // per-segment readback: hist | cnt (64 B) | io header (16 B) | large-kernel err (16 B) | first.
    // The ridge and leftover lists are not in it: copying their whole capacity
    // (80 KB) cost ~3 % of the wall time, and they are almost always empty
    // (collect fetches the used part, see readLists)
    const H = 15 * HSTRIDE * 4;
    this.rbLayout = { hist: 0, cnt: H, io: H + 64, err: H + 80, first: H + 96 };
    this.rbLayout.gap = Math.ceil((this.rbLayout.first + FIRST_BINS * 4) / 16) * 16;
    this.rbLayout.grows = this.rbLayout.gap + GAP_WORDS * 4;
    this.rbLayout.size = Math.ceil((this.rbLayout.grows + 2 * this.gapGroups * 4) / 16) * 16;
    const al = (b) => Math.ceil(b / 256) * 256;
    this.slotBytes = al(32 * this.split) + al(96 * 4) + al(16 * 4) + al(32 * 4) + al((128 + 15 * KA_MAX) * 4) + al(32 * 4) + al(16 * 4);
    this.bStage = mk(this.batch * this.slotBytes, CS | CD);
    this.bFirstInit = mk(FIRST_BINS * 4, CS | CD);
    this.maps = new Array(this.depth);
    this.rbs = Array.from({ length: this.depth }, () => dev.createBuffer({ size: this.rbLayout.size, usage: GPUBufferUsage.MAP_READ | CD }));
    dev.queue.writeBuffer(this.bFirstInit, 0, new Uint32Array(FIRST_BINS).fill(0xFFFFFFFF));
    if (cfg.profile) {
      this.qs = dev.createQuerySet({ type: "timestamp", count: 32 });
      this.bQ = mk(32 * 8, GPUBufferUsage.QUERY_RESOLVE | CS);
      this.rbQ = dev.createBuffer({ size: 32 * 8, usage: GPUBufferUsage.MAP_READ | CD });
      this.prof = {};
    }
    this.resetTotals();
  }

  resetTotals() {
    this.res = {
      evens: 0n, primes30: new Array(8).fill(0n), twins30: new Array(3).fill(0n),
      hist: Array.from({ length: 15 }, () => new Float64Array(HSTRIDE)),   // exact below 2^53
      first: new Map(), beyond: [], ridges: [], pending: 0, leftovers: 0,
    };
    if (this.geo) this.M._gb_gaps_begin(this.geo.start);
  }

  /// Encodes the checker of segment k (after encodeMap in the same encoder).
  encodeCheck(enc, k) {
    const { M, dev, geo } = this;
    const seg = geo.segs[k];
    const g = this.geo30(seg);
    const sm30 = Number(seg.segStart % 30n);
    const segEvens = Number(seg.segEvens);
    const C = new Uint32Array(128 + 15 * KA_MAX);
    C.set([this.cw32, this.KA, this.pendCap, 0, sm30, this.ridgeFloor, this.ridgeCap, this.leftCap, 0, TAIL_T, 0, ...this.ioLists(k)]);
    for (let c = 0; c < 8; c++) { C[16 + c] = g.n[c]; C[24 + c] = 9781 - 2 * g.phi[c]; }
    let maxEc = 0, ulo = Infinity, uhi = -Infinity;
    for (let cr = 0; cr < 15; cr++) {
      const e0 = ((2 * cr + 30 - sm30) % 30) / 2;
      const ec = segEvens > e0 ? Math.floor((segEvens - e0 + 14) / 15) : 0;
      maxEc = Math.max(maxEc, ec);
      const cd = this.cand[cr];
      C.set([e0, ec, cd.off, cd.n], 32 + 4 * cr);
      let mask = 0;
      for (let j = 0; j < this.KA; j++) {
        const V = 9781 - 2 * g.phi[cd.k[j]] + 2 * e0;
        if ((V - cd.p[j]) % 30 !== 0) throw new Error("checker: candidate class mismatch");
        const U = (V - cd.p[j]) / 30;
        ulo = Math.min(ulo, U); uhi = Math.max(uhi, U);
        C[128 + KA_MAX * cr + j] = U;
        mask |= 1 << cd.k[j];
      }
      C[96 + cr] = mask;
    }
    if (ulo < 0 || uhi - ulo > 64) throw new Error(`checker: phase-A window span [${ulo}, ${uhi}] out of bounds`);
    C[8] = ulo;
    // kernel form: window bit offset U - 32 floor(u_lo / 32) (< 96) | k << 16
    const ub = 32 * Math.floor(ulo / 32);
    for (let cr = 0; cr < 15; cr++)
      for (let j = 0; j < this.KA; j++) C[128 + KA_MAX * cr + j] = (C[128 + KA_MAX * cr + j] - ub) | (this.cand[cr].k[j] << 16);
    C[3] = Math.ceil(maxEc / BLK_A);
    this.stage(enc, this.bC, C);
    // count30 limits
    const lp = M._gb_malloc(128);
    coreCheck(M, M._gb_count30_limits(seg.segStart, seg.segEnd, seg.qFirst, seg.qOdds, lp));
    const lim = u32view(M, lp, 32).slice();
    this.stage(enc, this.bLim, lim);
    M._gb_free(lp);
    this.stage(enc, this.bGP, this.gapParams(k, g, lim));
    // resets
    enc.clearBuffer(this.bHist); enc.clearBuffer(this.bCnt); enc.clearBuffer(this.bGap);
    enc.clearBuffer(this.bIO, 0, 16);
    enc.copyBufferToBuffer(this.bFirstInit, 0, this.bFirst, 0, FIRST_BINS * 4);
    const run = (pipe, bg, n, indirect, name) => {
      const pass = this.pass(enc, name);
      pass.setPipeline(pipe); pass.setBindGroup(0, bg);
      if (indirect) pass.dispatchWorkgroupsIndirect(this.bArgs, 0); else DenseEngine.dispatch2(pass, n);
      pass.end();
    };
    run(this.pCnt, this.bgCnt, this.gapGroups, false, "count30");   // count30.wgsl: 4 words of every class per invocation
    run(this.pA, this.bgA, C[3], false, "check_a");          // one work-group per block
    run(this.pArgs, this.bgArgs, 1, false, "b_args");
    run(this.pB, this.bgB, 0, true, "check_b");
    const L = this.rbLayout;
    const rb = this.rbs[k % this.depth];
    enc.copyBufferToBuffer(this.bHist, 0, rb, L.hist, 15 * HSTRIDE * 4);
    enc.copyBufferToBuffer(this.bCnt, 0, rb, L.cnt, 64);
    enc.copyBufferToBuffer(this.bIO, 0, rb, L.io, 16);
    enc.copyBufferToBuffer(this.bErr, 0, rb, L.err, 16);
    enc.copyBufferToBuffer(this.bFirst, 0, rb, L.first, FIRST_BINS * 4);
    enc.copyBufferToBuffer(this.bGap, 0, rb, L.gap, GAP_WORDS * 4);
    enc.copyBufferToBuffer(this.bGR, 0, rb, L.grows, 2 * this.gapGroups * 4);
  }

  /// io word offsets [ridge, leftover] of segment k's lists: a region per
  /// segment in flight, so they stay intact until collect(k) has read them.
  /// Segment k + depth, the next user of the region, is submitted only after
  /// collect(k) (runAll; it also reuses k's readback buffer).
  ioLists(k) {
    const r = this.ioListsBase + (k % this.depth) * this.ioListsWords;
    return [r, r + 2 * this.ridgeCap];
  }

  /// The used part of segment k's ridge pairs (nr) and leftovers (nl), read
  /// with one extra copy and map. Called from collect(k) only when the segment
  /// reported entries (p(n) >= ridgeFloor, or no candidate below
  /// MAX_GB_PRIME: rare), with counts within the capacities.
  async readLists(k, nr, nl) {
    const [ro, lo] = this.ioLists(k);
    const bytesR = 2 * nr * 4, bytesL = nl * 4;
    const rb = this.dev.createBuffer({ size: Math.max(16, Math.ceil((bytesR + bytesL) / 16) * 16), usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    try {
      const enc = this.dev.createCommandEncoder();
      if (nr) enc.copyBufferToBuffer(this.bIO, ro * 4, rb, 0, bytesR);
      if (nl) enc.copyBufferToBuffer(this.bIO, lo * 4, rb, bytesR, bytesL);
      this.dev.queue.submit([enc.finish()]);
      await rb.mapAsync(GPUMapMode.READ);
      const buf = rb.getMappedRange();
      const lists = { ridge: new Uint32Array(buf, 0, 2 * nr).slice(), left: new Uint32Array(buf, bytesR, nl).slice() };
      rb.unmap();
      return lists;
    } finally { rb.destroy(); }
  }

  /// Encodes and submits segment k and starts mapping its readback buffer
  /// (the map resolves when the GPU is done; collect(k) awaits it).
  submit(k, k1 = k + 1) {
    if (k1 - k > this.batch) throw new Error("submit: more segments than staging slots");
    const enc = this.dev.createCommandEncoder();
    this.inSubmit = true;
    for (let j = k; j < k1; j++) {
      this.stageOff = (j - k) * this.slotBytes;
      this.encodeMap(enc, j);
      this.encodeCheck(enc, j);
    }
    this.inSubmit = false;
    if (this.qs) {
      enc.resolveQuerySet(this.qs, 0, 2 * this.qn, this.bQ, 0);
      enc.copyBufferToBuffer(this.bQ, 0, this.rbQ, 0, 2 * this.qn * 8);
    }
    this.dev.queue.submit([enc.finish()]);
    for (let j = k; j < k1; j++) this.maps[j % this.depth] = this.rbs[j % this.depth].mapAsync(GPUMapMode.READ);
  }

  /// Per-segment input: written to this segment's staging slot (queue order:
  /// before this command buffer, after every earlier one), copied into place
  /// in the encoder (encoder order: after the previous segment's passes).
  stage(enc, dst, data) {
    const off = this.stageOff;
    this.dev.queue.writeBuffer(this.bStage, off, data);
    enc.copyBufferToBuffer(this.bStage, off, dst, 0, data.byteLength);
    this.stageOff = off + Math.ceil(data.byteLength / 256) * 256;
  }

  /// Encodes + submits segment k, then collects its results (fail closed).
  async runSegment(k) {
    const t0 = performance.now();
    this.qn = 0; this.qnames = [];
    this.submit(k);
    const t1 = performance.now();
    await this.collect(k);
    if (this.qs) {
      const P = this.prof;
      P.host = (P.host ?? 0) + (t1 - t0);
      P.wall = (P.wall ?? 0) + (performance.now() - t0);
      await this.rbQ.mapAsync(GPUMapMode.READ);
      const t = new BigUint64Array(this.rbQ.getMappedRange()).slice();
      this.rbQ.unmap();
      for (let i = 0; i < this.qn; i++) P[this.qnames[i]] = (P[this.qnames[i]] ?? 0) + Number(t[2 * i + 1] - t[2 * i]) / 1e6;
    }
  }

  /// Runs segments [k0, n) with up to `depth` segments in flight: the host
  /// prepares and submits the next segments while the GPU works, and results
  /// are collected in order. Queue order makes the shared GPU buffers safe
  /// (each segment copies its results out before the next one resets them);
  /// only the readback buffers are per segment.
  /// @param onSegment  awaited after segment k is collected; false stops the run
  ///                   (segments already in flight are still collected).
  /// @return true if every segment ran, false if stopped.
  async runAll(onSegment = () => true, k0 = 0) {
    this.idle = false;                // stays false if anything throws: never reused
    const n = this.geo.segs.length;
    let next = k0, stop = false;
    for (let k = k0; k < n; k++) {
      while (!stop && next < n) {
        const e = Math.min(n, next + this.batch);
        if (e > k + this.depth) break;
        this.submit(next, e);
        next = e;
      }
      if (k >= next) {
        if (!stop) throw new Error(`runAll: segment ${k} was never submitted`);   // fail closed, never a partial result
        break;
      }
      await this.collect(k);
      if (!stop && (await onSegment(k)) === false) stop = true;
      if (stop && k + 1 >= next) { this.idle = true; return k === n - 1; }
    }
    this.idle = true;
    return !stop;
  }

  async collect(k) {
    const { M, geo, res } = this;
    const seg = geo.segs[k];
    const L = this.rbLayout;
    const rb = this.rbs[k % this.depth];
    await this.maps[k % this.depth];
    const buf = rb.getMappedRange();
    const u = (o, n) => new Uint32Array(buf, o, n).slice();
    const ioh = u(L.io, 4);
    const err = ioh[3] | u(L.err, 1)[0];
    const npend = ioh[0];
    const hist = u(L.hist, 15 * HSTRIDE), cnt = u(L.cnt, 11), first = u(L.first, FIRST_BINS);
    const gap = u(L.gap, GAP_WORDS), grows = u(L.grows, 2 * this.gapGroups);
    const nr = ioh[1], nl = ioh[2];
    rb.unmap();
    if (err) throw new Error(`segment ${k}: device error flags ${err} (pending ${npend}, ridge ${nr}, leftover ${nl})`);
    // within the capacities here: an overflow set an error flag above
    const { ridge, left } = nr || nl ? await this.readLists(k, nr, nl) : { ridge: [], left: [] };
    this.collectGaps(k, seg, gap, grows);
    // counts
    for (let c = 0; c < 8; c++) res.primes30[c] += BigInt(cnt[c]);
    for (let j = 0; j < 3; j++) res.twins30[j] += BigInt(cnt[8 + j]);
    let resolved = 0;
    for (let cr = 0; cr < 15; cr++)
      for (let j = 0; j < this.cand[cr].n; j++) { const v = hist[cr * HSTRIDE + j]; if (v) { res.hist[cr][j] += v; resolved += v; } }
    // first occurrences (p >= tail_t)
    for (let b = TAIL_T >> 1; b < FIRST_BINS; b++)
      if (first[b] !== 0xFFFFFFFF) this.noteFirst(2 * b + 1, seg.segStart + 2n * BigInt(first[b]));
    // ridges
    for (let i = 0; i < ridge.length; i += 2) {
      const e = ridge[i], cr = ridge[i + 1] >>> 16, j = ridge[i + 1] & 0xFFFF;
      this.noteRidge(seg.segStart + 2n * BigInt(e), this.cand[cr].p[j]);
    }
    // leftovers: CPU, exact
    for (const e of left) {
      const n = seg.segStart + 2n * BigInt(e);
      const p = Number(M._gb_pn_exact(n));
      if (p === 0) throw new Error(`GOLDBACH COUNTEREXAMPLE candidate n = ${n}`);
      this.addCpu(n, p);
    }
    res.pending += npend;
    res.leftovers += left.length;
    res.evens += seg.segEvens;
    if (BigInt(resolved + left.length) !== seg.segEvens)
      throw new Error(`segment ${k}: ${resolved} + ${left.length} evens accounted, ${seg.segEvens} in the segment`);
  }

  /// count30.wgsl parameters of segment k (GP): cw32, the smallest counted
  /// position, segment 0's lower bound for a gap's upper prime (the run starts
  /// at start + 1), the tail capacity, phi by class. g = geo30(seg), lim = its
  /// count30 limits.
  gapParams(k, g, lim) {
    let phic = 0, posLo = 0xFFFFFFFF;
    for (let c = 0; c < 8; c++) {
      phic |= g.phi[c] << (4 * c);
      if (lim[2 * c] < lim[2 * c + 1]) posLo = Math.min(posLo, 30 * lim[2 * c] + 2 * g.phi[c]);
    }
    const seg = this.geo.segs[k];
    const minUpper = k === 0 ? Number(this.geo.start + 1n - seg.qFirst) : 0;
    return new Uint32Array([this.cw32, 0, 0, posLo >>> 0, minUpper, GAP_TCAP, phic >>> 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
  }

  /// count30's gap output of segment k -> the core's run accumulator (core.cpp
  /// gb_gaps_segment: the same code as the CPU verifier's).
  collectGaps(k, seg, gap, grows) {
    const { M } = this;
    if (gap[2]) throw new Error(`segment ${k}: count30 gap error flags ${gap[2]}`);
    if (gap[3] > GAP_TCAP) throw new Error(`segment ${k}: count30 gap tail list overflow`);
    let gmax = 0, gpos = 0;
    for (let i = 0; i < grows.length; i += 2) {
      const g = grows[i], q = grows[i + 1];
      if (g > gmax || (g === gmax && g && q < gpos)) { gmax = g; gpos = q; }
    }
    const w = M._gb_malloc((5 + GAP_HBINS + 2 * GAP_TCAP) * 4);
    try {
      const v = new Uint32Array(M.HEAPU32.buffer, w, 5 + GAP_HBINS + 2 * GAP_TCAP);   // live (u32view copies)
      v[0] = gap[0] ? ~gap[0] >>> 0 : 0xFFFFFFFF;          // max(~first) -> first
      v[1] = gap[1] ? gap[1] - 1 : 0;                       // max(last + 1) -> last
      v[2] = gmax; v[3] = gpos; v[4] = gap[3];
      v.set(gap.subarray(4, 4 + GAP_HBINS), 5);
      v.set(gap.subarray(4 + GAP_HBINS, 4 + GAP_HBINS + 2 * gap[3]), 5 + GAP_HBINS);
      if (!M._gb_gaps_segment(seg.qFirst, w)) throw new Error(`segment ${k}: ${M.UTF8ToString(M._gb_error())}`);
    } finally { M._gb_free(w); }
  }

  noteFirst(p, n) {
    const f = this.res.first.get(p);
    if (f === undefined || n < f) this.res.first.set(p, n);
  }
  noteRidge(n, p) {
    if (p >= RIDGE_FLOOR) this.res.ridges.push({ n, p });
  }
  addCpu(n, p) {
    const cr = Number(n % 30n) / 2;
    const j = this.cand[cr].p.indexOf(p);
    if (j >= 0) {
      this.res.hist[cr][j] += 1;
      if (p >= TAIL_T) { this.noteFirst(p, n); this.noteRidge(n, p); }
    } else {
      this.res.beyond.push({ n, p });
      this.noteRidge(n, p);
    }
  }

  /// Exact run result (call after every segment was collected).
  finish() {
    const { M, geo, res } = this;
    const p30 = new BigInt64Array(res.primes30), t30 = new BigInt64Array(res.twins30);
    const pp = M._gb_malloc(64), tp = M._gb_malloc(32);
    new BigInt64Array(M.HEAPU8.buffer, pp, 8).set(p30);
    new BigInt64Array(M.HEAPU8.buffer, tp, 3).set(t30);
    M._gb_finish_counts(geo.start, geo.limit, pp, tp);
    const primes30 = Array.from(new BigInt64Array(M.HEAPU8.buffer, pp, 8));
    const twins30 = Array.from(new BigInt64Array(M.HEAPU8.buffer, tp, 3));
    M._gb_free(pp); M._gb_free(tp);
    // p(n) histogram by prime (all classes), tail by prime
    const byP = new Map();
    for (let cr = 0; cr < 15; cr++)
      this.cand[cr].p.forEach((p, j) => { const v = res.hist[cr][j]; if (v) byP.set(p, (byP.get(p) ?? 0) + v); });
    for (const b of res.beyond) byP.set(b.p, (byP.get(b.p) ?? 0) + 1);
    let tailEvens = 0, pmax = 0;
    for (const [p, v] of byP) if (p >= TAIL_T) { tailEvens += v; pmax = Math.max(pmax, p); }
    return {
      start: geo.start, limit: geo.limit, evens: res.evens,
      primes: primes30.reduce((a, b) => a + b, 0n), twins: twins30.reduce((a, b) => a + b, 0n),
      primes30, twins30, tailT: TAIL_T, tailEvens, pmax,
      histByP: [...byP.entries()].sort((a, b) => a[0] - b[0]),
      histByClass: res.hist.map((h, cr) => this.cand[cr].p.map((p, j) => [p, h[j]]).filter((x) => x[1])),
      first: [...res.first.entries()].sort((a, b) => a[0] - b[0]),
      beyond: res.beyond, ridgeFloor: RIDGE_FLOOR,
      ridges: [...res.ridges].sort((a, b) => (a.n < b.n ? -1 : a.n > b.n ? 1 : 0)),
      gaps: JSON.parse(M.UTF8ToString(M._gb_gaps_finish(geo.limit))),   // core.cpp: as the CPU verifier's, byte for byte
      pending: res.pending, leftovers: res.leftovers,
    };
  }

  geo30(seg) {
    const { M } = this;
    const p = M._gb_malloc(64);
    M._gb_geo30(seg.qFirst, seg.qOdds, p);
    const v = u32view(M, p, 16);
    M._gb_free(p);
    return { phi: [...v.slice(0, 8)], n: [...v.slice(8, 16)] };
  }

  /// Begins a compute pass; with cfg.profile, brackets it with timestamps
  /// under `name` (needs the timestamp-query feature).
  pass(enc, name) {
    if (!this.qs) return enc.beginComputePass();
    const i = this.qn++;
    this.qnames[i] = name;
    return enc.beginComputePass({ timestampWrites: { querySet: this.qs, beginningOfPassWriteIndex: 2 * i, endOfPassWriteIndex: 2 * i + 1 } });
  }

  static dispatch2(pass, n) {
    pass.dispatchWorkgroups(Math.min(n, 65535), Math.ceil(n / 65535));
  }

  /// Encodes segment k (map only: tile, harvest at batch starts, large).
  encodeMap(enc, k) {
    const { M, dev, geo } = this;
    if (!this.inSubmit) this.stageOff = 0;           // a caller encoding one segment per submit (tests)
    const seg = geo.segs[k];
    const g = this.geo30(seg);
    // tile params
    coreCheck(M, M._gb_tile_first_bits(seg.qFirst, seg.qOdds, this.split, this.fbP));
    this.stage(enc, this.bFb, new Uint32Array(M.HEAPU32.buffer, this.fbP, 8 * this.split));
    const P = new Uint32Array(96);
    P.set([this.cw32, this.tpc, this.ng, this.split, this.ownEnd, this.maskCount]);
    P.set(g.n, 8);
    P.set(this.gdesc, 16);
    const yP = M._gb_malloc(64);
    for (let c = 0; c < 8; c++) {
      const n = M._gb_group_offsets(seg.qFirst + 2n * BigInt(g.phi[c]), 4096, yP);
      if (n !== this.ng) throw new Error("mask group layout mismatch");
      P.set(u32view(M, yP, n), 32 + 8 * c);
    }
    M._gb_free(yP);
    this.stage(enc, this.bP, P);
    // harvest at the start of each batch
    const jb = this.K ? k % this.K : 0;
    if (this.K && jb === 0) {
      const nseg = Math.min(this.K, geo.segs.length - k);
      const last = geo.segs[k + nseg - 1];
      const H = new Uint32Array(16);
      H.set([this.ha, this.hb, Number(geo.delta), Number(geo.qOddsFull), nseg, Number(last.qOdds),
             Number(seg.qFirst % 15015n), this.capg, Number(geo.delta % 15015n)]);
      if (this.lite) {
        const wend = BigInt(nseg - 1) * geo.delta + last.qOdds;
        if (wend >= 2n ** 32n) throw new Error("hit-list batch too long for harvest_lite (its offsets are u32): lower K");
        H.set([Number(seg.qFirst >> 32n), Number(seg.qFirst & 0xFFFFFFFFn), Number(wend >> 32n), Number(wend & 0xFFFFFFFFn),
               Math.floor(this.ha / 512)], 9);
      }
      this.stage(enc, this.bH, H);
      const pass = this.pass(enc, 'harvest');
      pass.setPipeline(this.pHarv); pass.setBindGroup(0, this.bgHarv);
      DenseEngine.dispatch2(pass, this.G);
      pass.end();
    }
    // tile
    {
      const pass = this.pass(enc, 'tile');
      pass.setPipeline(this.pTile); pass.setBindGroup(0, this.bgTile);
      DenseEngine.dispatch2(pass, 8 * this.tpc);
      pass.end();
    }
    // large + fused apply
    const phaseClass = new Uint32Array(15).fill(15);
    for (let c = 0; c < 8; c++) phaseClass[g.phi[c]] = c;
    const L = new Uint32Array(32);
    L.set([this.cw32, Number(seg.qOdds), Number(geo.delta), Number(seg.qFirst % 30030n), Number(seg.qFirst % 15015n),
           this.pa, this.pb, this.pone, this.K ? this.G : 0, this.capg, jb,
           Math.floor(this.pa / 512), Number(seg.qFirst >> 32n), Number(seg.qFirst & 0xFFFFFFFFn), Math.log2(this.capg || 1)]);
    L.set(phaseClass, 16);
    this.stage(enc, this.bL, L);
    {
      const pass = this.pass(enc, 'large');
      pass.setPipeline(this.pLarge); pass.setBindGroup(0, this.bgLarge);
      const groups = this.lite
        ? Math.ceil(this.pb / 512) - Math.floor(this.pa / 512)                 // one work-group per 512-prime group
        : Math.ceil(Math.ceil((this.pb - this.pa) / this.cfg.ppi) / this.cfg.wgLarge);
      DenseEngine.dispatch2(pass, groups || 1);
      pass.end();
    }
  }

  /// Frees every GPU buffer and WASM allocation of this run.
  destroy() {
    for (const v of Object.values(this)) if (v && typeof v === "object" && typeof v.destroy === "function" && v !== this.dev) v.destroy();
    if (this.fbP) { this.M._gb_free(this.fbP); this.fbP = 0; }
  }

  /// Reads the device error flags (0 = none).
  async errors() {
    const rb = this.dev.createBuffer({ size: 16, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    const enc = this.dev.createCommandEncoder();
    enc.copyBufferToBuffer(this.bErr, 0, rb, 0, 16);
    this.dev.queue.submit([enc.finish()]);
    await rb.mapAsync(GPUMapMode.READ);
    const e = new Uint32Array(rb.getMappedRange())[0];
    rb.unmap(); rb.destroy();
    return e;
  }
}

/// Runs slices one after another on one device. The worker and the benchmark
/// CI both use it, so a branch that changes how a slice is set up (or what is
/// kept between slices) is measured end to end.
export class SliceRunner {
  /// @param segments  segment sizes to try, largest first (the first that fits the device)
  constructor(M, device, src, cfg, segments) {
    Object.assign(this, { M, dev: device, src, cfg, segments });
  }

  /// A prepared engine for [start, limit]; give it back with close(). The
  /// engine of the previous slice is re-targeted when the layout allows it
  /// (full mode: the 437 MB of primes, buffers and pipelines stay).
  async open(start, limit) {
    const kept = this.kept;
    this.kept = null;
    if (kept) {
      let ok = false;
      try { ok = await kept.reset(start, limit); } catch { ok = false; }
      if (ok) return kept;
      kept.destroy();
    }
    let lastErr = null;
    for (const seg of this.segments) {
      const eng = new DenseEngine(this.M, this.dev, this.src, this.cfg);
      try {
        return await eng.prepare(start, limit, seg);
      } catch (e) { lastErr = e; eng.destroy(); }
    }
    throw lastErr;
  }

  /// Done with an engine from open() (its results were taken or dropped).
  close(eng) {
    if (eng.idle && !this.kept) this.kept = eng;   // a run that threw is never reused
    else eng.destroy();
  }

  /// Frees whatever the runner keeps between slices.
  destroy() {
    this.kept?.destroy();
    this.kept = null;
  }
}
