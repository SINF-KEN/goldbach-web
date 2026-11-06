// goldbach-web live paper (/paper): the figures and tables of Oliveira e Silva,
// Herzog and Pardi, Math. Comp. 83 (2014), recomputed from the native verifier's
// statistics (/api/results, server/sync_native.py paper()) next to their data up
// to 4e18 (paper.json, tools/make_paper_json.py). Plain SVG, no libraries.
const $ = (id) => document.getElementById(id);
const X0 = 4e18;
const C2 = 0.6601618158468695739;          // twin primes constant
const ln = Math.log;
const sp = (s) => String(s).replace(/\B(?=(\d{3})+(?!\d))/g, " ");
const e18 = (x) => `${(x / 1e18).toFixed(6)}e18`;

// ---- primes up to 9781 (the p(n) range of the paper's table) ----------------
const PMAX = 9781;
const sieve = new Uint8Array(PMAX + 1).fill(1);
for (let i = 2; i * i <= PMAX; ++i) if (sieve[i]) for (let j = i * i; j <= PMAX; j += i) sieve[j] = 0;
const ODD = [];                              // odd primes ascending: index = the stats lists' index
const PI = new Map();                        // p -> pi(p)
const DELTA = new Map();                     // p -> pi(p; 3, 2) - pi(p; 3, 1)
{
  let n = 0, d = 0;
  for (let p = 2; p <= PMAX; ++p) {
    if (!sieve[p]) continue;
    PI.set(p, ++n);
    if (p % 3 === 2) ++d; else if (p % 3 === 1) --d;
    DELTA.set(p, d);
    if (p > 2) ODD.push(p);
  }
}

// ---- integrals for the predictions -------------------------------------------
// Simpson's rule on [a, b] (smooth integrands: 64 panels are far below 1 count)
function simpson(f, a, b, n = 64) {
  const h = (b - a) / n;
  let s = f(a) + f(b);
  for (let i = 1; i < n; ++i) s += f(a + i * h) * (i % 2 ? 4 : 2);
  return (s * h) / 3;
}
// Hardy-Littlewood density of evens with p(n) = p near x (paper eq. 2.4, per even)
function hlDensity(C, x) {
  const L = ln(x);
  let s = 0;
  for (const [k, c] of C) s += (k % 2 ? 1 : -1) * c / L ** k;
  return 2 * s;
}

// ---- a small scatter / line plot ---------------------------------------------
const W = 620, ML = 52, MR = 12, MT = 10, MB = 30;
const NS = "http://www.w3.org/2000/svg";
function el(name, attrs, parent) {
  const e = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  parent?.appendChild(e);
  return e;
}
function niceTicks(lo, hi, n = 6) {
  const raw = (hi - lo) / n, mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw);
  const out = [];
  for (let t = Math.ceil(lo / step) * step; t <= hi + step * 1e-9; t += step) out.push(+t.toFixed(12));
  return out;
}
/** svg: target; o: { x: [lo, hi], y: [lo, hi], ylog, xfmt, yfmt, xticks, yticks, refs: [y...], series:
 *  [{ pts: [[x, y]], cls, r, shape: "dot" | "circle" | "line" }] } */
