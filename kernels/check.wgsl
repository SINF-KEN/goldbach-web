// goldbach-web: the mod-30 checker and the exact statistics (WebGPU).
//
// Evens n = seg_start + 2 e are grouped by class cr = (n mod 30) / 2; the evens
// of class cr are e = e0_cr + 15 i, and for a candidate p (q = n - p in class
// array k) the map bit of q is t = i + U with U = (V_k - p) / 30, V_k =
// seg_start + 2 e0_cr - Q_k (exact: q is in class k). Consecutive evens of a
// class are consecutive bits of class k, so one candidate tests 32 evens per
// u32 word.
//
// Candidates are tested in ASCENDING order (per class, the useful ones: the
// others give q divisible by 3 or 5). So the first candidate that resolves an
// even is p(n), the smallest Goldbach prime, and the histogram of p(n) is exact
// for EVERY even, independent of the segmentation:
//   check_a: the first KA candidates (all < tail_t = 3000), bit-parallel, with
//            the class windows of a block in workgroup memory; unresolved evens
//            are appended to `pending`;
//   check_b: one invocation per pending even continues the ascending scan;
//            p(n) >= tail_t also updates the first occurrence of p and, above
//            the ridge floor, appends (e, j) to the ridge list; nothing found
//            -> `leftover` (settled by CPU Miller-Rabin).
// Overflows of pending / ridge / leftover lists set err (fail closed).

const WGA: u32 = 64u;
const WA: u32 = 2u;                   // u32 words of evens per invocation (B580: 4 words made check_a
                                      // 23 % slower, 8 another 60 %: the loop is register-bound)
const BLK: u32 = WGA * WA * 32u;      // evens of each class per block (one work-group)
const WIN: u32 = WGA * WA + 4u;       // window words per class array (host: U span <= 64)
const KA_MAX: u32 = 128u;             // C layout stride
const KA_CNT: u32 = 96u;              // phase-A length cap of the per-invocation counters (host-checked)
const HSTRIDE: u32 = 1280u;           // histogram stride per class (>= list length)
const ERR_PENDING: u32 = 4u;
const ERR_RIDGE: u32 = 8u;
const ERR_LEFTOVER: u32 = 16u;

// C (params):
//   [0] cw32 [1] KA [2] pend_cap [3] nb (blocks per class) [4] sm30 = seg_start mod 30
//   [5] ridge_floor (p) [6] ridge_cap [7] left_cap [8] u_lo (min U of phase A, >= 0)
//   [9] tail_t [10] first_off: firstE index base [11] io offset of the ridge pairs [12] of the leftovers
//   [16 + k] n_k (valid bits of class k)   [24 + k] V_k - 2 e0 offset: 9781 - 2 phi_k
//   [32 + 4 cr] e0_cr, ec_cr (evens of the class), cand offset, cand length
//   [96 + cr] class mask of the arrays used by class cr (bit k)
//   [128 + KA_MAX cr + j] phase-A candidate j of class cr (host, per segment):
//                         (U - 32 floor(u_lo / 32)) | k << 16, U = (V_k - p) / 30
@group(0) @binding(0) var<storage, read> map: array<u32>;
@group(0) @binding(1) var<storage, read> cand: array<u32>;        // p | k << 16
@group(0) @binding(2) var<storage, read> C: array<u32>;
@group(0) @binding(3) var<storage, read_write> hist: array<atomic<u32>>;   // [cr * HSTRIDE + j]
// io: one buffer for the per-segment lists (<= 8 storage bindings per stage):
//   [0] pending count [1] ridge count [2] leftover count [3] error flags
//   [16 ..)            pending evens (C[2] entries)
//   [C[11] ..)         ridge pairs (e, cr << 16 | j) (C[6] pairs)
//   [C[12] ..)         leftover evens (C[7] entries)
@group(0) @binding(4) var<storage, read_write> io: array<atomic<u32>>;
const IO_PEND: u32 = 16u;

// class windows, this class's phase-A candidates (4 per vec4), and the
// histogram partials: candidates 2 m, 2 m + 1 as the u16 halves of
// hwg[8 m + (me & 7)] (a work-group resolves at most BLK = 4096 evens per
// candidate: no carry into the high half)
var<workgroup> win: array<u32, 8u * WIN>;
var<workgroup> cu: array<vec4<u32>, 24>;   // KA_CNT / 4
var<workgroup> hwg: array<atomic<u32>, 384>;   // KA_CNT / 2 x 8

