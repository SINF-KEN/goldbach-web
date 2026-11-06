# goldbach-web: 25-Operation and Optimization Roadmap

## Purpose

Improve `goldbach-web`, the WebGPU + WebAssembly Goldbach verifier and coordinator. Keep every result exact while reducing the time required to verify and accept a slice across desktop and low-memory devices.

This guide combines two workstreams:

1. **The 25-operation roadmap:** a possible typed operation layer for representing, simplifying, and lowering computations.
2. **The repository optimization roadmap:** improve the existing WebGPU kernels, WASM core, browser worker, prime cache, coordinator, CPU verifier, tests, and benchmark CI.

The repository currently uses C++/WASM, WGSL kernels, and a JavaScript WebGPU pipeline. Do not assume that a general-purpose 25-operation compiler already exists. Treat the operation layer as a design workstream. Add it only when it solves a measured problem or makes a required transformation easier to prove and test. Do not add an abstraction that slows down or complicates the verifier without a clear benefit.

## Rules for Every Change

1. **Correctness comes first.** Never accept a speed improvement that changes a result or weakens a safety check.
2. **Measure the full slice.** Use GPU pass timings to explain results. Use slice wall time as the main measure of user-visible performance.
3. **Compare against current main.** Benchmark the exact candidate that would merge.
4. **Change one main variable at a time.** This makes the cause of a result easier to identify.
5. **Keep a baseline.** Record the commit, device, software version, mode, segment size, cache state, test window, and result.
6. **Fail closed.** List overflow, device errors, incomplete accounting, bad hashes, failed self-tests, and failed cross-checks must reject the work.
7. **Protect production.** Use the existing benchmark workflow and shared GPU lock. Do not run competing GPU work during a benchmark.
8. **Prove value before adding complexity.** Every new compiler layer, kernel variant, cache, or tuning rule must have a clear purpose and a test.

## 1. Establish a Correct Baseline

Run the existing checks before changing code:

```bash
bash core/build.sh
bash core/build.sh native
bash server/build_cpu.sh
python3 test/server_test.py
node test/pcache_test.mjs
node test/live_update_test.mjs
node test/worker_e2e.mjs 3
bash test/golden.sh cpu
bash tools/ci_local.sh
```

For GPU testing, follow the project procedure and do not run the tests alongside the production campaign:

```bash
bash test/golden.sh gpu
node test/tile_test.mjs
node test/map_test.mjs START LIMIT SEG K
node test/run_test.mjs START LIMIT [SEG] [K]
node test/prof.mjs [START LIMIT SEG]
```

Use the exact argument forms supported by each test. Save the current main commit and the test output. Do not start performance experiments until the relevant baseline passes.

The existing tests cover golden results, tile sieving, map generation, full statistics, base-prime caching, worker flows, and coordinator behaviour. Keep these tests. Add new tests for new risks.

## 2. Locate the Real Bottleneck

Use `node test/prof.mjs` to measure GPU pass time. Use the benchmark CI to measure full-slice time. These measurements answer different questions:

- **GPU pass time** identifies the cost of each shader pass.
- **Slice wall time** includes setup, uploads, submissions, pipeline gaps, host work, and segment scheduling.
- **Startup time** includes WebGPU setup, pipeline creation, WASM loading, and base-prime generation or loading.
- **Long-run stability** reveals changes caused by memory pressure, heat, device loss, or competing work.

Measure full mode and phone/lite mode separately. They can have different bottlenecks and memory limits.

Do not optimize a pass only because it has the largest timing. First establish whether reducing that pass can reduce the full-slice time.

## 3. Roadmap Part A: Build a Clear Operation Model

**Goal:** Make each operation's meaning, inputs, outputs, and correctness rules clear before considering graph rewrites or code generation.

Use the 25 operations below as a typed operation design if an intermediate representation (IR) is justified. The list is not a claim that the current verifier already contains these operations. First identify which operations the real Goldbach execution path needs.

For every operation, define:
- Its inputs and their valid types and ranges.
- Its output type and behaviour.
- The conditions required for a correct result.
- The error behaviour for invalid inputs or state.
- The rewrites that are allowed, with a correctness argument and tests.

### 3.1. Operations and Rewrites — 13 operations

Build operation graphs and simplify expressions. Use constant values when possible. Apply only rewrites that preserve the program's results.

