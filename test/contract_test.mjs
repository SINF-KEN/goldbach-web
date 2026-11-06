// Host/shader agreements (no GPU): constants that client/engine.js, the WASM
// core and the WGSL kernels each define for themselves must agree, and the
// workgroup arrays must be large enough for the constants that index them.
// A change on one side only would otherwise surface as wrong results or a
// fail-closed run on some devices, not as a build error.
//   node test/contract_test.mjs
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url);
const read = (f) => readFileSync(new URL(f, root), "utf8");
let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures++;
};

/// Evaluates an integer constant expression of WGSL or JS (literals with an
/// optional u suffix, + - * / >> << and parentheses, names defined earlier).
function evaluate(expr, env) {
  const js = expr.replace(/\b(0x[0-9a-fA-F]+|\d+)u\b/g, "$1").replace(/\b([A-Z_][A-Z0-9_]*)\b/g, (n) => {
    if (!(n in env)) throw new Error(`unknown constant ${n} in "${expr}"`);
    return String(env[n]);
  });
  if (!/^[\d\sxa-fA-F+\-*/()<>]+$/.test(js)) throw new Error(`not a constant expression: ${expr}`);
  return Number(Function(`"use strict"; return (${js});`)());
}

/// The integer constants of a source file: `const NAME[: u32] = expr;` at the
/// start of a line (WGSL and JS alike; BigInt literals and other types skipped).
function constants(file) {
  const env = {};
  for (const m of read(file).matchAll(/^(?:export )?const ([A-Z_][A-Z0-9_]*)(?:: u32)? = ([^;]+);/gm)) {
    try { env[m[1]] = evaluate(m[2].replace(/\/\/.*$/, "").trim(), env); } catch { /* not an integer constant */ }
  }
  return env;
}

/// Sizes of the workgroup arrays of a kernel: `var<workgroup> name: array<T, N>`,
/// N a literal or a constant expression of the file's constants.
function workgroupArrays(file) {
  const env = constants(file);
  const out = {};
  for (const m of read(file).matchAll(/var<workgroup> (\w+): array<(?:[^<>,]|<[^<>]*>)+, ([^>]+)>;/g)) out[m[1]] = evaluate(m[2].trim(), env);
  return out;
}

const eng = constants("client/engine.js");
const check_ = constants("kernels/check.wgsl");
const tile = constants("kernels/tile.wgsl");

// ---- engine.js mirrors of kernel constants ----
check(eng.KA_MAX === check_.KA_MAX, `KA_MAX: engine ${eng.KA_MAX}, check.wgsl ${check_.KA_MAX} (C layout stride)`);
check(eng.KA_CNT === check_.KA_CNT, `KA_CNT: engine ${eng.KA_CNT}, check.wgsl ${check_.KA_CNT} (phase-A length cap)`);
check(eng.HSTRIDE === check_.HSTRIDE, `HSTRIDE: engine ${eng.HSTRIDE}, check.wgsl ${check_.HSTRIDE} (histogram stride)`);
check(eng.BLK_A === check_.BLK, `BLK_A: engine ${eng.BLK_A}, check.wgsl BLK ${check_.BLK} (evens per phase-A work-group)`);
check(eng.TILE_BITS === tile.TILE_BITS && eng.TILE_WORDS === tile.TILE_WORDS,
  `TILE_BITS/TILE_WORDS: engine ${eng.TILE_BITS}/${eng.TILE_WORDS}, tile.wgsl ${tile.TILE_BITS}/${tile.TILE_WORDS}`);
check(tile.TILE_BITS === 32 * tile.TILE_WORDS, `tile.wgsl TILE_BITS = 32 TILE_WORDS`);
for (const f of ["harvest", "harvest_lite"]) {
  const k = constants(`kernels/${f}.wgsl`);
  check(eng.HL_MAX_K === k.HL_MAX_K, `HL_MAX_K: engine ${eng.HL_MAX_K}, ${f}.wgsl ${k.HL_MAX_K}`);
}