function plot(svg, o) {
  svg.replaceChildren();
  const H = +svg.getAttribute("viewBox").split(" ")[3];
  const ty = (v) => (o.ylog ? Math.log10(v) : v);
  const [x0, x1] = o.x, [y0, y1] = o.y.map(ty);
  const X = (v) => ML + ((v - x0) / (x1 - x0)) * (W - ML - MR);
  const Y = (v) => H - MB - ((ty(v) - y0) / (y1 - y0)) * (H - MT - MB);
  const xt = o.xticks ?? niceTicks(x0, x1), yt = o.yticks ?? (o.ylog ? [] : niceTicks(o.y[0], o.y[1], 5));
  if (o.ylog) for (let e = Math.ceil(y0); e <= y1; ++e) yt.push(10 ** e);
  for (const t of xt) {
    el("line", { class: "grid", x1: X(t), x2: X(t), y1: MT, y2: H - MB }, svg);
    el("text", { class: "axis", x: X(t), y: H - MB + 14, "text-anchor": "middle" }, svg).textContent = (o.xfmt ?? String)(t);
  }
  for (const t of yt) {
    el("line", { class: "grid", x1: ML, x2: W - MR, y1: Y(t), y2: Y(t) }, svg);
    el("text", { class: "axis", x: ML - 6, y: Y(t) + 3, "text-anchor": "end" }, svg).textContent =
      (o.yfmt ?? ((v) => (o.ylog ? `1e${Math.round(Math.log10(v))}` : String(v))))(t);
  }
  for (const r of o.refs ?? []) el("line", { class: "ref", x1: ML, x2: W - MR, y1: Y(r), y2: Y(r) }, svg);
  el("rect", { class: "frame", x: ML, y: MT, width: W - ML - MR, height: H - MT - MB }, svg);
  if (o.xlabel) el("text", { class: "axis", x: W - MR, y: H - 2, "text-anchor": "end" }, svg).textContent = o.xlabel;
  if (o.ylabel) el("text", { class: "axis", x: ML + 4, y: MT + 10 }, svg).textContent = o.ylabel;
  if (o.progress != null && o.progress >= x0 && o.progress <= x1) {
    el("line", { class: "prog", x1: X(o.progress), x2: X(o.progress), y1: MT, y2: H - MB }, svg);
  }
  const inside = ([x, y]) => x >= x0 && x <= x1 && Number.isFinite(ty(y)) && ty(y) >= y0 && ty(y) <= y1;
  for (const s of o.series) {
    const pts = s.pts.filter(inside);
    if (s.shape === "line") {
      if (pts.length) el("path", { class: s.cls, d: pts.map(([x, y], i) => `${i ? "L" : "M"}${X(x).toFixed(1)},${Y(y).toFixed(1)}`).join("") }, svg);
      continue;
    }
    // one path of tiny arcs per series: thousands of points stay one DOM node
    const r = s.r ?? 1.6;
    const d = pts.map(([x, y]) => `M${(X(x) - r).toFixed(1)},${Y(y).toFixed(1)}a${r},${r} 0 1,0 ${2 * r},0a${r},${r} 0 1,0 ${-2 * r},0`).join("");
    el("path", { class: s.cls, d }, svg);
  }
  if (!o.series.some((s) => s.pts.some(inside))) {
    el("text", { class: "axis", x: W / 2, y: H / 2, "text-anchor": "middle" }, svg).textContent = o.empty ?? "no data yet";
  }
}

// ---- the figures ---------------------------------------------------------------
let ref = null, live = null;

