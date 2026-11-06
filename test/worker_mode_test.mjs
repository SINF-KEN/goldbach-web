// Regression tests for the worker's engine selection (liteReasonFor in
// client/worker.js). Engine choice is separate from the page layout: a desktop
// browser, macOS Safari included, gets the full engine whenever the adapter's
// buffer limits allow it, whatever the window width. Lite is chosen only for a
// stated reason (shown in the device label), never as a hidden "phone mode".
//   node test/worker_mode_test.mjs
import { readFile } from "node:fs/promises";

const worker = await readFile(new URL("../client/worker.js", import.meta.url), "utf8");

if (!/^let liteWhy\s*=\s*null;/m.test(worker)) throw new Error("worker must initialize liteWhy before setup");
if (!/liteWhy = liteReasonFor\(L\);/.test(worker)) throw new Error("setupGpu must pass the adapter limits to liteReasonFor");
const start = worker.indexOf("function liteReasonFor(L) {");
const end = worker.indexOf("\nasync function setup()", start);
if (start < 0 || end < 0) throw new Error("cannot find liteReasonFor(L) in worker.js");
const source = worker.slice(start, end + 2);
const body = source.slice(source.indexOf("{") + 1, source.lastIndexOf("}"));
const needFull = Number(worker.match(/^const NEED_FULL = ([\d\s*]+);/m)?.[1].split("*").reduce((a, x) => a * Number(x), 1));
if (!(needFull > 0)) throw new Error("cannot read NEED_FULL from worker.js");
const mib = (bytes) => Math.floor(bytes / 2 ** 20);
const getReason = new Function("navigator", "L", "NEED_FULL", "forceLite", "secondary", "mib", body);

const MAC_SAFARI = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
const MAC_CHROME = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
const big = { maxBufferSize: 2 ** 31 - 1, maxStorageBufferBindingSize: 2 ** 31 - 1 };
const exact = { maxBufferSize: needFull, maxStorageBufferBindingSize: needFull };
let failures = 0;
const test = (name, nav, gpu, expected,
              flags = {}) => {
  const got = getReason(nav, gpu, needFull, !!flags.forceLite, !!flags.secondary, mib);
  console.log(`${got === expected ? "ok  " : "FAIL"} ${name}: got ${got}, expected ${expected}`);
  if (got !== expected) failures++;
};

// desktop: full engine
test("macOS Safari desktop gets the full engine", { userAgent: MAC_SAFARI }, big, null);
test("macOS Safari with touch points still gets the full engine", { userAgent: MAC_SAFARI, maxTouchPoints: 5 }, big, null);
test("macOS Chrome with 8 GB deviceMemory gets the full engine", { userAgent: MAC_CHROME, deviceMemory: 8 }, big, null);
test("limits exactly at the full-engine requirement get the full engine", { userAgent: MAC_SAFARI }, exact, null);
// lite, each with its stated reason
test("iPhone Safari selects lite", { userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1" }, big, "mobile browser");
test("iPad with an iPad UA token selects lite", { userAgent: "Mozilla/5.0 (iPad; CPU OS 26_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Mobile/15E148 Safari/604.1" }, big, "mobile browser");
test("Android browser selects lite", { userAgent: "Mozilla/5.0 (Linux; Android 16; Pixel) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Mobile Safari/537.36" }, big, "mobile browser");
test("a 4 GB Chromium device selects lite", { userAgent: MAC_CHROME, deviceMemory: 4 }, big, "device memory ≤ 4 GB");
test("buffer limits one byte short of the full engine select lite, naming the requirement",
  { userAgent: MAC_SAFARI }, { maxBufferSize: needFull - 1, maxStorageBufferBindingSize: 2 ** 31 - 1 },
  `WebGPU limits (buffer ${mib(needFull - 1)} MiB, storage binding 2047 MiB; full engine needs ${mib(needFull)} MiB)`);
test("a small storage-binding limit alone selects lite",
  { userAgent: MAC_SAFARI }, { maxBufferSize: 2 ** 31 - 1, maxStorageBufferBindingSize: 512 * 2 ** 20 },
  `WebGPU limits (buffer 2047 MiB, storage binding 512 MiB; full engine needs ${mib(needFull)} MiB)`);
test("?lite forces lite on a desktop", { userAgent: MAC_SAFARI }, big, "forced by ?lite", { forceLite: true });
test("the secondary GPU worker always runs lite", { userAgent: MAC_SAFARI }, big, "secondary GPU", { secondary: true });

if (failures) process.exit(1);
console.log("WORKER MODE OK");