1. **PARAM** — Define an input to a function.
2. **CONST** — Define a fixed value.
3. **INDEX** — Add index axes to an operation.
4. **RECIP** — Calculate the reciprocal of a value (1/x).
5. **MUL** — Multiply values.
6. **FLOORDIV** — Divide values and round the result down.
7. **WHERE** — Select a value based on a condition.
8. **CALL** — Call a function.
9. **STACK** — Add a leading axis to data.
10. **CAST** — Convert a value to another data type.
11. **ADD** — Add values.
12. **CMPLT** — Test if one value is less than another.
13. **FLOORMOD** — Calculate the remainder after floor division.

**Goldbach-specific checks:** Define integer widths and signedness. Document overflow rules. Do not simplify expressions if the rewrite changes integer overflow behaviour, division semantics, rounding, or exact results. Check whether the operation belongs in the CPU core, the WGSL shader, or both.

**Milestone:** Each operation has a clear contract. Every enabled rewrite has tests that compare its result with an unreduced expression or a trusted reference.

### 3.2. Code Generation, Runtime, and Memory — 5 operations

Generate code and execute it with controlled memory access. Define how a program reads and writes data. Set the correct order for memory operations.

14. **BUFFER** — Create a global buffer that keeps data available.
15. **ALLOC** — Reserve storage for a function call.
16. **LOAD** — Read data from memory.
17. **STORE** — Write data to memory.
18. **AFTER** — Set the required order of memory operations.

**Goldbach-specific checks:** Connect buffer definitions to WebGPU device limits, storage-buffer limits, binding sizes, and memory budgets. Define which operation owns each buffer and how long the buffer must remain valid. Keep the required order of writes, dispatches, barriers encoded through WebGPU command ordering, and readbacks. Reject configurations that can exceed a device limit.

Avoid adding an IR layer that only repeats the existing JavaScript resource setup. It must enable a useful optimisation, a clearer safety check, or a verifiable code-generation rule.

**Milestone:** Code reads and writes data in the required order. Resource sizes and lifetimes are checked before dispatch.

### 3.3. Loops — 2 operations

Represent repeated work with loop operations. Support calculations that repeat across a range of values.

19. **RANGE** — Repeat an operation for each value from zero to the end value minus one.
20. **END** — Close a range and pass its final value to the next operation.

**Goldbach-specific checks:** Keep segment boundaries, carried prime states, final partial segments, and loop exit conditions exact. Do not skip or process a segment twice. Check that any loop transformation preserves candidate order and complete accounting.

**Milestone:** Repeated work has explicit bounds. Boundary tests prove that each expected value is processed exactly as required.

### 3.4. Data Movement and Rangeify — 5 operations

Transform operations on shaped data into simpler loops. Change data shapes, reorder axes, select parts of data, and combine values.

21. **RESHAPE** — Change the shape of data without changing its elements.
22. **EXPAND** — Add dimensions so that data can be broadcast across an operation.
23. **PERMUTE** — Change the order of data axes.
24. **SHRINK** — Select a smaller range along one or more dimensions.
25. **REDUCE** — Combine values across selected axes, for example by addition.

**Goldbach-specific checks:** Specify the meaning of each axis and the mapping between logical elements, packed words, residue classes, and segment offsets. Check tail masks and invalid elements. A reduction must not drop or count any even twice. Axis movement must preserve the association between an even number and its statistics.

**Milestone:** Data transformations have clear shape rules and exact tests. Add only the transformations needed by the real computation.

### 3.5. Decide Whether the Operation Layer Is Worth Keeping

Before implementing all 25 operations, compare the cost and benefit of a typed IR with direct, well-tested host and WGSL code.

Keep the IR only if it provides a measurable or verifiable benefit, such as safer code generation, reusable rewrites, fewer duplicated calculations, or clearer tests. Measure compile/setup costs and runtime costs. A generic compiler layer is not automatically faster than hand-written kernels.

## 4. Optimize the Existing WebGPU Pipeline

Main file: `client/engine.js`.

Measure each pass and the gaps between passes before changing the pipeline. Check for:

- Unnecessary command submissions and repeated setup.
- Repeated buffer writes when the data has not changed.
- Resources that can be reused safely between slices.
- Uploads or readbacks that prevent useful GPU work.
- Pipeline or bind-group creation that can be reused.
- Segment concurrency that improves wall time without excessive memory use.
- Unchecked resource sizes or dispatch bounds.

