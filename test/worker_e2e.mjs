// End to end without a browser: the real client/worker.js (engine, prime cache,
// self-test, leases, heartbeats, lease-ahead, background result uploads) in
// Node.js against a local coordinator (the Rust server, temporary database,
// small slices), WebGPU from Dawn (test/gpu.mjs; lavapipe in the CI = phone mode).
//   node test/worker_e2e.mjs [SLICES]      (env GB_E2E_SLICE: integers per slice)
import "./gpu.mjs";
import { execFileSync, spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const want = Number(process.argv[2] ?? "3");
const root = fileURLToPath(new URL("..", import.meta.url));
const dir = await mkdtemp(join(tmpdir(), "gbweb-e2e-"));
const port = 18000 + Math.floor(Math.random() * 1000);
const base = `http://127.0.0.1:${port}`;
// the coordinator binary: GB_SERVER_BIN, else a build of server-rs (tools/ci_local.sh builds it first)
const bin = process.env.GB_SERVER_BIN ?? execFileSync("bash", [join(root, "server-rs/build.sh")], { encoding: "utf8" }).trim();
const srv = spawn(bin, ["--db", join(dir, "e2e.db"), "--static", join(root, "client"), "--port", String(port)],
  { env: { ...process.env, GB_SLICE: process.env.GB_E2E_SLICE ?? "4000000000" }, stdio: ["ignore", "pipe", "inherit"] });
process.on("exit", () => srv.kill());                 // never leave the test server behind
await new Promise((ok) => srv.stdout.once("data", ok));

// the worker's globals: fetch (relative URLs -> the server, file: URLs -> disk), messages
const realFetch = globalThis.fetch;
globalThis.fetch = async (u, init) => {
  const url = String(u instanceof URL ? u.href : u);
  if (url.startsWith("file:")) {
    const body = await readFile(fileURLToPath(url));
    return new Response(body, { headers: { "Content-Type": url.endsWith(".wasm") ? "application/wasm" : "text/plain" } });
  }
  return realFetch(url.startsWith("http") ? url : `${base}/${url.replace(/^\//, "")}`, init);
};
const msgs = [];
let done = 0, stopped;
const finished = new Promise((ok) => { stopped = ok; });
globalThis.postMessage = (m) => {
  msgs.push(m);
  if (m.type === "done") { done++; console.log(`slice #${m.slice}: ${m.status}, ${m.elapsed.toFixed(1)} s`); if (done === want) globalThis.onmessage({ data: { type: "stop" } }); }
  if (m.type === "status" && m.error) console.log("status:", m.text);
  if (m.type === "error") { console.log("ERROR:", m.text); stopped(); }
  if (m.type === "stopped") stopped();
};
globalThis.onmessage = null;                 // the worker assigns it (a module: must exist first)
await import("../client/worker.js");
const reg = await (await realFetch(`${base}/api/register`, { method: "POST", body: JSON.stringify({ name: "e2e", gpu: "test" }) })).json();
const t0 = performance.now();
globalThis.onmessage({ data: { type: "start", client: reg.client } });
await finished;
const secs = (performance.now() - t0) / 1000;

// the coordinator's view: every result accepted, no lease lost, no lease left active
const db = execFileSync(bin, ["dev", "lease-summary", join(dir, "e2e.db")], { encoding: "utf8" });
srv.kill();
await rm(dir, { recursive: true, force: true });
const v = JSON.parse(db);
const st = Object.fromEntries(v.leases);
const lostMsgs = msgs.filter((m) => m.type === "status" && /lost|Error/.test(m.text ?? "")).length;
console.log(`${done} slices in ${secs.toFixed(1)} s; leases ${JSON.stringify(st)}; results stored ${v.results}`);
const ok = done >= want && !st.active && !st.expired && lostMsgs === 0;
console.log(ok ? "WORKER E2E OK" : "WORKER E2E FAIL");
process.exit(ok ? 0 : 1);
