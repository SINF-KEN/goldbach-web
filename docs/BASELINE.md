# Baseline and bottlenecks

Record for `docs/OPTIMIZATION_GUIDE.md` §1–2: what was measured, on what, and
what it showed. Add a dated section per measurement session; keep the old ones.

## 2026-10-12: main 5d55cb4

**Machine:** Intel Arc B580 (Mesa 26.2.4, Vulkan), Node.js 26.10 with Dawn
(`test/gpu.mjs`); production paused at a slice boundary for every GPU run
(`tools/gpu_slot.sh`).

**Correctness baseline:** `bash tools/ci_local.sh` passes: coordinator logic,
core test, live updates, base-prime cache, maps vs the reference (phone mode,
both ends of the range), modq vs BigInt (1M), self-test hash 796ed43bc0e8be34,
fail-closed full engine on a small device, worker end to end, CPU golden windows.

**Benchmark CI numbers** (latest report, PR #107, per 233.75M evens):

| mode | GPU pass sum | slice wall | wall per segment | setup per slice |
|---|---:|---:|---:|---:|
| full (467.5M segments) | 7.03 ms | 8.02 ms | 7.80 ms | 475 ms |
| phone (500M segments) | 9.13 ms | 9.54 ms | 10.20 ms (per 250M evens) | ~0 |

### Where the wall time goes (`test/timeline.mjs`)

Pipelined runs as the worker runs them (64 segments, batch 2, depth 6), every
pass stamped on one GPU clock. Plain wall time equals the GPU span: the host
keeps the queue full, so host-side encoding is not a bottleneck.

| ms per segment | full | phone |
|---|---:|---:|
| plain wall | 7.35 | 10.27 |
| in passes | 6.95 | 9.84 |
| gaps between passes of a segment | 0.04 | 0.04 |
| gaps between segments of a submit | 0.15 | 0.15 |
| gaps between submits | 0.23 | 0.25 |

Ablations (timing only, results not checked) locate the gaps:

| full mode, wall ms per segment | |
|---|---:|
| main | 7.35 |
| no readback copies | 7.11 (−3.3 %) |
| every readback copy cut to 256 B | 7.12 (−3.0 %) |
| no ridge + leftover copies (80 KB of mostly empty capacity) | 7.15 (−2.7 %) |
| no histogram + first-occurrence copies (96 KB) | 7.31 (−0.5 %) |
| no staging copies (per-segment inputs) | 7.44 (no gain) |

The bytes copied cost, not the number of copies: the full capacity of the
ridge and leftover lists went back to the host every segment. → PR #113.

The rest of the gap is the per-slice setup in full mode (475 ms per 1e12 slice,
~2.8 %): the WASM core computes the packed and hit-list states of ~109M primes
and ~0.8 GB go up. → branch `opt/gpu-states` (the states computed on the GPU).

Not pursued: submit boundaries (batch 1 and 4 were measured in #104/#105,
not faster; the remaining per-submit gap after #113 is ~0.09 ms per segment).

### Accepted (benchmark CI, exact on every gate)

| PR | change | full slice wall | phone slice wall |
|---|---|---:|---:|
| #113 | ridge/leftover lists: per-segment regions, used part read back | +3.22 % [+2.67, +3.77] | +2.31 % [+1.96, +2.65] |
| #115 | per-run states on the GPU (setup 464 → 4 ms per slice) | +4.38 % [+3.62, +5.13] | −0.01 % [−0.07, +0.04] |

Full mode 8.020 → 7.429 ms per 233.75M evens (29.14 → 31.46 G evens/s, +8.0 %);
phone mode 9.535 → 9.307 (24.51 → 25.12 G evens/s, +2.5 %). Remaining
non-kernel time in full mode: ~0.09 ms per segment at submit boundaries and
~0.04 ms for the histogram readback; the rest is in the passes (check_a 39 %,
large 34 %, tile 13 %).
