#!/usr/bin/env bash
# WebGPU for the Node.js tests and the benchmark: Dawn (Chrome's WebGPU
# implementation) as the npm package webgpu, pinned and checked against the
# registry's integrity hash, cached once per machine and linked into
# node_modules/ (gitignored). No npm needed.
#   bash tools/setup_node.sh
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION=0.6.2
INTEGRITY=sha512-3L4GfmqJLvYUIEWJngEQY+2HQiUlAFkg8+P8NDDj3Xx5hHHIFO/r8JW+ynfqBj7kHN4mbnwO74uB+lrRtLhp4Q==
CACHE=${GB_NODE_CACHE:-$HOME/.cache/goldbach-web/webgpu-$VERSION}
if [ ! -f "$CACHE/package/dist/linux-x64/dawn.node" ]; then
  mkdir -p "$CACHE"
  tgz="$CACHE/webgpu-$VERSION.tgz"
  curl -fsSL -o "$tgz" "https://registry.npmjs.org/webgpu/-/webgpu-$VERSION.tgz"
  got="sha512-$(openssl dgst -sha512 -binary "$tgz" | base64 -w0)"
  [ "$got" = "$INTEGRITY" ] || { echo "webgpu-$VERSION.tgz: integrity mismatch" >&2; rm -f "$tgz"; exit 1; }
  tar xzf "$tgz" -C "$CACHE"
fi
mkdir -p node_modules
ln -sfn "$CACHE/package" node_modules/webgpu
echo "node_modules/webgpu -> $CACHE/package (webgpu $VERSION, Dawn)"
