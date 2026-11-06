// goldbach-web: hit-list harvest (port of hl_harvest; see large.wgsl).
// Walks every odd multiple of each hit-list prime through a batch of nseg
// segments in (segment j, offset off < delta) form, keeps multipliers coprime
// to 30030 (CP15015 test) and files each hit into this work-group's region of
// the segment's list; leaves the next batch's state in hstate.

const NONE: u32 = 0xFFFFFFFFu;
const T_CP: u32 = 65820u;
const HL_MAX_K: u32 = 128u;
const ERR_OVERFLOW: u32 = 2u;
override WG_HARVEST: u32 = 256u;

// ---- hl_harvest -------------------------------------------------------------
// H: [0] a [1] b [2] delta [3] q_odds_full [4] nseg [5] q_odds_tail
//    [6] qf15 (batch q_first mod 15015) [7] capg [8] dm = delta mod 15015
@group(0) @binding(0) var<storage, read> hprimes: array<u32>;
@group(0) @binding(1) var<storage, read_write> hstate: array<u32>;   // (j, off) pairs
@group(0) @binding(2) var<storage, read_write> hlists: array<u32>;
@group(0) @binding(3) var<storage, read_write> hgcnt: array<u32>;
@group(0) @binding(4) var<storage, read_write> herr: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read> H: array<u32>;
@group(0) @binding(6) var<storage, read> htabs: array<u32>;

var<workgroup> lpos: array<atomic<u32>, HL_MAX_K>;

fn qo_of(j: u32) -> u32 { return select(H[3], H[5], j == H[4] - 1u); }

fn emit(j: u32, off: u32, grp: u32, ngrp: u32) {
  let idx = atomicAdd(&lpos[j], 1u);
  if (idx < H[7]) { hlists[(j * ngrp + grp) * H[7] + idx] = off; }
}

@compute @workgroup_size(WG_HARVEST)
fn harvest(@builtin(workgroup_id) wid: vec3<u32>,
           @builtin(num_workgroups) nwg: vec3<u32>,
           @builtin(local_invocation_index) lid: u32) {
  let grp = wid.x + wid.y * nwg.x;
  let ngrp = nwg.x * nwg.y;
  for (var k = lid; k < HL_MAX_K; k += WG_HARVEST) { atomicStore(&lpos[k], 0u); }
  workgroupBarrier();
  let a = H[0];
  let b = H[1];
  let delta = H[2];
  let nseg = H[4];
  for (var c0 = grp * WG_HARVEST; c0 < b - a; c0 += ngrp * WG_HARVEST) {
    let i = a + c0 + lid;
    if (i >= b) { break; }
    let p = hprimes[i];
    var j = hstate[2u * (i - a)];
    var off = hstate[2u * (i - a) + 1u];
    if (j <= nseg) {
      // q mod 15015 of the position: q = q_first + 2 (j delta + off)
      var v = (H[6] + 2u * (((j % 15015u) * H[8] + off % 15015u) % 15015u)) % 15015u;
      let sv = (2u * (p % 15015u)) % 15015u;
      var nj = NONE;
      var noff = 0u;
      loop {
        if (j >= nseg && nj == NONE) { nj = j - nseg; noff = off; }
        if (j > nseg) { break; }
        if (((htabs[T_CP + (v >> 5u)] >> (v & 31u)) & 1u) != 0u) {
          if (j < nseg && off < qo_of(j)) { emit(j, off, grp, ngrp); }
          if (j >= 1u && j - 1u < nseg && off + delta < qo_of(j - 1u)) { emit(j - 1u, off + delta, grp, ngrp); }
        }
        off += p;                                   // < delta + p < 2^32 (host-checked)
        let t = off / delta;
        j += t;
        off -= t * delta;
        v += sv;
        if (v >= 15015u) { v -= 15015u; }
      }
      j = nj;
      off = noff;
    } else {
      j -= nseg;                                    // dormant: no multiple in this batch
    }
    hstate[2u * (i - a)] = j;
    hstate[2u * (i - a) + 1u] = off;
  }
  workgroupBarrier();
  if (lid < nseg) {
    let c = atomicLoad(&lpos[lid]);
    hgcnt[lid * ngrp + grp] = min(c, H[7]);
    if (c > H[7]) { atomicOr(&herr[0], ERR_OVERFLOW); }
  }
}

