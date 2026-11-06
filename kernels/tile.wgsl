// goldbach-web: tile sieve of the mod-30 class map (WebGPU port of
// make_prime_map, kernels/goldbach.cl, class mode).
//
// One work-group of 256 invocations builds one 16 KB tile = 131072 bits of one
// class array in workgroup memory, then writes it to the map once:
//   1. mask groups: every word = AND of the canonical group tables (primes
//      7 .. 61; 3 and 5 have no multiples in the classes), table word
//      (x - Y_g) mod P_g for u64 word x of the class array;
//   2. thread-owned walk for primes [mask_count, own_end): the tile's first
//      mark of each prime is computed once (tf[]), every invocation clears the
//      marks inside its own 512-bit range;
//   3. medium primes [own_end, split): an invocation takes whole primes and
//      clears all their marks in the tile;
//   4. coalesced write-out, bits >= n_c of the class cleared.
// All arithmetic is exact: u32, and float quotients only inside fmod (exact
// fix-up; class indices of a segment are < 2^24, host-checked).
//
// Map layout: class c at u32 word c * cw32, cw32 = tpc * TILE_WORDS; bit t of
// class c <-> q = Q_c + 30 t.

const WG: u32 = 256u;
const TILE_WORDS: u32 = 4096u;     // u32 words per tile (16 KB)
const TILE_BITS: u32 = 131072u;
const WPT: u32 = 16u;              // u32 words per invocation
const OWN_MAX: u32 = 256u;         // owned primes per tile (host-checked)
const MAX_GROUPS: u32 = 8u;
const NONE: u32 = 0xFFFFFFFFu;

// P (read-only storage, u32 words):
//   [0] cw32  [1] tpc  [2] n_groups  [3] split  [4] own_end  [5] mask_count
//   [8 + c]            n_c, valid bits of class c (c < 8)
//   [16 + 2 g]         group table offset (u64 entries), [17 + 2 g] period
//   [32 + 8 c + g]     Y_g of class c
@group(0) @binding(0) var<storage, read_write> map: array<u32>;
@group(0) @binding(1) var<storage, read> primes: array<u32>;   // base primes [0, split)
@group(0) @binding(2) var<storage, read> fb: array<u32>;       // first marks, [c * split + j]
@group(0) @binding(3) var<storage, read> gmask: array<u32>;    // group tables, u64 as u32 pairs
@group(0) @binding(4) var<storage, read> P: array<u32>;

var<workgroup> tile: array<atomic<u32>, TILE_WORDS>;
var<workgroup> tf: array<u32, OWN_MAX>;

// d mod p for d < 2^24 (class-array bits: host-checked), p >= 1: a float
// quotient (d and p exact in f32, the quotient within 1 of floor(d / p)) and
// an exact integer fix-up -- no emulated u32 division.
fn fmod(d: u32, p: u32) -> u32 {
  let q = u32(f32(d) / f32(p));
  var r = i32(d) - i32(q * p);
  if (r < 0) { r += i32(p); }
  if (r >= i32(p)) { r -= i32(p); }
  return u32(r);
}

// Tile-local bit of the first mark of p at or after the tile start, given the
// prime's first mark f0 in the class array (NONE: none in the segment); the
// result may lie beyond the tile (no mark).
fn first_in_tile(f0: u32, p: u32, tb: u32) -> u32 {
  if (f0 == NONE) { return NONE; }
  if (f0 >= tb) { return f0 - tb; }
  let r = fmod(tb - f0, p);
  return select(p - r, 0u, r == 0u);
}

@compute @workgroup_size(256)
fn tile_sieve(@builtin(workgroup_id) wid: vec3<u32>,
              @builtin(num_workgroups) nwg: vec3<u32>,
              @builtin(local_invocation_index) me: u32) {
  let tpc = P[1];
  let gidx = wid.x + wid.y * nwg.x;
  let cls = gidx / tpc;
  if (cls >= 8u) { return; }                       // uniform per work-group
  let t = gidx - cls * tpc;
  let tb = t * TILE_BITS;                          // class bit of the tile start
  let split = P[3];
  let own_end = P[4];
  let mask_count = P[5];

  // ---- 1. mask groups --------------------------------------------------
  let w0 = me * WPT;                               // my first u32 word (even)
  let x0 = (tb >> 6u) + (w0 >> 1u);                // my first u64 word of the class array
  let ng = P[2];
  var ph: array<u32, MAX_GROUPS>;
  for (var g = 0u; g < ng; g++) {
    let per = P[17u + 2u * g];
    let y = P[32u + 8u * cls + g];
    ph[g] = fmod(fmod(x0, per) + per - y, per);
  }
  for (var i = 0u; i < WPT / 2u; i++) {
    var lo = 0xFFFFFFFFu;
    var hi = 0xFFFFFFFFu;
    for (var g = 0u; g < ng; g++) {
      let e = 2u * (P[16u + 2u * g] + ph[g]);
      lo &= gmask[e];
      hi &= gmask[e + 1u];
      let per = P[17u + 2u * g];
      ph[g] = select(ph[g] + 1u, 0u, ph[g] + 1u == per);
    }
    atomicStore(&tile[w0 + 2u * i], lo);
    atomicStore(&tile[w0 + 2u * i + 1u], hi);
  }

  // ---- 2. thread-owned walk ----------------------------------------------
  let fbc = cls * split;
  let nown = own_end - mask_count;
  for (var j = me; j < nown; j += WG) {
    tf[j] = first_in_tile(fb[fbc + mask_count + j], primes[mask_count + j], tb);
  }
  workgroupBarrier();
  let b0 = me * (WPT * 32u);
  let b1 = b0 + WPT * 32u;
  for (var j = 0u; j < nown; j++) {
    var f = tf[j];
    if (f >= TILE_BITS) { continue; }
    let p = primes[mask_count + j];
    if (f < b0) { let r = fmod(b0 - f, p); f = b0 + select(p - r, 0u, r == 0u); }
    for (; f < b1; f += p) {
      atomicAnd(&tile[f >> 5u], ~(1u << (f & 31u)));
    }
  }
  workgroupBarrier();

  // ---- 3. medium primes, prime-parallel -------------------------------------
  for (var j = own_end + me; j < split; j += WG) {
    let p = primes[j];
    var f = first_in_tile(fb[fbc + j], p, tb);
    for (; f < TILE_BITS; f += p) {
      atomicAnd(&tile[f >> 5u], ~(1u << (f & 31u)));
    }
  }
  workgroupBarrier();

  // ---- 4. write-out, tail cleared ------------------------------------------
  let n = P[8u + cls];
  let dst = cls * P[0] + t * TILE_WORDS;
  for (var i = 0u; i < WPT; i++) {
    let k = w0 + i;
    var v = atomicLoad(&tile[k]);
    let bit0 = tb + k * 32u;
    if (bit0 + 32u > n) {
      v &= select(0u, select(0xFFFFFFFFu, (1u << (n - bit0)) - 1u, n - bit0 < 32u), n > bit0);
    }
    map[dst + k] = v;
  }
}