fn vk(cr: u32, k: u32) -> i32 {
  return i32(C[24u + k] + 2u * C[32u + 4u * cr]);
}

// candidate v = u | k << 16: my 2 words at window bit 64 me + u of class k
fn cwords(v: u32, me: u32) -> vec2<u32> {
  let u = v & 0xFFFFu;
  let rr = u & 31u;
  let l = 31u - rr;
  let P = (v >> 16u) * WIN + WA * me + (u >> 5u);
  let x0 = win[P]; let x1 = win[P + 1u]; let x2 = win[P + 2u];
  // funnel by rr (rr = 0: (x << 31) << 1 = 0)
  return vec2<u32>((x0 >> rr) | ((x1 << l) << 1u), (x1 >> rr) | ((x2 << l) << 1u));
}

fn newly(h: vec2<u32>, acc: vec2<u32>) -> u32 {
  let c = countOneBits(h & ~acc);
  return c.x + c.y;
}

@compute @workgroup_size(64)
fn check_a(@builtin(workgroup_id) wid: vec3<u32>,
           @builtin(num_workgroups) nwg: vec3<u32>,
           @builtin(local_invocation_index) me: u32) {
  // one work-group per block: the 15 classes of a block read the same map
  // windows, loaded into workgroup memory once (a work-group per class and
  // block loaded them 15 times: check_a -12 % on the B580)
  let blk = wid.x + wid.y * nwg.x;
  if (blk >= C[3]) { return; }
  let i0 = blk * BLK;
  let cw32 = C[0];
  let KA = C[1];                                  // host: a multiple of 4, <= KA_CNT
  let ulo = C[8];
  var cmask = 0u;                                 // arrays used by the classes with evens here
  for (var cr = 0u; cr < 15u; cr++) {
    if (i0 < C[33u + 4u * cr]) { cmask |= C[96u + cr]; }
  }
  // ---- class windows: words from (i0 + ulo) / 32 ----
  let wb = (i0 >> 5u) + (ulo >> 5u);
  for (var k = 0u; k < 8u; k++) {
    if (((cmask >> k) & 1u) == 0u) { continue; }
    for (var w = me; w < WIN; w += WGA) {
      let gw = wb + w;
      win[k * WIN + w] = select(0u, map[k * cw32 + gw], gw < cw32);
    }
  }
  let nq = KA >> 2u;
  let hs = me & 7u;
  let ib0 = i0 + 32u * me * WA;                   // my evens: class indices ib0 + 32 w + b, w < WA
  for (var cr = 0u; cr < 15u; cr++) {
    let ec = C[33u + 4u * cr];
    if (i0 >= ec) { continue; }                   // uniform
    if (me < nq) {
      let o = 128u + KA_MAX * cr + 4u * me;
      cu[me] = vec4<u32>(C[o], C[o + 1u], C[o + 2u], C[o + 3u]);
    }
    for (var j = me; j < 384u; j += WGA) { atomicStore(&hwg[j], 0u); }
    workgroupBarrier();
    var acc = vec2<u32>(0u);
    for (var w = 0u; w < WA; w++) {
      let ib = ib0 + 32u * w;
      var a = select(0u, 0xFFFFFFFFu, ib >= ec);
      if (ib < ec && ec - ib < 32u) { a = ~((1u << (ec - ib)) - 1u); }
      acc[w] = a;
    }
    for (var q = 0u; q < nq; q++) {
      let v = cu[q];
      let h0 = cwords(v.x, me); let h1 = cwords(v.y, me); let h2 = cwords(v.z, me); let h3 = cwords(v.w, me);
      let c0 = newly(h0, acc); acc |= h0;
      let c1 = newly(h1, acc); acc |= h1;
      let c2 = newly(h2, acc); acc |= h2;
      let c3 = newly(h3, acc); acc |= h3;
      let p01 = c0 | (c1 << 16u);
      let p23 = c2 | (c3 << 16u);
      if (p01 != 0u) { atomicAdd(&hwg[16u * q + hs], p01); }
      if (p23 != 0u) { atomicAdd(&hwg[16u * q + 8u + hs], p23); }
    }
    // ---- unresolved evens -> pending (one reservation per invocation) ----
    let e0 = C[32u + 4u * cr];
    let nmiss = countOneBits(~acc);
    let nm = nmiss.x + nmiss.y;
    if (nm != 0u) {
      var idx = atomicAdd(&io[0], nm);
      if (idx + nm > C[2]) { atomicOr(&io[3], ERR_PENDING); }
      for (var w = 0u; w < WA; w++) {
        var miss = ~acc[w];
        while (miss != 0u) {
          let b = firstTrailingBit(miss);
          miss &= miss - 1u;
          if (idx < C[2]) { atomicStore(&io[IO_PEND + idx], e0 + 15u * (i0 + 32u * (me * WA + w) + b)); }
          idx++;
        }
      }
    }
    workgroupBarrier();
    // ---- partials -> histogram: one global add per candidate ----
    for (var j = me; j < KA; j += WGA) {
      let m = j >> 1u;
      var v = 0u;
      for (var t = 0u; t < 8u; t++) { v += atomicLoad(&hwg[8u * m + t]); }
      v = select(v & 0xFFFFu, v >> 16u, (j & 1u) != 0u);
      if (v != 0u) { atomicAdd(&hist[cr * HSTRIDE + j], v); }
    }
    workgroupBarrier();                           // cu and hwg are reused by the next class
  }
}