function render() {
  if (!ref) return;
  if (!live?.range) {
    $("status").textContent = loadError || "waiting for the verifier's statistics (synced every 10 minutes)";
    return;
  }
  const x = Number(live.range[1]);
  const evens = (BigInt(live.range[1]) - BigInt(live.range[0])) / 2n + 1n;
  const age = live.time ? Date.now() / 1000 - live.time : Infinity;
  const updated = live.time ? new Date(live.time * 1000).toISOString().slice(0, 16).replace("T", " ") + " UTC" +
    (age > 3600 ? ` — ${(age / 3600).toFixed(1)} h old, the sync is behind` : "") : "unknown";
  const known = new Map(), paperL = new Map();
  for (const [p, S, L] of ref.t0) { if (S) known.set(p, S); paperL.set(p, Number(L)); }
  const unknown = new Set(ref.t0.filter((r) => !r[1]).map((r) => r[0]));
  const first = new Map(live.first.map(([p, n]) => [p, n]));
  const found = [...unknown].filter((p) => first.has(p)).sort((a, b) => a - b);
  const rest = [...unknown].filter((p) => !first.has(p)).sort((a, b) => a - b);
  $("status").textContent =
    `data: [4e18, ${e18(x)}]  ${sp(evens.toString())} evens verified  ·  updated ${updated}\n` +
    `new S(p) found: ${found.length} of ${unknown.size}  ·  sampled evens ${sp(live.sampledEvens)}`;

  // Figures 1-2: B580 time per 1e12 integers against x
  const f1 = live.slices.map(([s, l, g]) => [Number(s), g / ((Number(l) - Number(s) + 2) / 1e12)]).filter((p) => p[1] > 0);
  const y1max = Math.max(1, ...f1.map((p) => p[1])) * 1.1;
  plot($("fig1"), { x: [X0, 5e18], y: [0, y1max], xticks: niceTicks(X0, 5e18, 5), xfmt: (v) => `${(v / 1e18).toFixed(1)}e18`,
                    yfmt: (v) => `${v} s`, progress: x, xlabel: "x", ylabel: "seconds per 10^12 integers",
                    series: [{ pts: f1, cls: "new", r: 1.4 }] });

  // Table 4: records of p(n) from 4e18 upward (first occurrences ascending in n)
  const byN = [...live.first].sort((a, b) => (BigInt(a[1]) < BigInt(b[1]) ? -1 : 1));
  let best = 0;
  const rec = [];
  for (const [p, n] of byN) if (p > best) { best = p; rec.push([n, p]); }
  $("tab4").textContent = `${"n".padStart(27)}  p(n)\n` + rec.map(([n, p]) => `${sp(n).padStart(27)}  ${String(p).padStart(4)}`).join("\n");

  // Table 5: the new S(p)
  const q1 = (p, S) => { const L = ln(Number(S)); return p / (L * L * ln(L)); };
  $("tab5").textContent = found.length
    ? `${"p".padStart(5)}  ${"S(p)".padStart(27)}  Q1(p)\n` + found.map((p) => `${String(p).padStart(5)}  ${sp(first.get(p)).padStart(27)}  ${q1(p, first.get(p)).toFixed(4)}`).join("\n") +
      (rest.length ? `\n\nstill above ${e18(x)}: ${rest.length} primes, smallest ${rest[0]}` : "\n\nall 105 found")
    : "none yet";

  // Figure 3: Q1(p)
  const old3 = [...known].filter(([p]) => p > 2).map(([p, S]) => [p, q1(p, S)]);
  const new3 = found.map((p) => [p, q1(p, first.get(p))]);
  plot($("fig3"), { progress: rest[0], x: [0, 10000], y: [0, 1.8], refs: [1.51478, 1.70098], xlabel: "p", ylabel: "Q1(p)",
                    series: [{ pts: old3, cls: "old", r: 1.1 }, { pts: new3, cls: "new", r: 2.4 }] });

  // Figure 4: Q2(p, x) for p < 250 on the sampled evens, per snapshot
  const Cs = Object.entries(ref.C).map(([p, c]) => [Number(p), c]).filter(([p]) => p >= 3);
  const snaps = [...live.fig4];
  const cur = { x: live.range[1], evens: live.sampledEvens, counts: ODD.filter((p) => p < 250).map((p, i) => live.sampled[i] ?? 0) };
  if (!snaps.length || snaps.at(-1).x !== cur.x) snaps.push(cur);
  const pts4 = [];
  for (const s of snaps) {
    const xs = Number(s.x);
    for (const [p, C] of Cs) {
      const i = ODD.indexOf(p), L = s.counts[i];
      if (!(L > 20)) continue;
      // the sampled evens spread evenly over [4e18, x]: the mean density there
      const dens = simpson((t) => hlDensity(C, t), X0, xs, 8) / (xs - X0 || 1);
      const Lh = (xs > X0 ? dens : hlDensity(C, X0)) * s.evens;
      pts4.push([xs, (Lh - L) / Math.sqrt(2 * L * ln(ln(L)))]);
    }
  }
  plot($("fig4"), { progress: x, x: [X0, 5e18], y: [-1.5, 1.5], refs: [0], xticks: niceTicks(X0, 5e18, 5), xfmt: (v) => `${(v / 1e18).toFixed(1)}e18`,
                    xlabel: "x", ylabel: "Q2(p,x)", series: [{ pts: pts4, cls: "new", r: 1.6 }] });

  // Figure 5: Q3 and Q4 Q3 at x, L(p, x) = the paper's count to 4e18 + ours above it
  const tail = new Map(live.tail);
  const scale = Number(evens) / live.sampledEvens;
  const Lx = (p) => {
    const i = ODD.indexOf(p);
    const ours = p >= 3000 ? (tail.get(p) ?? 0) : (live.sampled[i] ?? 0) * scale;
    return (paperL.get(p) ?? 0) + ours;
  };
  const piRatio = (X0 / ln(X0)) / (x / ln(x));          // 1e-17 ~ 1 / pi(4e18), rescaled to x
  const k3 = 1 / (0.755 * ln(x) - 4.19);
  const q3 = [], q3c = [], q4 = [], q4c = [];
  for (const p of ODD) {
    const n = PI.get(p);
    if (n > 1000) break;
    const L = Lx(p);
    if (!(L > 0)) continue;
    const Q3 = 1e-17 * piRatio * Math.exp(k3 * n) * L;
    const Q4 = p % 3 === 1 ? 1 - 0.04 * DELTA.get(p) : 1 + 0.04 * DELTA.get(p);
    (p % 3 === 1 ? q3c : q3).push([n, Q3]);
    (p % 3 === 1 ? q4c : q4).push([n, Q4 * Q3]);
  }
  plot($("fig5a"), { progress: PI.get(rest[0]), x: [0, 1000], y: [0, 1.2], xlabel: "π(p)", ylabel: "Q3(p)",
                     series: [{ pts: q3c, cls: "newc", r: 1.9 }, { pts: q3, cls: "new", r: 1.6 }] });
  plot($("fig5b"), { progress: PI.get(rest[0]), x: [0, 1000], y: [0, 1.2], xlabel: "π(p)", ylabel: "Q4(p)Q3(p)",
                     series: [{ pts: [...q4, ...q4c], cls: "new", r: 1.2 }] });

  // Figure 6: Q5(p) for p > 1000
  const q5 = (p, S) => { const L = ln(Number(S)); return PI.get(p) / (0.755 * L * L - 0.755 * L * ln(L) - 4.19 * L); };
  plot($("fig6"), { progress: rest.find((p) => p > 1000), x: [1000, 10000], y: [0.8, 1.6], refs: [1], xlabel: "p", ylabel: "Q5(p)",
                    series: [{ pts: [...known].filter(([p]) => p > 1000).map(([p, S]) => [p, q5(p, S)]), cls: "old", r: 1.1 },
                             { pts: found.filter((p) => p > 1000).map((p) => [p, q5(p, first.get(p))]), cls: "new", r: 2.4 }] });

  renderGaps(x);

  // Table 11 and the count deviations
  const dev = [], devT = [];
  let lastRow = null;
  for (const [l, pc, tc] of live.counts) {
    const b = Number(l) + 1;
    const ep = simpson((t) => 1 / ln(t), X0, b), et = 2 * C2 * simpson((t) => 1 / ln(t) ** 2, X0, b);
    dev.push([b, 1e6 * (Number(pc) - ep) / ep]);
    devT.push([b, 1e6 * (Number(tc) - et) / et]);
    lastRow = { b, pc, tc, ep, et };
  }
  if (lastRow) {
    const { b, pc, tc, ep, et } = lastRow;
    const row = (name, a, e) => `${name.padEnd(13)}${sp(a).padStart(24)}${sp(Math.round(e)).padStart(24)}${(1e6 * (Number(a) - e) / e).toFixed(2).padStart(10)}`;
    $("tab11").textContent = `in (4e18, ${e18(b)}]      exact count              predicted       ppm\n` +
      row("primes", pc, ep) + "\n" + row("twin primes", tc, et) +
      "\n\nprediction: li(x) - li(4e18); twins 2·C2·∫dt/log²t, C2 = 0.66016…";
  }
  const all = [...dev, ...devT].map((p) => Math.abs(p[1]));
  const lim = Math.max(1, ...all) * 1.15;
  plot($("fig11"), { progress: x, x: [X0, 5e18], y: [-lim, lim], refs: [0], xticks: niceTicks(X0, 5e18, 5), xfmt: (v) => `${(v / 1e18).toFixed(1)}e18`,
                     yfmt: (v) => `${+v.toPrecision(3)}`, xlabel: "x", ylabel: "ppm",
                     series: [{ pts: dev, cls: "line1", shape: "line" }, { pts: devT, cls: "line2", shape: "line" }] });
}