The repository already has persistent engine state between slices, write-buffer uploads, and multiple segments per submit. Measure any proposed change against that current baseline.

Do not increase concurrency or memory use without testing both full and lite modes.

## 5. Optimize the Kernels

Shader files:

- `kernels/tile.wgsl`
- `kernels/large.wgsl`
- `kernels/harvest.wgsl`
- `kernels/harvest_lite.wgsl`
- `kernels/check.wgsl`
- `kernels/count30.wgsl`

For each kernel, document its input bounds, output bounds, state changes, and error conditions before modifying it.

### Tile sieve

Measure mask-table work, prime marking, medium-prime marking, and map write-out. Test supported workgroup sizes and prime splits in controlled runs. Confirm that all valid bits are handled and invalid tail bits are cleared.

### Large-prime marking

Measure packed-prime work and hit-list work separately. Test workgroup size and primes-per-invocation settings. Preserve the exact state carried between segments. Check every host-to-shader bound and index calculation.

### Hit-list harvest

Measure list generation, atomic counters, and capacity use. Do not reduce capacity or remove overflow checks to improve timing. Test dense and sparse cases, batch boundaries, and the final partial batch.

### Checker

The checker must test candidate primes in ascending order. The first successful candidate determines the exact minimum Goldbach prime, p(n). Preserve this order and all statistics that depend on it.

Measure phase A and phase B separately. Check pending-list capacity, ridge-list capacity, leftover handling, and tail behaviour. Any list overflow must cause the slice to fail closed.

### Count and statistics kernels

Confirm that each even and each required residue class contributes exactly as specified. Preserve integer widths, bounds, and all published statistics. Compare the complete canonical result, not only the total count.

**Milestone:** Every kernel change passes the relevant reference tests and improves a stated metric, or it is not accepted as a performance change.

## 6. Preserve Exact Arithmetic and Mathematical Meaning

The GPU kernels do not use native 64-bit integer arithmetic. The host core uses exact integer arithmetic to calculate values that the shaders cannot safely calculate.

Before changing arithmetic:

- Prove the valid input range.
- Check intermediate values for overflow.
- Check every conversion between host values and shader values.
- Do not replace exact integer calculations with floating-point approximations unless the exactness argument remains valid and is tested.
- Test values at the start and end of the supported range.
- Test segment boundaries, residue-class boundaries, and the final partial segment.
- Compare GPU output with an independent CPU implementation.

Keep the state rules for large primes and hit-list primes intact unless the new design has a complete correctness argument and test coverage.

For every mathematical rewrite, write down why it preserves results. Include the integer type, valid range, boundary behaviour, and any required ordering. A comment is useful context, but it does not replace the proof or the test.

## 7. Improve Segment Size and Kernel Settings

Segment size, workgroup size, prime split, and other settings can behave differently across devices. Do not choose one configuration based only on the Arc B580.

Build a repeatable benchmark matrix that covers:

- Desktop and lite mode.
- Multiple segment sizes.
- Different supported workgroup sizes.
- Different prime split values.
- Different primes-per-invocation values.
- Short and long windows.
- Cold and warm prime-cache states when startup is part of the goal.

For automatic tuning:

1. Query adapter limits and device capabilities.
2. Build a list of configurations that meet known safety limits.
3. Run a warm-up and correctness check for each candidate.
4. Measure repeated runs on a representative window.
5. Select the fastest configuration that passes all checks.
6. Save the result with an adapter and software-version key.
7. Revalidate cached settings when the engine version or device changes.
8. Keep a safe fallback when measurement or a candidate fails.

Do not rely only on user-agent strings to identify performance. Use device limits and measured results. Keep lite mode available for devices with small memory limits.

**Milestone:** The tuning system selects a safe configuration based on device capability and measured performance. Different valid settings must produce identical canonical results.

## 8. Reduce Startup and Cache Costs

Relevant files: `client/pcache.js`, `client/worker.js`, and `client/selftest.js`.

Base primes are generated once per device and then cached. Preserve the cache checksum and known prime fingerprint checks.

Measure cache lookup, validation, generation, and load times. Check for duplicate prime generation when workers or tabs start. Consider whether pipeline creation can overlap safe preparation work.

A cache hit must never bypass integrity checks. Reject and rebuild a corrupt or incomplete cache. Keep the device self-test before a worker contributes results.

