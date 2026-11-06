// goldbach-web, lite engine: hit-list harvest without per-prime state (see
// harvest.wgsl for the walk). Each batch recomputes every prime's first odd
// multiple from the batch's q_first exactly: X = (max(p^2, first odd multiple
// >= q_first) - q_first) / 2 with 32-bit long division and 64-bit values built
// from u32 pairs. Primes come from the half-gap stream, 512 per group, one
// group per work-group step (grid-stride over the groups of [a, b)).

const NONE: u32 = 0xFFFFFFFFu;
const T_CP: u32 = 65820u;
const HL_MAX_K: u32 = 128u;
const ERR_OVERFLOW: u32 = 2u;
const GROUP: u32 = 512u;

// H: [0] a [1] b [2] delta [3] q_odds_full [4] nseg [5] q_odds_tail
//    [6] qf15 (batch q_first mod 15015) [7] capg [8] dm = delta mod 15015
//    [9] batch q_first hi [10] lo [11] wend hi [12] wend lo [13] first gap group of [a, b)
@group(0) @binding(0) var<storage, read> gaps: array<u32>;
@group(0) @binding(1) var<storage, read> bases: array<u32>;
@group(0) @binding(2) var<storage, read_write> hlists: array<u32>;
@group(0) @binding(3) var<storage, read_write> hgcnt: array<u32>;
@group(0) @binding(4) var<storage, read_write> herr: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read> H: array<u32>;
@group(0) @binding(6) var<storage, read> htabs: array<u32>;

var<workgroup> lpos: array<atomic<u32>, HL_MAX_K>;
var<workgroup> scan: array<u32, 128>;

fn qo_of(j: u32) -> u32 { return select(H[3], H[5], j == H[4] - 1u); }

fn emit(j: u32, off: u32, grp: u32, ngrp: u32) {
  let idx = atomicAdd(&lpos[j], 1u);
  if (idx < H[7]) { hlists[(j * ngrp + grp) * H[7] + idx] = off; }
}

// 32 x 32 -> 64 bit product as (lo, hi), exact.
fn mulw(a: u32, b: u32) -> vec2<u32> {
  let a0 = a & 0xFFFFu; let a1 = a >> 16u; let b0 = b & 0xFFFFu; let b1 = b >> 16u;
  let p00 = a0 * b0; let p01 = a0 * b1; let p10 = a1 * b0; let p11 = a1 * b1;
  let mid = (p00 >> 16u) + (p01 & 0xFFFFu) + (p10 & 0xFFFFu);
  return vec2<u32>((p00 & 0xFFFFu) | (mid << 16u), p11 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u));
}
// (lo, hi) comparisons and subtraction
fn ge64(a: vec2<u32>, b: vec2<u32>) -> bool { return a.y > b.y || (a.y == b.y && a.x >= b.x); }
fn sub64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
  return vec2<u32>(a.x - b.x, a.y - b.y - select(0u, 1u, a.x < b.x));
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

fn decode(g: u32, lid: u32) -> vec4<u32> {
  let w = gaps[g * (GROUP / 4u) + lid];
  let h = vec4<u32>(w & 0xFFu, (w >> 8u) & 0xFFu, (w >> 16u) & 0xFFu, w >> 24u);
  scan[lid] = h.x + h.y + h.z + h.w;
  workgroupBarrier();
  for (var d = 1u; d < 128u; d <<= 1u) {
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
  return vec4<u32>(p0, p1, p2, p2 + 2u * h.w);
}

@compute @workgroup_size(128)
fn harvest(@builtin(workgroup_id) wid: vec3<u32>,
           @builtin(num_workgroups) nwg: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
  let grp = wid.x + wid.y * nwg.x;
  let ngrp = nwg.x * nwg.y;
  if (lid < HL_MAX_K) { atomicStore(&lpos[lid], 0u); }
  workgroupBarrier();
  let a = H[0];
  let b = H[1];
  let delta = H[2];
  let dinv = 1.0 / f32(delta);
  let nseg = H[4];
  let qf = vec2<u32>(H[10], H[9]);
  let wend = vec2<u32>(H[12], H[11]);
  let g0 = H[13];
  let g1 = (b + GROUP - 1u) / GROUP;
  for (var g = g0 + grp; g < g1; g += ngrp) {             // uniform per work-group
    let pv = decode(g, lid);
    for (var u = 0u; u < 4u; u++) {
      let i = g * GROUP + 4u * lid + u;
      if (i < a || i >= b) { continue; }
      let p = pv[u];
      // X = (max(p^2, first odd multiple >= q_first) - q_first) / 2
      var X: vec2<u32>;
      let pp = mulw(p, p);
      if (ge64(pp, qf)) {
        let d = sub64(pp, qf);                            // even: p^2 and q_first odd
        X = vec2<u32>((d.x >> 1u) | (d.y << 31u), d.y >> 1u);
      } else {
        let r = modq(qf.y, qf.x, p);
        let t = select(p - r, 0u, r == 0u);
        X = vec2<u32>(select(t >> 1u, (t >> 1u) + (p >> 1u) + 1u, (t & 1u) != 0u), 0u);
      }
      if (ge64(X, wend)) { continue; }                   // no multiple in this batch
      // (j, off) = (X / delta, X mod delta), exact. X < wend < 2^32 here (host:
      // wend = (nseg - 1) delta + q_odds fits u32), so: an f32 estimate within 1
      // (X < 2^32, j < 2^8: relative error < 2^-22) and one integer fix-up each way
      // (j delta <= X + delta < 2^32: no wrap)
      var j = u32(f32(X.x) * dinv);
      if (j * delta > X.x) { j -= 1u; }
      if (X.x - j * delta >= delta) { j += 1u; }
      var off = X.x - j * delta;                         // < delta
      var v = (H[6] + 2u * (((j % 15015u) * H[8] + off % 15015u) % 15015u)) % 15015u;
      let sv = (2u * (p % 15015u)) % 15015u;
      loop {
        if (j > nseg) { break; }
        if (((htabs[T_CP + (v >> 5u)] >> (v & 31u)) & 1u) != 0u) {
          if (j < nseg && off < qo_of(j)) { emit(j, off, grp, ngrp); }
          if (j >= 1u && j - 1u < nseg && off + delta < qo_of(j - 1u)) { emit(j - 1u, off + delta, grp, ngrp); }
        }
        off += p;                                        // < delta + p < 2^32 (host-checked)
        var t = u32(f32(off) * dinv);                    // floor(off / delta) within 1, then exact
        if (t * delta > off) { t -= 1u; }
        if (off - t * delta >= delta) { t += 1u; }
        j += t;
        off -= t * delta;
        v += sv;
        if (v >= 15015u) { v -= 15015u; }
      }
    }
  }
  workgroupBarrier();
  if (lid < nseg) {
    let c = atomicLoad(&lpos[lid]);
    hgcnt[lid * ngrp + grp] = min(c, H[7]);
    if (c > H[7]) { atomicOr(&herr[0], ERR_OVERFLOW); }
  }
}
