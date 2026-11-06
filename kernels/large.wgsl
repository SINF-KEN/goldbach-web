// goldbach-web: large primes of the mod-30 class map (WebGPU port of
// sieve_large_packed_hl and hl_harvest, kernels/goldbach.cl).
//
// Exactness without 64-bit integers:
//   * packed primes [pa, pb) (split <= p < q_odds): s = map bit of the first
//     odd multiple of p at or after q_first (< p), recomputed every segment
//     from q_first by modq (exact, tested: test/modq_test.mjs). A per-prime
//     state carried across segments cost 12 bytes of memory traffic per prime
//     and segment (read p, read and write the state): the pass was bound by it;
//   * hit-list primes [ha, hb) (p >= q_odds): hstate = (j, off), the first
//     odd multiple at or after the batch start as segment j of the batch and
//     bit offset off < delta; the walk (harvest.wgsl) leaves the next batch's
//     state. Initial states: harvest.wgsl states, checked against web/core.
//
// Marks are cleared in class coordinates: dense bit b (q = q_first + 2 b) is
// bit t = b / 15 of class phase_class[b mod 15]. Only q coprime to 30 are
// marked (multipliers coprime to 30030, or the CP15015 test), so the class
// always exists.

const NONE: u32 = 0xFFFFFFFFu;
const WHL_MISS: u32 = 0xFFFFu;
// tabs: [0, 30030) WHL_INV, [30030, 60060) WHL_IDX, [60060, 65820) WHL_STEP,
//       [65820, 66290) CP15015 bit table
const T_INV: u32 = 0u;
const T_IDX: u32 = 30030u;
const T_STEP: u32 = 60060u;
const T_CP: u32 = 65820u;
const WHL_N: u32 = 5760u;
const ERR_ENTRY: u32 = 1u;
const ERR_OVERFLOW: u32 = 2u;

override WG_LARGE: u32 = 128u;
override PPI: u32 = 4u;            // packed primes per invocation

// L (large-kernel params):
//   [0] cw32 [1] q_odds [2] delta [3] qfw = q_first mod 30030 [4] qf15
//   [5] pa [6] pb [7] pone (first packed index with p >= q_odds)
//   [8] apply: G (harvest groups, 0 = no list) [9] capg [10] j (segment in batch)
//   [12] q_first hi [13] q_first lo [14] log2 capg
//   [16 + ph] class of dense phase ph (15 = not stored), ph < 15
@group(0) @binding(0) var<storage, read_write> map: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> primes: array<u32>;
@group(0) @binding(3) var<storage, read> tabs: array<u32>;
@group(0) @binding(4) var<storage, read> L: array<u32>;
@group(0) @binding(5) var<storage, read> lists: array<u32>;
@group(0) @binding(6) var<storage, read> gcnt: array<u32>;
@group(0) @binding(7) var<storage, read_write> err: array<atomic<u32>>;

fn clear_bit(b: u32) {
  let t = b / 15u;
  let k = L[16u + (b - 15u * t)];
  let a = k * (L[0] * 32u) + t;
  let m = 1u << (a & 31u);
  // bits only go 1 -> 0: a load that sees the bit clear is final, and ~3/4 of
  // these marks hit q with a factor below the split, cleared by the tile sieve
  if ((atomicLoad(&map[a >> 5u]) & m) != 0u) { atomicAnd(&map[a >> 5u], ~m); }
}

fn cp15015(x: u32) -> bool {
  return ((tabs[T_CP + (x >> 5u)] >> (x & 31u)) & 1u) != 0u;
}