## 9. Improve Worker Scheduling and Recovery

Relevant file: `client/worker.js`.

The worker leases slices, verifies them, retries failures, and handles device recreation and engine updates. Preserve these behaviours.

Measure time spent waiting for a lease, verifying a slice, uploading a result, retrying, and recreating a device. Include network latency and full-slice time. Do not judge scheduling from GPU throughput alone.

For a second adapter, confirm that it is a different physical GPU. Account for shared system memory on integrated GPUs. Use lite mode when required by memory limits. Prevent duplicate work from being counted twice. Test device loss, retry, stop, and live-update paths.

## 10. Keep the Coordinator, Auditor, and CPU Verifier Strict

Relevant files:

- `server/server.py`
- `server/auditor.mjs`
- `server/cpu_worker.cpp`
- `server/cpu_worker.py`

The coordinator must reject malformed, incomplete, or inconsistent submissions. Preserve independent agreement on the canonical result hash and the dispute path.

Test coordinator changes with `python3 test/server_test.py` and `node test/worker_e2e.mjs 3`. Test auditor changes with its self-test and known-answer checks.

Do not trust a client-reported speed, count, or completion flag as proof of correctness. Treat browser submissions as untrusted until the existing validation and agreement rules accept them.

When optimizing the CPU verifier, use the dedicated CPU benchmark and representative server settings. Compare full CPU slice time with the old CPU version, not with GPU pass time.

## 11. Benchmark Changes Correctly

Use the existing benchmark system:

- `.github/workflows/bench.yml`
- `tools/bench_ci.sh`
- `test/bench_ab.mjs`
- `server-rs/src/bin/goldbach-tools/bench_report.rs` (`goldbach-tools bench-report`)
- `server-rs/src/bin/goldbach-tools/bench_cpu.rs` (`goldbach-tools bench-cpu`)

The benchmark compares the candidate with main in the same process and uses alternating ABBA runs. It checks exact results, measures both modes, and reports confidence intervals.

For each experiment:

1. Create a branch for one focused change.
2. Run correctness tests first.
3. State the expected improvement before benchmarking.
4. Measure main and candidate on the same device and windows.
5. Use enough repetitions to separate a real change from noise.
6. Read slice wall time first; use pass timings to explain the result.
7. Check that no other gating metric has a significant regression.
8. Keep the report with the commit or pull request.

The existing benchmark policy requires exact output and a 95% confidence interval whose lower bound exceeds +1% in at least one gating metric, with no significant regression in another. Follow the current CI verdict and policy. Do not merge a change that the benchmark rejects.

## 12. Add Tests for New Risks

Keep fast, repeatable tests in normal CI. Use the hardware benchmark pipeline for performance measurements on the real GPU.

Add tests when a change introduces a new risk, including:

- Random segment sizes and boundaries.
- Small and maximum supported buffer sizes.
- List-capacity boundaries and forced overflow.
- Device loss during a slice.
- Cache corruption and cache-version changes.
- Two-worker and multi-adapter scheduling.
- Results across different valid segment sizes.
- Long-run memory growth and device stability.
- New IR operations and rewrite rules, if an IR is added.
- CPU/GPU agreement for all result fields affected by a change.

A failed correctness test must stop a candidate before performance results can lead to a merge.

## 13. Code Comments and Documentation

**Goal:** Make the code easy to understand, use, test, and maintain.

Follow the University of Washington CSE 123 commenting guide as a practical style reference. Adapt it to this C++/JavaScript/WGSL/Python project. Use one consistent style within each language. Do not add irrelevant course or personal details to file headers.

### 13.1. File and Module Comments

Add a short header when it helps explain a file's purpose.

- Explain the main purpose of the file.
- Describe the main functionality it provides.
- State important relationships with other components when needed.
- Do not add personal or course details that do not apply to the project.

### 13.2. Class and Type Comments

Document each public class or type that provides functionality to other code.

- Explain what the class or type represents.
- Describe the behaviour that callers can expect.
- Explain how to use it when the public interface is not enough.
- Do not list internal fields or describe code line by line.

### 13.3. Function and Method Comments

Document public functions and methods when the contract is not clear from the interface. Write comments so a reader can understand how to use the function without reading its implementation.

Include the following items when they apply:

