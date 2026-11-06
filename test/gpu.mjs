// WebGPU for the Node.js tests and the benchmark: Dawn, the implementation in
// Chrome (npm package webgpu, installed by tools/setup_node.sh). Import this
// module first: it installs navigator.gpu and the WebGPU globals
// (GPUBufferUsage, ...).
//   GB_WEBGPU_BACKEND  Dawn backend (default vulkan; never the OpenGL adapters
//                      of other GPUs)
//   GB_WEBGPU_ADAPTER  adapter name (Dawn's adapter= flag), e.g. llvmpipe
// Dawn's timestamp queries are in nanoseconds.
import { create, globals } from "webgpu";

// no implicit Vulkan layers (overlays, frame generation, capture tools a user
// may have installed) in the tests and the benchmark
process.env.VK_LOADER_LAYERS_DISABLE ??= "~implicit~";

Object.assign(globalThis, globals);
const flags = [`backend=${process.env.GB_WEBGPU_BACKEND ?? "vulkan"}`];
if (process.env.GB_WEBGPU_ADAPTER) flags.push(`adapter=${process.env.GB_WEBGPU_ADAPTER}`);
globalThis.navigator.gpu = create(flags);