// ---- engine.js and the WASM core ----
const core = read("core/core.cpp");
const jfar = core.match(/constexpr uint32_t HL_J_FAR = (0x[0-9A-Fa-f]+)u;/);
check(jfar !== null && Number(jfar[1]) === eng.HL_J_FAR, `HL_J_FAR: engine ${eng.HL_J_FAR}, core.cpp ${jfar?.[1]}`);

// ---- the wheel tables (core.cpp gb_wheel_tabs) as the large kernels address them ----
// [0, 30030) WHL_INV, [30030, 60060) WHL_IDX, [60060, 60060 + phi(30030)) WHL_STEP,
// then the CP15015 bit table: ceil(15015 / 32) words; 66290 words in all.
const TABS = 66290;
check(new RegExp(`std::fill\\(out, out \\+ ${TABS}, 0u\\)`).test(core), `core.cpp gb_wheel_tabs writes ${TABS} words`);
check(read("client/engine.js").includes(`_gb_malloc(${TABS} * 4)`) && read("client/engine.js").includes(`u32view(M, tabsP, ${TABS})`),
  `engine.js allocates and uploads ${TABS} table words`);
for (const f of ["large", "large_lite", "harvest", "harvest_lite"]) {
  const k = constants(`kernels/${f}.wgsl`);
  const has = (n) => n in k;
  if (has("T_INV")) check(k.T_INV === 0, `${f}.wgsl T_INV = 0`);
  if (has("T_IDX")) check(k.T_IDX === 30030, `${f}.wgsl T_IDX = 30030`);
  if (has("T_STEP")) check(k.T_STEP === 60060, `${f}.wgsl T_STEP = 60060`);
  if (has("WHL_N")) check(k.WHL_N === 5760 && (k.T_STEP ?? 60060) + k.WHL_N === 65820, `${f}.wgsl WHL_N = phi(30030) = 5760, WHL_STEP ends at T_CP`);
  if (has("T_CP")) check(k.T_CP === 65820 && k.T_CP + Math.ceil(15015 / 32) === TABS, `${f}.wgsl T_CP = 65820, CP15015 ends at ${TABS}`);
  if (has("WHL_MISS")) check(k.WHL_MISS === 0xFFFF && /0xFFFF/.test(core), `${f}.wgsl WHL_MISS = 0xFFFF (core: index of a non-unit)`);
}

// ---- check.wgsl workgroup arrays hold what the constants index ----
const wg = workgroupArrays("kernels/check.wgsl");
check(wg.win >= 8 * check_.WIN, `check.wgsl win (${wg.win}) >= 8 class windows of WIN = ${check_.WIN}`);
check(wg.cu === check_.KA_CNT / 4, `check.wgsl cu (${wg.cu}) = KA_CNT / 4 (4 candidates per vec4)`);
check(wg.hwg === (check_.KA_CNT / 2) * 8, `check.wgsl hwg (${wg.hwg}) = KA_CNT / 2 pairs x 8 slices`);

// ---- modq: one tested implementation (test/modq_test.mjs tests large_lite.wgsl's copy) ----
const modqBlock = (f) => {
  const t = read(`kernels/${f}.wgsl`), a = t.indexOf("// ---- modq"), b = t.indexOf("// ---- end modq");
  return a < 0 || b < 0 ? null : t.slice(a, b);
};
const tested = modqBlock("large_lite");
check(tested !== null, "large_lite.wgsl has the tested modq block");
for (const f of ["tile", "large", "harvest", "harvest_lite", "check", "count30"]) {
  const m = modqBlock(f);
  if (m !== null) check(m === tested, `${f}.wgsl modq block identical to the tested one`);
}

if (failures) { console.log(`CONTRACT FAILED (${failures})`); process.exit(1); }
console.log("CONTRACT OK");