- **Behaviour:** what the function does.
- **Parameters:** each input and its valid values.
- **Returns:** the result and what it means.
- **Exceptions or errors:** the conditions that cause failure and how failure is reported.

Use either BERP (Behaviour, Exceptions, Returns, Parameters) or pre-condition/post-condition comments. Do not repeat the same information in two formats.

### 13.4. Pre-Conditions and Post-Conditions

Use pre-conditions and post-conditions when they make a function contract easier to understand.

- **Pre-condition:** State what must be true before the function starts. Include valid input ranges and required state.
- **Post-condition:** State what the function guarantees when it finishes. Describe the output or any change to shared state.
- **Invalid input:** Document the error behaviour when a function rejects invalid input.
- **Correctness:** State the conditions required for a correct result.

Do not claim a guarantee that the implementation and tests do not support.

### 13.5. Private Functions and Internal Logic

Document private functions when this helps developers maintain the code. Explain their behaviour, inputs, outputs, important constraints, and assumptions.

Private comments may include implementation details. Public comments should focus on behaviour and the interface.

For Goldbach code, document non-obvious mathematical invariants, carried state, packed layouts, index bounds, overflow limits, and host/shader agreements. Explain why they must remain true.

### 13.6. Inline Comments

Use inline comments only when they explain something that is not clear from the code. Use them for complex mathematical steps, non-obvious correctness conditions, hardware constraints, or performance choices that must remain valid.

Keep each comment close to the code it explains. Do not add a comment to every line.

### 13.7. Remove Unnecessary Comments

- Remove comments that repeat the code.
- Correct comments that do not match the implementation.
- Remove obsolete comments and unused documentation.
- Replace vague comments with clear explanations.
- Move design information to the right document when it does not belong beside the code.
- Improve unclear code instead of writing a long comment to explain avoidable complexity.

### 13.8. Comment Quality Checks

Ask these questions for every important comment:

1. Can a reader understand the behaviour from the comment?
2. Are the relevant inputs and outputs described?
3. Are errors and invalid inputs documented?
4. Are pre-conditions and post-conditions accurate?
5. Does the comment explain behaviour instead of repeating code?
6. Does the comment match the current implementation?
7. Is it clear, concise, and consistent with other comments?

**Milestone:** Important code has accurate and useful comments. Public interfaces explain how to use them. Internal comments explain mathematical rules and constraints. No comment is misleading, obsolete, or redundant.

## 14. Final Acceptance Checklist

A change is ready only when all relevant items are complete:

- [ ] The baseline and relevant tests pass.
- [ ] Golden result hashes match.
- [ ] GPU output matches the independent CPU result.
- [ ] Boundary cases and invalid states are tested.
- [ ] Overflow and device-error checks remain active.
- [ ] Full and lite modes pass.
- [ ] New operation contracts and rewrites pass their tests, if an IR is used.
- [ ] The benchmark compares the candidate with current main.
- [ ] Slice wall time shows the intended improvement.
- [ ] No other gating metric has a significant regression.
- [ ] Startup, memory use, and long-run behaviour remain acceptable.
- [ ] Comments and documentation match the final implementation.
- [ ] The benchmark CI accepts the change.

## Recommended Priority

1. **Establish the baseline.** Measure full mode and lite mode. Save results from current main.
2. **Find the bottleneck.** Use pass timing and slice wall time to separate kernel cost from setup and transfer cost.
3. **Improve the existing pipeline.** Remove avoidable overhead before adding a new abstraction.
4. **Tune hot kernels.** Change one parameter or algorithm at a time. Keep exactness tests.
5. **Evaluate the 25-operation model.** Add an IR only if it improves correctness, maintainability, code generation, or measured performance. Do not implement unused operations to meet a count.
6. **Add safe device tuning.** Select settings from actual device limits and repeatable measurements.
7. **Improve startup and scheduling.** Measure cache, lease, transfer, retry, and recovery costs.
8. **Strengthen tests.** Cover each new failure mode and keep exactness requirements strict.
9. **Document and maintain.** Remove stale comments and record accepted benchmark results.

## Final Goal

The target is not the fastest single kernel, nor the largest number of compiler operations. The target is the highest rate of correctly verified and accepted Goldbach work across supported devices.

Every accepted optimization must preserve exact mathematical results, pass independent checks, and show a repeatable benefit. Every added abstraction must justify its cost. The tests, comments, and documentation must describe the implementation that actually runs.
