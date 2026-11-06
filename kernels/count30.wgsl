// goldbach-web: prime and twin-prime counts and prime gaps of a segment (port
// of goldbach_l0 2.5's count30: the gap walk of the former gaps30.wgsl fused
// in: one read of the class map, the range masks and look-back shared; the
// live paper's section 2.2). One invocation per WPI u32 words of every class
// array. Position of class c bit t: q - q_first = 30 t + 2 phi[c]; the walk
// works in half positions h = (q - q_first) / 2, so h - h_prev = g / 2.
//
// lim (gstats.hpp count30_limits): [2c, 2c + 1) = class-c bit range of the
// counted primes; twin pair j at 16 + 5 j: k, k2, d (0/1: partner bit t + d of
// class k2), [tl, th) range of t in class k.
// cnt: [0, 8) primes per class, [8, 11) twins per pair.
// GP: [0] cw32, [3] pos_lo (smallest counted position), [4] min_upper (segment
// 0: the run starts at start + 1), [5] tcap, [6] phi by class (nibbles).
// A gap is (previous counted prime, q) with q >= min_upper; the gap into the
// segment's first prime is the host's (it stitches segments). An invocation's
// first prime finds its predecessor in the 2 words before it (every gap below
// 2^64 is < 1552 < 30 * 64).
// G (zeroed by the host): [0] max(~first position), [1] max(last position + 1),
// [2] error flags (1: gap >= 2048, 2: predecessor beyond the look-back, 4: tail
// list full), [3] gaps >= 700, [4, 4 + 1024) histogram by g / 2, then the
// (upper position, g) pairs of the gaps >= 700 (tcap of them).
// R: per work-group (g, upper position) of its largest gap (first among equal;
// (0, 0) if none): the host takes the largest, then the smallest position.
@group(0) @binding(0) var<storage, read> cmap: array<u32>;
@group(0) @binding(1) var<storage, read> lim: array<u32>;
@group(0) @binding(2) var<storage, read_write> cnt: array<atomic<u32>>;
@group(0) @binding(3) var<storage, read> GP: array<u32>;
@group(0) @binding(4) var<storage, read_write> G: array<atomic<u32>>;
@group(0) @binding(5) var<storage, read_write> R: array<u32>;

const WPI: u32 = 4u;       // words per invocation (every class)
const HBINS: u32 = 1024u;
const TAILH: u32 = 350u;   // gaps >= 700, in half units
const NONE: u32 = 0xFFFFFFFFu;

var<workgroup> wcnt: array<atomic<u32>, 16>;
var<workgroup> wh: array<atomic<u32>, 1024>;   // bin 0: primes without a counted predecessor (no gap)
var<workgroup> wmax: atomic<u32>;
var<workgroup> wpos: atomic<u32>;

fn range_mask(w: u32, lo: u32, hi: u32) -> u32 {
  // bits t in [lo, hi) of word w
  let b0 = w * 32u;
  if (hi <= b0 || lo >= b0 + 32u) { return 0u; }
  var m = 0xFFFFFFFFu;
  if (lo > b0) { m &= ~((1u << (lo - b0)) - 1u); }
  if (hi < b0 + 32u) { m &= (1u << (hi - b0)) - 1u; }
  return m;
}
// class c's counted primes in word w
fn word(c: u32, w: u32, cw32: u32) -> u32 {
  return cmap[c * cw32 + w] & range_mask(w, lim[2u * c], lim[2u * c + 1u]);
}
// twin pairs of type j in word w: bit t of class k and bit t + d of class k2
fn twins(j: u32, w: u32, cw32: u32) -> u32 {
  let k = lim[16u + 5u * j];
  let k2 = lim[17u + 5u * j];
  var part = cmap[k2 * cw32 + w];
  if (lim[18u + 5u * j] == 1u) {
    let nxt = select(0u, cmap[k2 * cw32 + w + 1u], w + 1u < cw32);
    part = (part >> 1u) | (nxt << 31u);
  }
  return countOneBits(cmap[k * cw32 + w] & part & range_mask(w, lim[19u + 5u * j], lim[20u + 5u * j]));
}