// ---- 2.2 prime gaps --------------------------------------------------------------
// A(g, 2) = 2 C2 prod_{p | g, p > 2} (p - 1) / (p - 2)
function A2(g) {
  let a = 2 * C2;
  for (let p = 3, m = g; p <= m; p += 2) if (m % p === 0) { a *= (p - 1) / (p - 2); while (m % p === 0) m /= p; }
  return a;
}
const D_FIT = { 2: [0.99260, -3.7012, 7.7338], 3: [0.98357, -7.6839, 25.268], 4: [0.97109, -11.515, 51.238] };
const fact = [1, 1, 2, 6, 24];
const Dhat = (k, x) => { const L = ln(x); return fact[k] * x * L ** (k - 1) * D_FIT[k].reduce((s, d, n) => s + d / L ** n, 0); };

function renderGaps(x) {
  const G = live.gaps;
  const known = new Map(), paperN = new Map();
  for (const [g, P, N] of ref.g0) { if (P) known.set(g, P); paperN.set(g, Number(N)); }
  const unknown = new Set(ref.g0.filter((r) => !r[1]).map((r) => r[0]));
  const q6 = (g, P) => g / ln(Number(P)) ** 2;
  const q9 = (g, P) => { const L = ln(Number(P)); return g / ((0.960 * L - 3.58) * (L - 2 * ln(L) + ln(2))); };
  const old7 = [...known].filter(([g]) => g > 4).map(([g, P]) => [g, q6(g, P)]);
  const old10 = [...known].filter(([g]) => g >= 100).map(([g, P]) => [g, q9(g, P)]);
  if (!G || !G.range?.length) {
    const none = "starts with the release that adds the gap counter";
    $("gapstatus").textContent = `gap data: none yet (${none})`;
    for (const id of ["tab8", "tab9", "tab12"]) $(id).textContent = none;
    plot($("fig7"), { x: [0, 1500], y: [0.2, 1], xlabel: "g", ylabel: "Q6(g)", series: [{ pts: old7, cls: "old", r: 1.1 }] });
    plot($("fig8"), { x: [X0, 5e18], y: [-2, 2], refs: [0], xticks: niceTicks(X0, 5e18, 5), xfmt: (v) => `${(v / 1e18).toFixed(1)}e18`,
                      xlabel: "x", ylabel: "Q7(g,x)", series: [], empty: none });
    plot($("fig9"), { x: [0, 1000], y: [1.2, 1.7], xlabel: "g", ylabel: "Q8(g)", series: [], empty: none });
    plot($("fig10"), { x: [100, 1500], y: [0.8, 1.6], refs: [1], xlabel: "g", ylabel: "Q9(g)", series: [{ pts: old10, cls: "old", r: 1.1 }] });
    return;
  }
  const lo = Math.min(...G.range.map((r) => Number(r[0]))), hi = Math.max(...G.range.map((r) => Number(r[1])));
  const span = G.evens * 2;                                  // integers with gap data
  // Records (Table 8) and first occurrences (Table 9, Figures 7 and 10) need
  // the gap data contiguous from 4e18: only a gap whose lower prime lies in
  // [4e18, top] counts, top = the end of the merged ranges that start at 4e18
  // (BigInt: positions near 4e18 are beyond a double's exact integers)
  const segs = [];
  for (const [a, b] of G.range.map((r) => [BigInt(r[0]), BigInt(r[1])]).sort((x, y) => (x[0] < y[0] ? -1 : 1))) {
    if (segs.length && a <= segs.at(-1)[1] + 2n) { if (b > segs.at(-1)[1]) segs.at(-1)[1] = b; } else segs.push([a, b]);
  }
  const top = segs[0][0] <= BigInt(X0) + 2n ? segs[0][1] : null;
  const inPrefix = (p) => top !== null && BigInt(p) <= top;
  const holes = segs.slice(1).map((s, i) => `[${e18(Number(segs[i][1]))}, ${e18(Number(s[0]))}]`);
  const prefixNote = top === null
    ? `the gap data starts at ${e18(lo)}, not at 4e18: no first occurrence is certain yet`
    : holes.length ? `contiguous from 4e18 to ${e18(Number(top))} (the backfill fills ${holes.join(", ")}` +
      (live.backfill ? `: ${Number(live.backfill.pct).toFixed(2)} % done${live.backfill.eta_days != null ? `, ~${Number(live.backfill.eta_days).toFixed(1)} days left` : ""}` : "") +
      "); records and P(g) below that only"
    : `contiguous from 4e18`;
  const WG = live.webGaps;
  const webNote = !WG ? "" : Number(WG.n)
    ? `\nbrowsers' strip near 5e18 [${WG.range ? `${e18(Number(WG.range[0]))}, ${e18(Number(WG.range[1]))}` : "-"}]: ${sp(WG.n)} gaps on ${sp(WG.evens)} evens` +
      `, largest ${WG.max ? `${WG.max[0]} after ${sp(WG.max[1])}` : "-"}, ${WG.tail.reduce((a, t) => a + t[1], 0)} of 700 or more, D1 ${sp(WG.d[0])}` +
      `\n  (exact, ${sp(WG.states.single ?? 0)} + ${sp(WG.states.double ?? 0)} slices; not in the tables / figures: not contiguous with 4e18)` +
      (WG.states.missing ? `; ${sp(WG.states.missing)} slices verified before the gap counter have none` : "")
    : `\nbrowsers' strip near 5e18: no gap data yet (${sp(WG.states?.missing ?? 0)} slices verified before the gap counter)`;
  $("gapstatus").textContent =
    `gap data: [${e18(lo)}, ${e18(hi)}]  ${sp(G.n)} gaps  ·  largest ${G.max ? `${G.max[0]} after ${sp(G.max[1])}${G.max[2] ? "" : " (so far seen)"}` : "-"}\n` +
    `${prefixNote}\nsampled: ${sp(G.samp.reduce((a, b) => a + b, 0))} gaps on ${sp(G.sampledEvens)} evens` + webNote;

  // Table 8: record gaps (g >= 700 are tracked) by their first lower prime
  const tail = G.tail.filter((t) => inPrefix(t[2])).sort((a, b) => (BigInt(a[2]) < BigInt(b[2]) ? -1 : 1));
  let best = 0;
  const rec = [];
  for (const [g, , p] of tail) if (g > best) { best = g; rec.push([p, g]); }
  $("tab8").textContent = rec.length
    ? `${"p_k".padStart(27)}  g_k\n` + rec.map(([p, g]) => `${sp(p).padStart(27)}  ${String(g).padStart(4)}`).join("\n") +
      "\n\n(tracked from g = 700; the largest gap below 4e18 is 1476)" + (holes.length ? `\n${prefixNote}` : "")
    : top === null ? prefixNote : "no gap of 700 or more yet";

  // Table 9: new P(g)
  const firstG = new Map(tail.map(([g, , p]) => [g, p]).reverse());
  const found = [...unknown].filter((g) => firstG.has(g)).sort((a, b) => a - b);
  $("tab9").textContent = found.length
    ? `${"g".padStart(5)}  ${"P(g)".padStart(27)}  Q6(g)\n` + found.map((g) => `${String(g).padStart(5)}  ${sp(firstG.get(g)).padStart(27)}  ${q6(g, firstG.get(g)).toFixed(4)}`).join("\n") +
      (holes.length ? `\n\n${prefixNote}` : "")
    : top === null ? prefixNote : `none yet (${unknown.size} gaps g ≤ 1474 unseen up to 4e18)`;
  const gRest = [...unknown].filter((g) => !firstG.has(g)).sort((a, b) => a - b);
  const new7 = found.map((g) => [g, q6(g, firstG.get(g))]);
  plot($("fig7"), { progress: top === null ? null : gRest[0], x: [0, 1500], y: [0.2, 1], xlabel: "g", ylabel: "Q6(g)",
                    series: [{ pts: old7, cls: "old", r: 1.1 }, { pts: new7, cls: "new", r: 2.4 }] });
  plot($("fig10"), { progress: top === null ? null : gRest.find((g) => g >= 100), x: [100, 1500], y: [0.8, 1.6], refs: [1], xlabel: "g", ylabel: "Q9(g)",
                     series: [{ pts: old10, cls: "old", r: 1.1 }, { pts: found.filter((g) => g >= 100).map((g) => [g, q9(g, firstG.get(g))]), cls: "new", r: 2.4 }] });

  // Figure 8: Q7 on the sampled gaps, per snapshot (+ now)
  const As = Object.entries(ref.A).map(([g, a]) => [Number(g), a]);
  const snaps = live.fig4.filter((s) => s.gevens);
  const hiStr = G.range.map((r) => r[1]).reduce((a, b) => (BigInt(b) > BigInt(a) ? b : a));
  const cur = { x: hiStr, gevens: G.sampledEvens, gcounts: G.samp.slice(0, 107) };
  if (!snaps.length || snaps.at(-1).x !== cur.x) snaps.push(cur);
  const pts8 = [];
  for (const s of snaps) {
    const xs = Number(s.x);
    for (const [g, A] of As) {
      const N = s.gcounts[g / 2];
      if (!(N > 20)) continue;
      const dens = (t) => { const L = ln(t); return A.reduce((acc, [k, a]) => acc + (k % 2 ? -1 : 1) * a / L ** k, 0); };
      const mean = xs > lo ? simpson(dens, lo, xs, 8) / (xs - lo) : dens(lo);
      const Nh = mean * 2 * s.gevens;
      pts8.push([xs, (Nh - N) / Math.sqrt(2 * N * ln(ln(N)))]);
    }
  }
  plot($("fig8"), { progress: Number(hiStr), x: [X0, 5e18], y: [-2, 2], refs: [0], xticks: niceTicks(X0, 5e18, 5), xfmt: (v) => `${(v / 1e18).toFixed(1)}e18`,
                    xlabel: "x", ylabel: "Q7(g,x)", series: [{ pts: pts8, cls: "new", r: 1.4 }] });

  // Figure 9: Q8 at x_eff = 4e18 + the span with gap data
  const xe = X0 + span;
  const scale = G.sampledEvens ? G.evens / G.sampledEvens : 0;
  const exact = new Map(G.tail.map(([g, c]) => [g, c]));
  const k8 = 1 / (0.960 * ln(xe) - 3.58), norm = 5e-16 * (ln(xe) ** 2 / xe) / (ln(X0) ** 2 / X0);
  const pts9 = [];
  for (let g = 2; g <= 1000; g += 2) {
    const ours = g >= 700 ? (exact.get(g) ?? 0) : (G.samp[g / 2] ?? 0) * scale;
    const N = (paperN.get(g) ?? 0) + ours;
    if (N > 0) pts9.push([g, norm * Math.exp(k8 * g) * N / A2(g)]);
  }
  plot($("fig9"), { progress: top === null ? null : gRest.find((g) => g <= 1000), x: [0, 1000], y: [1.2, 1.7], xlabel: "g", ylabel: "Q8(g)", series: [{ pts: pts9, cls: "new", r: 1.2 }] });

  // Table 12: D1 exact; D2..D4 from the walked segments (every gap of the
  // sampled segments), scaled to all gaps, with the standard error of a mean
  const W = G.walk ?? { n: 0, mom: [] };
  const hm = (k) => G.samp.reduce((acc, c, i) => acc + c * (2 * i) ** k, 0) / Math.max(1, W.n);
  const rows = [[1, Number(G.d1), hi - lo, 0, true]];
  for (const k of [2, 3, 4]) {
    const est = W.n ? Number(W.mom[k - 1]) * (G.n / W.n) : NaN;
    const mean = hm(k), sd = Math.sqrt(Math.max(0, hm(2 * k) - mean * mean));
    rows.push([k, est, Dhat(k, hi) - Dhat(k, lo), W.n ? sd / mean / Math.sqrt(W.n) : NaN, false]);
  }
  $("tab12").textContent = `in [${e18(lo)}, ${e18(hi)}]\n  k  ${"D_k".padStart(26)}  ${"fit".padStart(12)}  D_k/fit\n` +
    rows.map(([k, d, f, rse, exact]) => `  ${k}  ${(exact ? sp(G.d1) : `${d.toExponential(9)}`).padStart(26)}  ` +
      `${f.toExponential(4).padStart(12)}  ${(d / f).toFixed(6)}${exact ? "  exact" : `  ±${(rse * (d / f)).toFixed(6)}`}`).join("\n") +
    `\n\nD1 exact (last prime minus the first one's predecessor); D2..D4 from the ${sp(W.n)} gaps of the sampled segments,` +
    "\nscaled to all gaps (± one standard error). The fit is the paper's Table 12 (N = 2).";
}

let loadError = "";
async function load() {
  try {
    const r = await fetch("/api/results", { cache: "no-cache" });
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const j = await r.json();
    // the sync's own time: stale data shows as stale
    live = { ...j.paper, tail: j.tail, webGaps: j.webGaps, backfill: j.backfill, time: j.paper?.synced ?? null };
    loadError = "";
  } catch (e) {
    loadError = `live data unavailable (${e.message}); retrying every minute`;
  }
  render();
}
fetch(document.querySelector('link[rel="preload"][as="fetch"]')?.href ?? "paper.json")
  .then((r) => { if (!r.ok) throw new Error(`HTTP ${r.status}`); return r.json(); })
  .then((j) => { ref = j; render(); })
  .catch((e) => { $("status").textContent = `reference data unavailable (${e.message})`; });
load();
setInterval(() => { if (!document.hidden) load(); }, 60 * 1000);
document.addEventListener("visibilitychange", () => { if (!document.hidden) load(); });
