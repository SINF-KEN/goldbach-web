# The 25-operation layer: evaluation and decision

`docs/OPTIMIZATION_GUIDE.md` §3 asks whether a typed operation layer (an IR of
25 operations, with rewrites and code generation) is worth adding to the
verifier, and to add it only if it solves a measured problem or makes a
required transformation easier to prove and test (§3.5).

## Where the 25 operations already live

The verifier has no IR, but each operation has a concrete counterpart, and its
contract is enforced by host guards, kernel error flags or tests:

| operation | in the verifier | contract enforced by |
|---|---|---|
| PARAM | per-segment parameter buffers `C`, `L`, `P`, `H`, `S`; pipeline overrides | `engine.js` guards before dispatch |
| CONST | WGSL `const`s, mirrored in `engine.js` and `core.cpp` | `test/contract_test.mjs` |
| INDEX | class addressing: dense bit b → class `phase_class[b mod 15]`, bit `b / 15` (`large.wgsl`); checker bit `t = i + U` | maps vs reference (`test/map_test.mjs`) |
| RECIP | f32 reciprocal inside `modq` (exact by construction, two corrections) | `test/modq_test.mjs` (1M cases vs BigInt) |
| MUL, ADD | state advance `(s − delta mod p) mod p`, 64-bit products in u32 halves | `test/states_test.mjs`, golden hashes |
| FLOORDIV, FLOORMOD | wheel indices mod 30030, `X / delta`, `X mod delta` | `test/states_test.mjs`, golden hashes |
| WHERE | `select` (branch-free bounds and corrections) | the tests above |
| CALL | WGSL functions (`modq`, `clear_bit`, `cwords`), WASM exports | per-function tests (`modq_test`, `core_test`) |
| STACK | 8 class arrays of the map; 15 residue-class rows of the histogram | `HSTRIDE` contract, golden hashes |
| CAST | u32 → f32 in `modq`; BigInt → u32 at the host/shader boundary | host guards (`< 2^32`, `delta < 2^31`) |
| CMPLT | bounds (`s < q_odds`), ascending candidate order | checker design (`check.wgsl` header) |
| BUFFER | GPU buffers made in `prepare` (map, primes, states, lists, io) | validation error scope, binding-limit guards |
| ALLOC | WASM `_gb_malloc` temporaries; workgroup arrays (`win`, `hwg`) | `test/contract_test.mjs` (array sizes) |
| LOAD, STORE | storage reads and writes, atomics | maps and statistics vs reference |
| AFTER | queue order (`writeBuffer` before its submit, encoder order), `workgroupBarrier`, readback maps | `test/lists_test.mjs` (per-segment regions read after the GPU) |
| RANGE, END | the segment loop (`runAll`), hit-list batches of K segments, kernel loops | accounting check: every even counted once per segment |
| RESHAPE | dense bits ↔ 8 class arrays of `cw32` words; segment ↔ 16 KB tiles | `test/tile_test.mjs`, maps |
| EXPAND | one candidate tested against 32 evens per u32 word | golden hashes, run vs native `goldbach_l0` |
| PERMUTE | phase-A class windows (a transposed layout was measured: #106) | golden hashes |
| SHRINK | tail masks (`range_mask`, the last word of a class), the final partial segment | maps at both range ends, golden windows |
| REDUCE | popcount sums (count30), histogram and first occurrences (atomics) | statistics vs native, per-segment accounting |

## Measurements that decide it

* The time is in memory-bound passes and data movement, not arithmetic. Full
  mode per segment: check_a 2.74 ms, large 2.35, tile 0.91; the gaps between
  them come from readback bytes and per-slice setup (`docs/BASELINE.md`).
* Rewrites of arithmetic were measured and did not pay off: #107 (f32-reciprocal
  modulo instead of emulated u32 division in the large kernel) −0.32 %
  [−0.90, +0.25]; #108 (the same in phone mode) not faster; #106 (transposed
  windows, a PERMUTE) not faster.
* The gains found in this round are structural and need no IR: readback of
  empty list capacity (#113, ~−2.7 % expected), per-slice states on the GPU
  (`opt/gpu-states`, setup 475 ms per slice).

## Decision

No general operation compiler. It would duplicate the existing resource setup
(§3.2 of the guide warns against that), its rewrites target the arithmetic that
measurements show is not the bottleneck, and generated kernels would have to
beat hand-tuned ones that already hold up against such rewrites.

What the operation model is good for is kept, as tests that cost nothing at
run time:

* **contracts**: `test/contract_test.mjs` checks constants and array sizes that
  the host, the core and the kernels each define;
* **exact rewrites**: a transformation of the arithmetic is tested against an
  unreduced BigInt reference of the core's definition (`test/states_test.mjs`
  with `opt/gpu-states`, `test/modq_test.mjs`), and against the core itself on real prime ranges;
* **memory order**: per-segment regions and the rule that region k is reused
  only after `collect(k)` are tested end to end (`test/lists_test.mjs`, with #113).

Revisit this when many device-specific kernel variants are needed (guide §7):
generating variants from one description could then save duplicated code. The
test to pass first is the same: a measured gain or a check that is easier to
prove.
