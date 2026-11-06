// modq (kernels/large_lite.wgsl, harvest_lite.wgsl): (hi 2^32 + lo) mod p on the
// GPU vs BigInt, random and edge cases, for every p the kernels can see
// (odd primes 36000 .. 2^32). Run on a correct GPU stack (not RADV, see README).
//   node test/modq_test.mjs [n]
import "./gpu.mjs";                       // WebGPU (Dawn) globals first
import { readFile } from "node:fs/promises";
const N = Number(process.argv.slice(2)[0] ?? 1 << 20);
const lite = await readFile(new URL("../kernels/large_lite.wgsl", import.meta.url), "utf8");
const fn = lite.slice(lite.indexOf("// ---- modq"), lite.indexOf("// ---- end modq"));
if (!fn) throw new Error("modq block not found in large_lite.wgsl");
const code = `${fn}
@group(0) @binding(0) var<storage, read> inp: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> outp: array<u32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) g: vec3<u32>) {
  let i = g.x + g.y * 65535u * 64u;
  if (i >= arrayLength(&outp)) { return; }
  let v = inp[i];
  outp[i] = modq(v.x, v.y, v.z);
}`;
const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
const device = await adapter.requestDevice({ requiredLimits: { maxStorageBufferBindingSize: adapter.limits.maxStorageBufferBindingSize, maxBufferSize: adapter.limits.maxBufferSize } });
const mod = device.createShaderModule({ code });
const info = await mod.getCompilationInfo();
if (info.messages.some((m) => m.type === "error")) throw new Error(info.messages.map((m) => `${m.lineNum}: ${m.message}`).join("\n"));
const inp = new Uint32Array(4 * N);
const r32 = () => (Math.random() * 2 ** 32) >>> 0;
for (let i = 0; i < N; i++) {
  const k = i % 8;
  let p = k === 0 ? 36001 + 2 * Math.floor(Math.random() * 15000)            // small packed primes
        : k === 1 ? (0xFFFFFFFF - 2 * Math.floor(Math.random() * 1000)) >>> 0  // near 2^32
        : k === 2 ? (0x80000001 + 2 * Math.floor(Math.random() * 1000)) >>> 0  // near 2^31
        : (r32() | 1) >>> 0;
  if (p < 36001) p = 36001;
  const hi = k === 3 ? 0xFFFFFFFF : k === 4 ? 0 : r32(), lo = k === 5 ? 0xFFFFFFFF : k === 6 ? 0 : r32();
  inp.set([hi, lo, p, 0], 4 * i);
}
const bi = device.createBuffer({ size: inp.byteLength, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_DST });
device.queue.writeBuffer(bi, 0, inp);
const bo = device.createBuffer({ size: 4 * N, usage: GPUBufferUsage.STORAGE | GPUBufferUsage.COPY_SRC });
const rb = device.createBuffer({ size: 4 * N, usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
const pipe = device.createComputePipeline({ layout: "auto", compute: { module: mod, entryPoint: "main" } });
const bg = device.createBindGroup({ layout: pipe.getBindGroupLayout(0), entries: [{ binding: 0, resource: { buffer: bi } }, { binding: 1, resource: { buffer: bo } }] });
const enc = device.createCommandEncoder();
const pass = enc.beginComputePass(); pass.setPipeline(pipe); pass.setBindGroup(0, bg);
const groups = Math.ceil(N / 64); pass.dispatchWorkgroups(Math.min(groups, 65535), Math.ceil(groups / 65535)); pass.end();
enc.copyBufferToBuffer(bo, 0, rb, 0, 4 * N);
device.queue.submit([enc.finish()]);
await rb.mapAsync(GPUMapMode.READ);
const got = new Uint32Array(rb.getMappedRange());
let bad = 0;
for (let i = 0; i < N; i++) {
  const [hi, lo, p] = [inp[4 * i], inp[4 * i + 1], inp[4 * i + 2]];
  const want = Number(((BigInt(hi) << 32n) | BigInt(lo)) % BigInt(p));
  if (got[i] !== want) { if (bad++ < 5) console.log(`MISMATCH hi=${hi} lo=${lo} p=${p}: got ${got[i]} want ${want}`); }
}
console.log(`${adapter.info.description}: ${N} cases, ${bad} wrong`);
console.log(bad ? "MODQ FAILED" : "MODQ OK");
process.exit(bad ? 1 : 0);