// ---- per-run states on the GPU (replaces core.cpp gb_packed_state and
// gb_harvest_state, which it must match bit for bit: test/states_test.mjs) ----
// For each prime index i in [pa, np), with q_first odd (64-bit, as hi/lo):
//   * packed primes, i < pb (host: p^2 < q_first): pstate[i - pa] = map bit of
//     the first odd multiple of p at or after q_first, i.e. t / 2 for the
//     smallest t >= 0 with q_first + t an odd multiple of p (t < 2p);
//   * hit-list primes, i >= pb: X = (max(p^2, first odd multiple >= q_first)
//     - q_first) / 2 as hstate pair (j = X / delta, off = X mod delta), or
//     (j_far, 0) when j >= j_far (parked: no multiple in any accepted run).
// Exact in u32 arithmetic: q_first mod p by modq (p >= 36000 odd: every prime
// from the split on), p^2 as a 64-bit product of 16-bit halves, and X / delta
// by restoring division (host: delta < 2^31, so 2 rem + 1 < 2^32).
// S: [0] q_first hi [1] q_first lo [2] delta [3] pa [4] pb [5] np [6] j_far
@group(0) @binding(7) var<storage, read_write> spstate: array<u32>;
@group(0) @binding(8) var<storage, read> S: array<u32>;

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

// a b as (lo, hi), exact
fn mul64(a: u32, b: u32) -> vec2<u32> {
  let al = a & 0xFFFFu; let ah = a >> 16u;
  let bl = b & 0xFFFFu; let bh = b >> 16u;
  let ll = al * bl;
  let mid = al * bh + ah * bl;                       // may wrap: carry below
  let cmid = select(0u, 0x10000u, mid < al * bh);
  let lo = ll + (mid << 16u);
  let hi = ah * bh + (mid >> 16u) + cmid + select(0u, 1u, lo < ll);
  return vec2<u32>(lo, hi);
}

// t / 2 for the smallest t >= 0 with q_first + t an odd multiple of p (q_first odd)
fn first_odd_half(p: u32) -> u32 {
  let r = modq(S[0], S[1], p);
  let t = select(p - r, 0u, r == 0u);
  // t odd: the even multiple q_first + t is skipped, (t + p) / 2 without overflow
  return select((t >> 1u) + (p >> 1u) + 1u, t >> 1u, (t & 1u) == 0u);
}

@compute @workgroup_size(256)
fn states(@builtin(global_invocation_id) gid3: vec3<u32>,
          @builtin(num_workgroups) nwg: vec3<u32>) {
  let i = S[3] + gid3.x + gid3.y * nwg.x * 256u;
  if (i >= S[5]) { return; }
  let p = hprimes[i];
  if (i < S[4]) {
    spstate[i - S[3]] = first_odd_half(p);
    return;
  }
  // X = (xh, xl)
  var xh = 0u;
  var xl = 0u;
  let sq = mul64(p, p);
  if (sq.y > S[0] || (sq.y == S[0] && sq.x >= S[1])) {
    // p^2 - q_first: both odd, the difference is even
    let dl = sq.x - S[1];
    let dh = sq.y - S[0] - select(0u, 1u, sq.x < S[1]);
    xl = (dl >> 1u) | (dh << 31u);
    xh = dh >> 1u;
  } else {
    xl = first_odd_half(p);
  }
  let delta = S[2];
  var j = S[6];
  var off = 0u;
  if (xh == 0u) {
    j = xl / delta;
    off = xl - j * delta;
  } else if (xh < delta) {
    // quotient < 2^32: restoring division of (xh, xl) by delta, rem < delta
    var rem = xh;
    var q = 0u;
    for (var b = 0u; b < 32u; b++) {
      rem = (rem << 1u) | (xl >> 31u);
      xl <<= 1u;
      q <<= 1u;
      if (rem >= delta) { rem -= delta; q |= 1u; }
    }
    j = q;
    off = rem;
  }                                                  // else: X / delta >= 2^32 > j_far
  if (j >= S[6]) { j = S[6]; off = 0u; }
  hstate[2u * (i - S[4])] = j;
  hstate[2u * (i - S[4]) + 1u] = off;
}
