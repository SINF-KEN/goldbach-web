// goldbach-web, lite engine: large primes without per-prime state (phones and
// low-memory devices). Same marks as large.wgsl, but the primes come from the
// 1-byte half-gap stream (109 MB for every prime below 2^32 instead of 437) and
// each prime's first multiple is recomputed every segment, exactly, with
// 32-bit long division (q_first mod p): no 64-bit integers, no floats.
//
// One work-group of 128 invocations decodes one group of 512 consecutive primes
// (gap word w holds the half-gaps of primes 4w .. 4w + 3; the half-gap at a group
// start is 0 and bases[g] is the group's first prime): a workgroup prefix sum.

const WHL_MISS: u32 = 0xFFFFu;
const T_INV: u32 = 0u;
const T_IDX: u32 = 30030u;
const T_STEP: u32 = 60060u;
const T_CP: u32 = 65820u;
const WHL_N: u32 = 5760u;
const ERR_ENTRY: u32 = 1u;
const GROUP: u32 = 512u;

// L: [0] cw32 [1] q_odds [2] delta [3] qfw = q_first mod 30030 [4] qf15
//    [5] pa [6] pb [7] pone (first index with p >= q_odds)
//    [8] apply: G (0 = no list) [9] capg [10] j (segment in batch)
//    [11] g0 = first gap group of the packed range [12] q_first hi [13] q_first lo [14] log2 capg
//    [16 + ph] class of dense phase ph
@group(0) @binding(0) var<storage, read_write> map: array<atomic<u32>>;
@group(0) @binding(1) var<storage, read> gaps: array<u32>;
@group(0) @binding(2) var<storage, read> bases: array<u32>;
@group(0) @binding(3) var<storage, read> tabs: array<u32>;
@group(0) @binding(4) var<storage, read> L: array<u32>;
@group(0) @binding(5) var<storage, read> lists: array<u32>;
@group(0) @binding(6) var<storage, read> gcnt: array<u32>;
@group(0) @binding(7) var<storage, read_write> err: array<atomic<u32>>;

var<workgroup> scan: array<u32, 128>;

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

// Decodes this invocation's 4 primes of group g into pv; workgroup-uniform call.
fn decode(g: u32, lid: u32) -> vec4<u32> {
  let w = gaps[g * (GROUP / 4u) + lid];
  let h = vec4<u32>(w & 0xFFu, (w >> 8u) & 0xFFu, (w >> 16u) & 0xFFu, w >> 24u);
  scan[lid] = h.x + h.y + h.z + h.w;
  workgroupBarrier();
  for (var d = 1u; d < 128u; d <<= 1u) {          // inclusive Hillis-Steele scan
    var v = scan[lid];
    if (lid >= d) { v += scan[lid - d]; }
    workgroupBarrier();
    scan[lid] = v;
    workgroupBarrier();
  }
  let excl = select(0u, scan[lid - 1u], lid > 0u);
  workgroupBarrier();
  let p0 = bases[g] + 2u * (excl + h.x);
  let p1 = p0 + 2u * h.y;
  let p2 = p1 + 2u * h.z;
  return vec4<u32>(p0, p1, p2, p1 + 2u * h.z + 2u * h.w);
}

@compute @workgroup_size(128)
fn large_sieve(@builtin(workgroup_id) wid: vec3<u32>,
               @builtin(num_workgroups) nwg: vec3<u32>,
               @builtin(local_invocation_index) lid: u32) {
  let wg = wid.x + wid.y * nwg.x;
  let gid = wg * 128u + lid;
  let gsize = nwg.x * nwg.y * 128u;
  let qo = L[1];
  // ---- fused hit-list apply ----
  let G = L[8];
  if (G != 0u) {
    let capg = L[9];
    let jseg = L[10];
    let nsl = G * capg;
    for (var s = gid; s < nsl; s += gsize) {
      let g = s >> L[14];                  // capg = 2^L[14] (host)
      let k = s & (capg - 1u);
      if (k < gcnt[jseg * G + g]) {
        let o = lists[jseg * nsl + s];
        if (o < qo) { clear_bit(o); } else { atomicOr(&err[0], ERR_ENTRY); }
      }
    }
  }
  // ---- packed primes of group g0 + wg ----
  let g = L[11] + wg;
  let pv = decode(g, lid);
  let pa = L[5];
  let pb = L[6];
  let pone = L[7];
  for (var u = 0u; u < 4u; u++) {
    let i = g * GROUP + 4u * lid + u;
    if (i < pa || i >= pb) { continue; }
    let p = pv[u];
    // first odd multiple at or after q_first (p^2 < q_first: host-checked)
    let r = modq(L[12], L[13], p);
    var t = select(p - r, 0u, r == 0u);
    var s: u32;
    if ((t & 1u) != 0u) { s = (t >> 1u) + (p >> 1u) + 1u; } else { s = t >> 1u; }   // (t + p) / 2 without overflow
    if (i >= pone) {                                   // p >= q_odds: at most one mark
      if (s < qo && cp15015((L[4] + 2u * (s % 15015u)) % 15015u)) { clear_bit(s); }
      continue;
    }
    var m = (L[3] + 2u * (s % 15015u)) % 30030u;
    m = (m * tabs[T_INV + p % 30030u]) % 30030u;
    var bit = s;
    var ok = bit < qo;
    while (ok && tabs[T_IDX + m] == WHL_MISS) {
      if (qo - bit <= p) { ok = false; break; }
      bit += p;
      m += 2u;
      if (m >= 30030u) { m -= 30030u; }
    }
    if (ok) {
      var w = tabs[T_IDX + m];
      loop {
        clear_bit(bit);
        let step = p * tabs[T_STEP + w];              // <= 11 p < 2^32 (host: 12 q_odds < 2^32)
        if (qo - bit <= step) { break; }
        bit += step;
        w += 1u;
        if (w == WHL_N) { w = 0u; }
      }
    }
  }
}
