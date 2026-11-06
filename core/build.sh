#!/usr/bin/env bash
# Builds the goldbach-web core.
#   bash core/build.sh          -> client/core.js + core.wasm (Emscripten)
#   bash core/build.sh native   -> build/core_native.so (test oracle)
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=(core/core.cpp shared/src/primes.cpp)
INC=(-Ishared/include)
if [ "${1:-}" = native ]; then
    mkdir -p build
    g++ -std=c++20 -O3 -fPIC -shared "${INC[@]}" "${SRC[@]}" -o build/core_native.so
    echo "built build/core_native.so"
    exit 0
fi
if ! command -v em++ >/dev/null; then
    # shellcheck disable=SC1091
    source "$HOME/emsdk/emsdk_env.sh" >/dev/null 2>&1
fi
em++ -std=c++20 -O3 -fexceptions "${INC[@]}" "${SRC[@]}" -o client/core.js \
    -sMODULARIZE -sEXPORT_ES6 -sENVIRONMENT=web,worker \
    -sALLOW_MEMORY_GROWTH -sMAXIMUM_MEMORY=4GB \
    -sEXPORTED_RUNTIME_METHODS=ccall,cwrap,HEAPU32,HEAPU8,UTF8ToString \
    -sEXPORTED_FUNCTIONS=_gb_init,_gb_init_lite,_gb_small_count,_gb_gaps,_gb_bases,_gb_base_count,_gb_prime_at,_gb_error,_gb_primes,_gb_prime_count,_gb_is_prime,_gb_geo30,_gb_ref_class_map,_gb_ref_class_map_range,_gb_tile_first_bits,_gb_prime_index,_gb_mask_groups,_gb_group_offsets,_gb_packed_state,_gb_harvest_state,_gb_wheel_tabs,_gb_cand_list,_gb_count30_limits,_gb_finish_counts,_gb_pn_exact,_gb_gap_walk,_gb_prev_prime,_gb_gaps_begin,_gb_gaps_segment,_gb_gaps_finish,_gb_malloc,_gb_free,_gb_cache_header,_gb_cache_done,_gb_cache_open,_gb_init_cached
echo "built client/core.js + core.wasm"