@compute @workgroup_size(256)
fn count30(@builtin(global_invocation_id) gid3: vec3<u32>,
           @builtin(workgroup_id) wid: vec3<u32>,
           @builtin(num_workgroups) nwg: vec3<u32>,
           @builtin(local_invocation_index) me: u32) {
  if (me < 16u) { atomicStore(&wcnt[me], 0u); }
  for (var i = me; i < HBINS; i += 256u) { atomicStore(&wh[i], 0u); }
  if (me == 0u) { atomicStore(&wmax, 0u); atomicStore(&wpos, NONE); }
  workgroupBarrier();
  let cw32 = GP[0];
  let muh = (GP[4] + 1u) / 2u;               // the smallest counted upper prime, half position
  let lane = gid3.x + gid3.y * nwg.x * 256u;
  let w = WPI * lane;
  var first = NONE;
  var last = 0u;
  var gmax = 0u;                             // largest gap / 2 and its upper h
  var gpos = 0u;
  var err = 0u;
  if (w + WPI <= cw32) {
    var ph: array<u32, 8>;
    for (var c = 0u; c < 8u; c++) { ph[c] = (GP[6] >> (4u * c)) & 15u; }
    // the last counted prime before the invocation: word w - 1, else w - 2
    var hp = NONE;
    for (var b = 1u; b <= 2u && hp == NONE && b <= w; b++) {
      let wb = w - b;
      for (var c = 0u; c < 8u; c++) {
        let x = word(c, wb, cw32);
        if (x != 0u) {
          let h = 15u * (32u * wb + 31u - countLeadingZeros(x)) + ph[c];
          hp = select(h, max(hp, h), hp != NONE);
        }
      }
    }
    // none although the look-back lay wholly inside the counting range
    let lookErr = hp == NONE && w >= 2u && 30u * 32u * (w - 2u) >= GP[3];
    var np: array<u32, 8>;
    for (var c = 0u; c < 8u; c++) { np[c] = 0u; }
    var nt = vec3<u32>(0u);
    for (var i = 0u; i < WPI; i++) {
      let wi = w + i;
      var x: array<u32, 8>;
      var any = 0u;
      var wf = NONE;                         // the word's first counted prime, half position
      for (var c = 0u; c < 8u; c++) {
        x[c] = word(c, wi, cw32);
        any |= x[c];
        np[c] += countOneBits(x[c]);
        if (x[c] != 0u) {
          wf = min(wf, 15u * (32u * wi + countTrailingZeros(x[c])) + ph[c]);
          last = max(last, 2u * (15u * (32u * wi + 31u - countLeadingZeros(x[c])) + ph[c]));
        }
      }
      for (var j = 0u; j < 3u; j++) { nt[j] += twins(j, wi, cw32); }
      if (any == 0u) { continue; }
      first = min(first, 2u * wf);
      if (hp == NONE) {
        // the segment's first prime: no gap here (bin 0)
        if (lookErr) { err |= 2u; }
        hp = wf;
      }
      // the word's primes in ascending order, one per iteration: a block's
      // primes as a 15-bit mask of half offsets, refilled from the next
      // non-empty block
      var nb = any;
      var m = 0u;
      var hb = 0u;
      loop {
        if (m == 0u) {
          if (nb == 0u) { break; }
          let b = countTrailingZeros(nb);
          nb &= nb - 1u;
          for (var c = 0u; c < 8u; c++) { m |= ((x[c] >> b) & 1u) << ph[c]; }
          hb = 15u * (32u * wi + b);
        }
        let h = hb + countTrailingZeros(m);
        m &= m - 1u;
        let d = h - hp;
        hp = h;
        let counted = h >= muh;
        err |= select(0u, 1u, counted && d >= HBINS);
        atomicAdd(&wh[select(0u, min(d, HBINS - 1u), counted)], 1u);
        if (counted && d > gmax) { gmax = d; gpos = h; }
        if (counted && d >= TAILH) {
          let slot = atomicAdd(&G[3], 1u);
          if (slot < GP[5]) {
            atomicStore(&G[4u + HBINS + 2u * slot], 2u * h);
            atomicStore(&G[5u + HBINS + 2u * slot], 2u * d);
          } else { err |= 4u; }
        }
      }
    }
    for (var c = 0u; c < 8u; c++) { if (np[c] != 0u) { atomicAdd(&wcnt[c], np[c]); } }
    for (var j = 0u; j < 3u; j++) { if (nt[j] != 0u) { atomicAdd(&wcnt[8u + j], nt[j]); } }
  }
  if (err != 0u) { atomicOr(&G[2], err); }
  if (first != NONE) {
    atomicMax(&G[0], ~first);
    atomicMax(&G[1], last + 1u);
  }
  if (gmax != 0u) { atomicMax(&wmax, gmax); }
  workgroupBarrier();
  let wm = atomicLoad(&wmax);
  if (wm != 0u && gmax == wm) { atomicMin(&wpos, gpos); }
  workgroupBarrier();
  if (me < 11u) {
    let v = atomicLoad(&wcnt[me]);
    if (v != 0u) { atomicAdd(&cnt[me], v); }
  }
  let wg = wid.x + wid.y * nwg.x;
  if (me == 0u) {
    R[2u * wg] = 2u * wm;
    R[2u * wg + 1u] = select(0u, 2u * atomicLoad(&wpos), wm != 0u);
  }
  for (var i = 1u + me; i < HBINS; i += 256u) {
    let v = atomicLoad(&wh[i]);
    if (v != 0u) { atomicAdd(&G[4u + i], v); }
  }
}