// ---- modq --------------------------------------------------------------------
// (hi 2^32 + lo) mod p, exact for odd p >= 36000 (all primes the kernels see):
// four 16-bit digits; each quotient digit from an f32 estimate biased low by 1
// (the estimate is within 0.03), then the exact 64-bit remainder y - q p
// (q < 2^16: products of 16-bit halves) is reduced by two conditional
// subtractions of p. No integer division, no data-dependent loop.
fn modq_digit(R: u32, c: u32, p: u32, pinv: f32) -> u32 {
  // y = R 2^16 + c (R < p), as (yl, yh)
  let yl = (R << 16u) | c;
  let yh = R >> 16u;
  // y / p < 2^16 to within 0.03 (f32 relative error < 6 2^-24): q = estimate - 1
  // is floor(y / p) - {0, 1, 2}, never above it
  let q = u32(max((f32(R) * 65536.0 + f32(c)) * pinv - 1.0, 0.0));
  // q p as (lo, hi): q < 2^16, p = ph 2^16 + pl
  let a = q * (p & 0xFFFFu);
  let b = q * (p >> 16u);
  let lo = a + (b << 16u);
  let hi = (b >> 16u) + select(0u, 1u, lo < a);
  // d = y - q p in [0, 3p): two exact conditional subtractions of p
  var dl = yl - lo;
  var dh = yh - hi - select(0u, 1u, yl < lo);
  for (var k = 0u; k < 2u; k++) {
    let ge = dh != 0u || dl >= p;
    dh -= select(0u, select(0u, 1u, dl < p), ge);
    dl -= select(0u, p, ge);
  }
  return dl;
}
fn modq(hi: u32, lo: u32, p: u32) -> u32 {
  let pinv = 1.0 / f32(p);
  var r = modq_digit(0u, hi >> 16u, p, pinv);
  r = modq_digit(r, hi & 0xFFFFu, p, pinv);
  r = modq_digit(r, lo >> 16u, p, pinv);
  return modq_digit(r, lo & 0xFFFFu, p, pinv);
}
// ---- end modq ------------------------------------------------------------------

@compute @workgroup_size(WG_LARGE)
fn large_sieve(@builtin(global_invocation_id) gid3: vec3<u32>,
               @builtin(num_workgroups) nwg: vec3<u32>) {
  let gid = gid3.x + gid3.y * nwg.x * WG_LARGE;
  let gsize = nwg.x * nwg.y * WG_LARGE;
  let qo = L[1];
  // ---- fused hit-list apply: this segment's regions --------------------
  let G = L[8];
  if (G != 0u) {
    let capg = L[9];
    let j = L[10];
    let nsl = G * capg;
    for (var s = gid; s < nsl; s += gsize) {
      let g = s >> L[14];                  // capg = 2^L[14] (host)
      let k = s & (capg - 1u);
      if (k < gcnt[j * G + g]) {
        let o = lists[j * nsl + s];
        if (o < qo) { clear_bit(o); } else { atomicOr(&err[0], ERR_ENTRY); }
      }
    }
  }
  // ---- packed primes ------------------------------------------------------
  let pa = L[5];
  let pb = L[6];
  let pone = L[7];
  let i0 = pa + gid * PPI;
  for (var u = 0u; u < PPI; u++) {
    let i = i0 + u;
    if (i >= pb) { break; }
    let p = primes[i];
    // first odd multiple at or after q_first (p^2 < q_first, p >= 36000 for
    // modq: host-checked), as a map bit: t / 2, or (t + p) / 2 for odd t
    let r = modq(L[12], L[13], p);
    let t = select(p - r, 0u, r == 0u);
    let s = select((t >> 1u) + (p >> 1u) + 1u, t >> 1u, (t & 1u) == 0u);
    if (i >= pone) {                       // p >= q_odds: at most one mark (s < p)
      if (s < qo && cp15015((L[4] + 2u * s) % 15015u)) { clear_bit(s); }
    } else {
      // multiplier of the first odd multiple, mod 30030, then the first
      // multiplier coprime to 30030 (as mark_multiples_whl)
      var m = (L[3] + 2u * (s % 15015u)) % 30030u;
      m = (m * tabs[T_INV + p % 30030u]) % 30030u;
      var bit = s;
      var ok = true;
      while (tabs[T_IDX + m] == WHL_MISS) {
        if (qo - bit <= p) { ok = false; break; }
        bit += p;
        m += 2u;
        if (m >= 30030u) { m -= 30030u; }
      }
      if (ok && bit < qo) {
        var w = tabs[T_IDX + m];
        loop {
          clear_bit(bit);
          let step = p * tabs[T_STEP + w];       // <= 11 p < 2^32 (host: 12 q_odds < 2^32)
          if (qo - bit <= step) { break; }
          bit += step;
          w += 1u;
          if (w == WHL_N) { w = 0u; }
        }
      }
    }
  }
}