// ---- phase B --------------------------------------------------------------
@group(0) @binding(5) var<storage, read_write> firstE: array<atomic<u32>>;  // per p >> 1: min e (segment)

// phase-B hits of candidates KA .. KA + BW - 1 (nearly all of them) are counted
// per work-group first (one global add per bin and work-group, not one per
// even: millions of evens hit a dozen bins), the rarer ones directly
const BW: u32 = 64u;
var<workgroup> hb: array<atomic<u32>, 960>;      // 15 x BW

@compute @workgroup_size(64)
fn check_b(@builtin(global_invocation_id) gid3: vec3<u32>,
           @builtin(num_workgroups) nwg: vec3<u32>,
           @builtin(local_invocation_index) me: u32) {
  for (var x = me; x < 15u * BW; x += 64u) { atomicStore(&hb[x], 0u); }
  workgroupBarrier();
  let g = gid3.x + gid3.y * nwg.x * 64u;
  let npend = min(atomicLoad(&io[0]), C[2]);
  let KA = C[1];
  if (g < npend) {                                  // no early return: barrier below
    let cw32 = C[0];
    let e = atomicLoad(&io[IO_PEND + g]);
    let cr = ((C[4] + 2u * (e % 15u)) % 30u) / 2u;
    let e0 = C[32u + 4u * cr];
    let i = (e - e0) / 15u;
    let co = C[34u + 4u * cr];
    let len = C[35u + 4u * cr];
    var found = false;
    for (var j = KA; j < len; j++) {
      let c = cand[co + j];
      let p = c & 0xFFFFu;
      let k = c >> 16u;
      let t = i32(i) + (vk(cr, k) - i32(p)) / 30;
      if (t < 0 || u32(t) >= C[16u + k]) { continue; }
      let tu = u32(t);
      if (((map[k * cw32 + (tu >> 5u)] >> (tu & 31u)) & 1u) != 0u) {
        if (j - KA < BW) { atomicAdd(&hb[cr * BW + (j - KA)], 1u); }
        else { atomicAdd(&hist[cr * HSTRIDE + j], 1u); }
        if (p >= C[9]) {
          atomicMin(&firstE[p >> 1u], e);
          if (p >= C[5]) {
            let idx = atomicAdd(&io[1], 1u);
            if (idx < C[6]) {
              atomicStore(&io[C[11] + 2u * idx], e);
              atomicStore(&io[C[11] + 2u * idx + 1u], (cr << 16u) | j);
            } else { atomicOr(&io[3], ERR_RIDGE); }
          }
        }
        found = true;
        break;
      }
    }
    if (!found) {
      let idx = atomicAdd(&io[2], 1u);
      if (idx < C[7]) { atomicStore(&io[C[12] + idx], e); } else { atomicOr(&io[3], ERR_LEFTOVER); }
    }
  }
  workgroupBarrier();
  for (var x = me; x < 15u * BW; x += 64u) {
    let v = atomicLoad(&hb[x]);
    if (v != 0u) {
      let cr = x / BW;
      atomicAdd(&hist[cr * HSTRIDE + KA + (x - cr * BW)], v);
    }
  }
}


// ---- indirect dispatch arguments of check_b (64 invocations per group) -------
@group(0) @binding(6) var<storage, read_write> bargs: array<u32>;

@compute @workgroup_size(1)
fn b_args() {
  let n = min(atomicLoad(&io[0]), C[2]);
  let g = (n + 63u) / 64u;
  bargs[0] = min(g, 65535u);
  bargs[1] = max(1u, (g + 65534u) / 65535u);
  bargs[2] = 1u;
}
