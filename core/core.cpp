// goldbach-web core: the host-side mathematics of the dense verifier, shared
// with the native build (shared/: copies of goldbach-l0-tidy's src/primes.cpp,
// include/{config,primes,gstats}.hpp) and compiled to
// WebAssembly with Emscripten (web/core/build.sh). Everything here is exact
// 64-bit integer arithmetic; the GPU side (web/kernels/*.wgsl) only ever
// receives values this file has computed or proven in range.
//
// The same file builds natively (core/build.sh native) as the test
// oracle for the WebGPU port.

#include <algorithm>
#include <cstdint>
#include <cmath>
#include <cstring>
#include <new>
#include <stdexcept>
#include <string>
#include <vector>

#include "config.hpp"
#include "primes.hpp"
#include "gstats.hpp"

#ifdef __EMSCRIPTEN__
#include <emscripten/emscripten.h>
#define GB_EXPORT extern "C" EMSCRIPTEN_KEEPALIVE
#else
#define GB_EXPORT extern "C"
#endif

namespace {

std::vector<uint32_t> g_primes;        // odd base primes <= base_prime_bound(limit) (normal mode)
// Lite mode (phones, low-memory devices): the same primes as 1-byte half-gaps
// (p_i - p_{i-1}) / 2 (< 256 below 2^32: the largest gap is 336) with the
// prime itself at every multiple of LITE_GROUP (g_base), 109 MB instead of 437;
// the first LITE_SMALL primes are also kept as u32 (tile primes, masks).
constexpr uint32_t LITE_GROUP = 512;
constexpr uint32_t LITE_SMALL = 1u << 20;
std::vector<uint8_t> g_gap;
std::vector<uint32_t> g_base;
std::vector<uint32_t> g_small;
uint32_t g_count = 0;
bool g_lite = false;

const uint32_t* small_primes() { return g_lite ? g_small.data() : g_primes.data(); }
uint32_t small_count() { return g_lite ? uint32_t(g_small.size()) : uint32_t(g_primes.size()); }
/// Prime of index i (either mode).
uint32_t prime_at(uint32_t i)
{
    if (!g_lite) return g_primes[i];
    if (i < g_small.size()) return g_small[i];
    uint32_t p = g_base[i / LITE_GROUP];
    for (uint32_t k = (i / LITE_GROUP) * LITE_GROUP + 1; k <= i; ++k) p += 2u * g_gap[k];
    return p;
}
uint64_t g_limit = 0;
std::string g_error;

// Mod-30 class map geometry (identical to src/verifier.cpp class_geometry,
// K = 8): class c holds q = Q[c] + 30 t, t < n[c].
constexpr uint32_t RES30[8] = {1, 7, 11, 13, 17, 19, 23, 29};
struct Geo30 {
    uint32_t phi[8];
    uint64_t Q[8];
    uint64_t n[8];
};
Geo30 geo30(uint64_t q_first, uint64_t q_odds)
{
    Geo30 g{};
    const uint32_t r0 = uint32_t(q_first % 30);
    for (uint32_t c = 0; c < 8; ++c) {
        g.phi[c] = ((RES30[c] + 30 - r0) % 30) / 2;
        g.Q[c] = q_first + 2 * uint64_t(g.phi[c]);
        g.n[c] = q_odds > g.phi[c] ? (q_odds - g.phi[c] + 14) / 15 : 0;
    }
    return g;
}

} // namespace

/// Generates the odd base primes for runs up to @p limit (every prime <=
/// floor(sqrt(limit)) + 16). Returns the count, or 0 on error (gb_error()).
GB_EXPORT uint32_t gb_init(uint64_t limit)
{
    try {
        gb::require_supported_limit(limit);
        g_primes = gb::segmented_odd_primes_up_to(gb::base_prime_bound(limit));
        g_lite = false;
        g_limit = limit;
        return uint32_t(g_primes.size());
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

/// Lite-mode init: the base primes as half-gaps + group bases (see g_gap).
/// Returns the count, or 0 on error.
GB_EXPORT uint32_t gb_init_lite(uint64_t limit)
{
    try {
        gb::require_supported_limit(limit);
        const uint64_t n = gb::base_prime_bound(limit);
        g_primes.clear(); g_primes.shrink_to_fit();
        g_gap.clear(); g_base.clear(); g_small.clear();
        g_gap.reserve(size_t(1.06 * n / std::log(double(n))) + 1024);
        const uint32_t sqrt_n = uint32_t(std::sqrt(double(n)) + 1);
        const std::vector<uint32_t> base = gb::odd_primes_up_to(std::max(sqrt_n, 3u));
        constexpr uint64_t BLOCK_ODDS = 1u << 20;
        std::vector<uint8_t> seg(BLOCK_ODDS);
        uint32_t prev = 0, cnt = 0;
        for (uint64_t lo = 3; lo <= n; lo += 2 * BLOCK_ODDS) {
            const uint64_t hi = std::min(n, lo + 2 * BLOCK_ODDS - 2);
            const uint64_t odds = (hi - lo) / 2 + 1;
            std::fill(seg.begin(), seg.begin() + odds, 0);
            for (const uint32_t p : base) {
                const uint64_t pp = uint64_t(p) * p;
                if (pp > hi) break;
                uint64_t m = (pp >= lo) ? pp : ((lo + p - 1) / p) * p;
                if ((m & 1) == 0) m += p;
                for (uint64_t q = m; q <= hi; q += 2ull * p) seg[(q - lo) >> 1] = 1;
            }
            for (uint64_t i = 0; i < odds; ++i) {
                if (seg[i]) continue;
                const uint32_t p = uint32_t(lo + 2 * i);
                const uint32_t hg = cnt ? (p - prev) / 2 : 0;
                if (hg > 255) throw std::runtime_error("lite primes: half-gap above 255");
                if (cnt % LITE_GROUP == 0) g_base.push_back(p);
                g_gap.push_back(uint8_t(cnt % LITE_GROUP == 0 ? 0 : hg));
                if (cnt < LITE_SMALL) g_small.push_back(p);
                prev = p;
                ++cnt;
            }
        }
        g_count = cnt;
        g_lite = true;
        g_limit = limit;
        return cnt;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

GB_EXPORT const char* gb_error() { return g_error.c_str(); }

// ---- prime cache ---------------------------------------------------------
// Browsers keep the base primes (OPFS) and the idle worker in a file, so they
// are generated once per device instead of once per visit. Blob: an 8-word
// header {magic, version, limit lo, limit hi, count, nbase, check lo, check hi},
// then the group bases (nbase u32) and the half-gaps (count bytes) of the lite
// form. The check (a 64-bit hash of bases and gaps, from the generator) is
// recomputed before a cached blob is used: a truncated or damaged file is
// rejected and the primes are generated again.
namespace {
constexpr uint32_t CACHE_MAGIC = 0x47425043u;     // "GBPC"
constexpr uint32_t CACHE_VERSION = 1;

uint64_t cache_check(const std::vector<uint32_t>& bases, const std::vector<uint8_t>& gaps)
{
    uint64_t h = 0x9E3779B97F4A7C15ull ^ (uint64_t(bases.size()) << 32) ^ gaps.size();
    auto mix = [&h](uint64_t w) { h ^= w; h *= 0xFF51AFD7ED558CCDull; h ^= h >> 33; };
    for (const uint32_t b : bases) mix(b);
    size_t i = 0;
    for (; i + 8 <= gaps.size(); i += 8) { uint64_t w; std::memcpy(&w, gaps.data() + i, 8); mix(w); }
    uint64_t t = 0;
    std::memcpy(&t, gaps.data() + i, gaps.size() - i);
    mix(t ^ (uint64_t(gaps.size() - i) << 56));
    return h;
}

/// Lite form (g_gap, g_base) of the full-mode primes.
void gaps_from_primes()
{
    g_gap.assign(g_primes.size(), 0);
    g_base.clear();
    for (size_t i = 0; i < g_primes.size(); ++i) {
        if (i % LITE_GROUP == 0) g_base.push_back(g_primes[i]);
        else g_gap[i] = uint8_t((g_primes[i] - g_primes[i - 1]) / 2);
    }
}
} // namespace

/// Header of the cache blob of the current primes (either mode; full mode
/// builds the lite form, freed by gb_cache_done). out: 8 u32.
GB_EXPORT void gb_cache_header(uint32_t* out)
{
    if (!g_lite) gaps_from_primes();
    const uint64_t c = cache_check(g_base, g_gap);
    const uint32_t h[8] = {CACHE_MAGIC, CACHE_VERSION, uint32_t(g_limit), uint32_t(g_limit >> 32),
                           uint32_t(g_gap.size()), uint32_t(g_base.size()), uint32_t(c), uint32_t(c >> 32)};
    std::memcpy(out, h, sizeof h);
}

/// Frees the lite form after gb_cache_header in full mode.
GB_EXPORT void gb_cache_done()
{
    if (!g_lite) { g_gap.clear(); g_gap.shrink_to_fit(); g_base.clear(); g_base.shrink_to_fit(); }
}

/// Validates a cached header for @p limit and sizes the tables for it: the
/// caller then fills gb_bases() (nbase u32) and gb_gaps() (count bytes) and
/// calls gb_init_cached. Returns 1, or 0 (wrong format or limit).
GB_EXPORT uint32_t gb_cache_open(uint64_t limit, const uint32_t* h)
{
    if (h[0] != CACHE_MAGIC || h[1] != CACHE_VERSION || (uint64_t(h[3]) << 32 | h[2]) != limit
        || h[4] == 0 || h[5] != (h[4] + LITE_GROUP - 1) / LITE_GROUP) return 0;
    g_primes.clear(); g_primes.shrink_to_fit();
    g_small.clear(); g_small.shrink_to_fit();
    g_gap.assign(h[4], 0);
    g_base.assign(h[5], 0);
    return 1;
}

/// Builds the tables from the filled cache (lite: kept; full: expanded to u32).
/// Returns the prime count, or 0 if the check does not match (gb_error()).
GB_EXPORT uint32_t gb_init_cached(uint64_t limit, const uint32_t* h, uint32_t lite)
{
    try {
        if (cache_check(g_base, g_gap) != (uint64_t(h[7]) << 32 | h[6]))
            throw std::runtime_error("prime cache: check mismatch");
        const uint32_t n = uint32_t(g_gap.size());
        if (lite) {
            g_small.clear();
            g_small.reserve(std::min(n, LITE_SMALL));
            for (uint32_t i = 0, p = 0; i < std::min(n, LITE_SMALL); ++i) {
                p = (i % LITE_GROUP == 0) ? g_base[i / LITE_GROUP] : p + 2u * g_gap[i];
                g_small.push_back(p);
            }
            g_count = n;
        } else {
            g_primes.resize(n);
            for (uint32_t i = 0, p = 0; i < n; ++i) {
                p = (i % LITE_GROUP == 0) ? g_base[i / LITE_GROUP] : p + 2u * g_gap[i];
                g_primes[i] = p;
            }
            g_gap.clear(); g_gap.shrink_to_fit(); g_base.clear(); g_base.shrink_to_fit();
        }
        g_lite = lite != 0;
        g_limit = limit;
        return n;
    } catch (const std::exception& e) {
        g_error = e.what();
        g_gap.clear(); g_base.clear(); g_primes.clear(); g_small.clear();
        return 0;
    }
}

GB_EXPORT const uint32_t* gb_primes() { return g_lite ? g_small.data() : g_primes.data(); }
GB_EXPORT uint32_t gb_prime_count() { return g_lite ? g_count : uint32_t(g_primes.size()); }
GB_EXPORT uint32_t gb_small_count() { return small_count(); }
GB_EXPORT const uint8_t* gb_gaps() { return g_gap.data(); }
GB_EXPORT const uint32_t* gb_bases() { return g_base.data(); }
GB_EXPORT uint32_t gb_base_count() { return uint32_t(g_base.size()); }
GB_EXPORT uint32_t gb_prime_at(uint32_t i) { return i < gb_prime_count() ? prime_at(i) : 0; }

/// Deterministic Miller-Rabin, exact for all 64-bit n.
GB_EXPORT uint32_t gb_is_prime(uint64_t n) { return gb::is_prime_u64(n) ? 1u : 0u; }

/// Class-map geometry of a segment, written to @p out:
/// out[c] = phi_c, out[8 + c] = n_c (valid bits of class c, < 2^32).
GB_EXPORT void gb_geo30(uint64_t q_first, uint64_t q_odds, uint32_t* out)
{
    const Geo30 g = geo30(q_first, q_odds);
    for (uint32_t c = 0; c < 8; ++c) {
        out[c] = g.phi[c];
        out[8 + c] = uint32_t(g.n[c]);
    }
}

/// Exact CPU reference of a segment's mod-30 class map: class c occupies
/// words [c * cw32, (c + 1) * cw32) of @p out; bit t of class c is 1 iff
/// q = Q_c + 30 t is prime (t < n_c), 0 beyond n_c. Built by sieving with
/// every base prime from max(p^2, q_first), independently of the GPU code.
/// @pre   gb_init(limit) with q_first + 2 (q_odds - 1) <= limit; q_first odd;
///        cw32 >= ceil(max n_c / 32).
/// @return 1 on success, 0 on a violated precondition (gb_error()).
GB_EXPORT uint32_t gb_ref_class_map_range(uint64_t q_first, uint64_t q_odds, uint32_t* out,
                                          uint32_t cw32, uint32_t j_lo, uint32_t j_hi);
GB_EXPORT uint32_t gb_ref_class_map(uint64_t q_first, uint64_t q_odds, uint32_t* out, uint32_t cw32)
{
    return gb_ref_class_map_range(q_first, q_odds, out, cw32, 0, 0xFFFFFFFFu);
}

/// gb_ref_class_map sieving only with the base primes of index [j_lo, j_hi)
/// (stage-by-stage validation of the GPU sieve).
GB_EXPORT uint32_t gb_ref_class_map_range(uint64_t q_first, uint64_t q_odds, uint32_t* out,
                                          uint32_t cw32, uint32_t j_lo, uint32_t j_hi)
{
    try {
        if ((q_first & 1) == 0 || q_odds == 0) throw std::runtime_error("ref map: bad geometry");
        const uint64_t q_last = q_first + 2 * (q_odds - 1);
        if (q_last > g_limit) throw std::runtime_error("ref map: segment beyond gb_init limit");
        const Geo30 g = geo30(q_first, q_odds);
        for (uint32_t c = 0; c < 8; ++c)
            if ((g.n[c] + 31) / 32 > cw32) throw std::runtime_error("ref map: cw32 too small");
        // all ones on valid bits
        for (uint32_t c = 0; c < 8; ++c) {
            uint32_t* a = out + size_t(c) * cw32;
            std::memset(a, 0, size_t(cw32) * 4);
            const uint64_t n = g.n[c];
            for (uint64_t w = 0; w < n / 32; ++w) a[w] = 0xFFFFFFFFu;
            if (n % 32) a[n / 32] = (1u << (n % 32)) - 1u;
        }
        for (uint32_t j = j_lo; j < std::min<size_t>(j_hi, g_primes.size()); ++j) {
            const uint64_t p = g_primes[j];
            if (p == 3 || p == 5) continue;               // no multiples are stored
            if (p > q_last / p) break;                    // p^2 > q_last: no composite marks
            const uint64_t first = std::max(p * p, q_first);
            uint64_t pinv = 1;                            // p^-1 mod 30 = p^7 mod 30
            for (int e = 0; e < 7; ++e) pinv = pinv * (p % 30) % 30;
            const uint64_t m0 = (first + p - 1) / p;      // smallest multiplier with m p >= first
            for (uint32_t c = 0; c < 8; ++c) {
                const uint64_t mt = (RES30[c] * pinv) % 30; // m p = RES30[c] (mod 30)
                const uint64_t m = m0 + (mt + 30 - m0 % 30) % 30;
                const uint64_t q = m * p;                 // <= limit + 30 p: no wrap (limit <= 2^64 - 2^40)
                if (q > q_last) continue;
                uint32_t* a = out + size_t(c) * cw32;
                for (uint64_t t = (q - g.Q[c]) / 30; t < g.n[c]; t += p)
                    a[t >> 5] &= ~(1u << (t & 31));
            }
        }
        return 1;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

/// Per-class first marks of the tile primes [0, split) for a segment: out[c *
/// split + j] = class index t of the smallest q = Q_c + 30 t >= max(p^2,
/// q_first) with p | q, or 0xFFFFFFFF if there is none in the segment (or p
/// is 3 or 5). Class indices of a segment are < 2^32 (q_odds < 2^32).
/// @return 1, or 0 on a violated precondition.
GB_EXPORT uint32_t gb_tile_first_bits(uint64_t q_first, uint64_t q_odds, uint32_t split, uint32_t* out)
{
    try {
        if (split > small_count()) throw std::runtime_error("first bits: split beyond the small primes");
        const uint32_t* sp = small_primes();
        if ((q_first & 1) == 0 || q_odds == 0 || q_odds >= (1ull << 32))
            throw std::runtime_error("first bits: bad geometry");
        const uint64_t q_last = q_first + 2 * (q_odds - 1);
        const Geo30 g = geo30(q_first, q_odds);
        std::fill(out, out + size_t(8) * split, 0xFFFFFFFFu);
        for (uint32_t j = 0; j < split; ++j) {
            const uint64_t p = sp[j];
            if (p == 3 || p == 5 || p > q_last / p) continue;
            const uint64_t first = std::max(p * p, q_first);
            uint64_t pinv = 1;
            for (int e = 0; e < 7; ++e) pinv = pinv * (p % 30) % 30;
            const uint64_t m0 = (first + p - 1) / p;
            for (uint32_t c = 0; c < 8; ++c) {
                const uint64_t mt = (RES30[c] * pinv) % 30;
                const uint64_t q = (m0 + (mt + 30 - m0 % 30) % 30) * p;
                if (q > q_last) continue;
                const uint64_t t = (q - g.Q[c]) / 30;
                if (t < g.n[c]) out[size_t(c) * split + j] = uint32_t(t);
            }
        }
        return 1;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

/// Number of base primes below @p bound (a split point by value).
GB_EXPORT uint32_t gb_prime_index(uint32_t bound)
{
    if (!g_lite) return uint32_t(std::lower_bound(g_primes.begin(), g_primes.end(), bound) - g_primes.begin());
    // first group whose base is >= bound, then step back into the previous group
    const size_t g = std::lower_bound(g_base.begin(), g_base.end(), bound) - g_base.begin();
    if (g == 0) return 0;
    uint32_t i = uint32_t((g - 1) * LITE_GROUP), p = g_base[g - 1];
    while (p < bound && i + 1 < g_count && (i + 1) % LITE_GROUP != 0) { ++i; p += 2u * g_gap[i]; }
    if (p >= bound) return i;
    return std::min<uint32_t>(i + 1, g_count);
}

/// Canonical small-prime mask groups of the mod-30 class map (primes 3 .. 61;
/// 3 and 5 all ones), as in the native class mode (primes.cpp): u64 table words
/// written as little-endian u32 pairs to @p words, (offset, period) per group
/// to @p desc. @return the group count, 0 if @p cap_words is too small.
GB_EXPORT uint32_t gb_mask_groups(uint32_t max_period, uint32_t* words, uint32_t cap_words, uint32_t* desc)
{
    const uint32_t mask_count = gb_prime_index(64);
    const gb::SmallPrimeGroups gm = gb::small_prime_group_masks_canonical(small_primes(), mask_count, 15u, max_period);
    if (gm.words.size() * 2 > cap_words) { g_error = "mask groups: buffer too small"; return 0; }
    for (size_t i = 0; i < gm.words.size(); ++i) {
        words[2 * i] = uint32_t(gm.words[i]);
        words[2 * i + 1] = uint32_t(gm.words[i] >> 32);
    }
    std::copy(gm.desc.begin(), gm.desc.end(), desc);
    return gm.count();
}

/// Per-group word offsets of the class array whose bit t is q = base + 30 t
/// against the canonical tables. @return the group count written to @p out.
GB_EXPORT uint32_t gb_group_offsets(uint64_t base, uint32_t max_period, uint32_t* out)
{
    const uint32_t mask_count = gb_prime_index(64);
    const std::vector<uint32_t> ys = gb::small_prime_group_offsets(small_primes(), mask_count, base, 30, max_period);
    std::copy(ys.begin(), ys.end(), out);
    return uint32_t(ys.size());
}

/// Initial packed-prime states for a run starting at @p q_first: out[i - pa] =
/// map bit of the first odd multiple of primes[i] at or after q_first (< p).
/// @pre   primes[pb - 1]^2 < q_first (marks never start at p^2 here).
GB_EXPORT uint32_t gb_packed_state(uint64_t q_first, uint32_t pa, uint32_t pb, uint32_t* out)
{
    try {
        if (pb > g_primes.size() || pa > pb || (q_first & 1) == 0)
            throw std::runtime_error("packed state: bad arguments");
        if (pb > pa && uint64_t(g_primes[pb - 1]) * g_primes[pb - 1] >= q_first)
            throw std::runtime_error("packed state: p^2 >= q_first (range too low for the web path)");
        for (uint32_t i = pa; i < pb; ++i) {
            const uint64_t p = g_primes[i];
            const uint64_t r = q_first % p;
            uint64_t t = r ? p - r : 0;
            if (t & 1) t += p;                       // q_first odd: odd multiple
            out[i - pa] = uint32_t(t >> 1);
        }
        return 1;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

constexpr uint32_t HL_J_FAR = 0xF0000000u;   // MUST match engine.js HL_J_FAR

/// Initial hit-list states for a batch starting at @p q_first: for primes
/// [a, b), X = (max(p^2, first odd multiple >= q_first) - q_first) / 2 as
/// (j = X / delta, off = X mod delta) pairs.
GB_EXPORT uint32_t gb_harvest_state(uint64_t q_first, uint32_t delta, uint32_t a, uint32_t b, uint32_t* out)
{
    try {
        if (b > g_primes.size() || a > b || delta == 0 || (q_first & 1) == 0)
            throw std::runtime_error("harvest state: bad arguments");
        for (uint32_t i = a; i < b; ++i) {
            const uint64_t p = g_primes[i];
            uint64_t first = p * p;
            if (first < q_first) {
                const uint64_t r = q_first % p;
                first = q_first + (r ? p - r : 0);
                if ((first & 1) == 0) first += p;
            }
            const uint64_t X = (first - q_first) >> 1;
            // A first multiple at segment >= HL_J_FAR lies beyond any run the
            // engine accepts (it requires fewer than HL_J_FAR / 2 segments), so
            // the prime has no multiple in the run: park it at HL_J_FAR (the
            // harvest decrements it by at most the run's segment count).
            const uint64_t j = X / delta;
            out[2 * size_t(i - a)] = j >= HL_J_FAR ? HL_J_FAR : uint32_t(j);
            out[2 * size_t(i - a) + 1] = j >= HL_J_FAR ? 0u : uint32_t(X % delta);
        }
        return 1;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

/// Wheel / coprimality tables of the large kernels (66290 u32):
/// [0, 30030) WHL_INV (a^-1 mod 30030 for a coprime to 30030, else 0),
/// [30030, 60060) WHL_IDX (index of a among the units, else 0xFFFF),
/// [60060, 65820) WHL_STEP (half the gap to the next odd unit),
/// [65820, 66290) CP15015 (bit x set iff gcd(x, 15015) = 1).
GB_EXPORT void gb_wheel_tabs(uint32_t* out)
{
    constexpr uint32_t W = 30030;
    auto gcd = [](uint32_t x, uint32_t y) { while (y) { const uint32_t t = x % y; x = y; y = t; } return x; };
    std::fill(out, out + 66290, 0u);
    std::vector<uint32_t> units;
    for (uint32_t a = 1; a < W; ++a)
        if (gcd(a, W) == 1) units.push_back(a);
    for (uint32_t a = 0; a < W; ++a) out[W + a] = 0xFFFFu;
    for (uint32_t k = 0; k < units.size(); ++k) {
        const uint32_t a = units[k];
        out[W + a] = k;
        for (uint32_t b : units)
            if (uint64_t(a) * b % W == 1) { out[a] = b; break; }
        const uint32_t next = (k + 1 < units.size()) ? units[k + 1] : units[0] + W;
        out[2 * W + k] = (next - a) / 2;
    }
    for (uint32_t x = 0; x < 15015; ++x)
        if (gcd(x, 15015) == 1) out[65820 + x / 32] |= 1u << (x % 32);
}

/// Candidate list of even class cr = (n mod 30) / 2 for the checker: every odd
/// prime p <= MAX_GB_PRIME with (2 cr - p) mod 30 coprime to 30, ascending,
/// as p | k << 16 with k the class array of q = n - p. (The other candidates
/// give q divisible by 3 or 5: never prime this far up.)
/// @return the list length.
GB_EXPORT uint32_t gb_cand_list(uint32_t cr, uint32_t* out)
{
    static const std::vector<uint32_t> cand = gb::odd_primes_up_to(gb::MAX_GB_PRIME);
    uint32_t n = 0;
    for (const uint32_t p : cand) {
        const uint32_t res = (2 * cr + 30 * 400 - p) % 30;
        for (uint32_t k = 0; k < 8; ++k)
            if (RES30[k] == res) { out[n++] = p | (k << 16); break; }
    }
    return n;
}

/// count30 limits of a segment (gstats.hpp count30_limits): 32 u32 values.
GB_EXPORT uint32_t gb_count30_limits(uint64_t seg_start, uint64_t seg_end, uint64_t q_first, uint64_t q_odds, uint32_t* out)
{
    try {
        const Geo30 g = geo30(q_first, q_odds);
        const std::vector<uint32_t> lim = gb::count30_limits(seg_start, seg_end, g.Q);
        std::copy(lim.begin(), lim.end(), out);
        return 1;
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}

/// Exact run-boundary corrections of the counts (GoldbachStats::finish_run):
/// primes are counted over [start - 3, limit - 3], wanted [start + 1, limit - 1];
/// twins counted for q in [start - 5, limit - 5], wanted q in [start + 1, limit - 3].
/// @param p30  8 prime counts by residue class (int64, corrected in place)
/// @param t30  3 twin counts (11/13, 17/19, 29/31), corrected in place
GB_EXPORT void gb_finish_counts(uint64_t start, uint64_t limit, int64_t* p30, int64_t* t30)
{
    auto res_idx = [](uint64_t q) { const uint32_t r = uint32_t(q % 30); for (uint32_t c = 0; c < 8; ++c) if (RES30[c] == r) return int(c); return -1; };
    auto twin_idx = [](uint64_t q) { const uint32_t r = uint32_t(q % 30); return r == 11 ? 0 : r == 17 ? 1 : r == 29 ? 2 : -1; };
    for (uint64_t q : {start - 3, start - 1})
        if (gb::is_prime_u64(q)) { const int c = res_idx(q); if (c >= 0) p30[c] -= 1; }
    if (gb::is_prime_u64(limit - 1)) { const int c = res_idx(limit - 1); if (c >= 0) p30[c] += 1; }
    for (uint64_t q : {start - 5, start - 3, start - 1})
        if (gb::is_prime_u64(q) && gb::is_prime_u64(q + 2)) { const int t = twin_idx(q); if (t >= 0) t30[t] -= 1; }
    if (gb::is_prime_u64(limit - 3) && gb::is_prime_u64(limit - 1)) { const int t = twin_idx(limit - 3); if (t >= 0) t30[t] += 1; }
}

// ---- prime gaps (the live paper's section 2.2; goldbach_l0 2.4 gaps30) --------
// A segment's counted primes (count30_limits: q in [seg_start - 3, seg_end - 3])
// walked in ascending order; its gaps are those whose lower prime is counted
// too (the gap into the segment's first prime is the caller's: it stitches
// segments). Positions are q - q_first.
namespace {
constexpr uint32_t GAP_TAIL = 700, GAP_HBINS = 1024;
template <class Word>
uint32_t gap_walk(Word word, uint32_t cw32, uint64_t seg_start, uint64_t seg_end, uint64_t q_first,
                  uint64_t q_odds, uint32_t min_upper, uint32_t tcap, uint32_t* out)
{
    const Geo30 g = geo30(q_first, q_odds);
    const std::vector<uint32_t> lim = gb::count30_limits(seg_start, seg_end, g.Q);
    uint32_t ord[8] = {0, 1, 2, 3, 4, 5, 6, 7};
    std::sort(ord, ord + 8, [&](uint32_t x, uint32_t y) { return g.phi[x] < g.phi[y]; });
    std::memset(out, 0, (5 + GAP_HBINS) * 4);
    out[0] = ~0u;
    uint64_t prev = 0;
    bool have = false;
    for (uint32_t w = 0; w < cw32; ++w) {
        uint32_t m[8], any = 0;
        for (uint32_t k = 0; k < 8; ++k) {
            const uint32_t c = ord[k], lo = lim[2 * c], hi = lim[2 * c + 1], b = 32 * w;
            uint32_t x = word(c, w);
            if (hi <= b || lo >= b + 32) x = 0;
            else {
                if (lo > b) x &= ~((1u << (lo - b)) - 1u);
                if (hi < b + 32) x &= (1u << (hi - b)) - 1u;
            }
            m[k] = x;
            any |= x;
        }
        while (any) {
            const uint32_t j = uint32_t(__builtin_ctz(any));
            any &= any - 1;
            for (uint32_t k = 0; k < 8; ++k) {
                if (!((m[k] >> j) & 1u)) continue;
                const uint64_t q = 30ull * (32ull * w + j) + 2 * g.phi[ord[k]];
                if (q > 0xFFFFFFFFull) return 0;
                if (!have) out[0] = uint32_t(q);
                if (have && q >= min_upper) {           // the run's gaps only (segment 0)
                    const uint64_t gap = q - prev;
                    if (gap >= 2 * GAP_HBINS) return 0;
                    out[5 + gap / 2] += 1;
                    if (gap > out[2]) { out[2] = uint32_t(gap); out[3] = uint32_t(q); }
                    if (gap >= GAP_TAIL) {
                        if (out[4] >= tcap) return 0;
                        out[5 + GAP_HBINS + 2 * out[4]] = uint32_t(q);
                        out[6 + GAP_HBINS + 2 * out[4]] = uint32_t(gap);
                        ++out[4];
                    }
                }
                out[1] = uint32_t(q);
                prev = q;
                have = true;
            }
        }
    }
    return 1;
}
} // namespace

/// Prime gaps of one segment's class map (the WebGPU layout: class c at words
/// [c cw32, (c + 1) cw32)): the CPU reference of count30.wgsl's gap walk and the CPU
/// verifier's gap walk.
/// @param out  [0] / [1] first / last counted prime position (~0 / 0: none),
///             [2] largest gap, [3] its upper position (the first such gap),
///             [4] gaps >= 700 listed, [5, 5 + 1024) histogram by g / 2, then
///             (upper position, g) pairs for the listed gaps (room for tcap).
/// @return 1, or 0 when a gap >= 2048, a position >= 2^32 or more than tcap
///         gaps >= 700 occur (none does below 2^64 with these segments).
GB_EXPORT uint32_t gb_gap_walk(const uint32_t* map, uint32_t cw32, uint64_t seg_start, uint64_t seg_end,
                               uint64_t q_first, uint64_t q_odds, uint32_t min_upper, uint32_t tcap, uint32_t* out)
{
    try {
        return gap_walk([&](uint32_t c, uint32_t w) { return map[size_t(c) * cw32 + w]; }, cw32, seg_start, seg_end,
                        q_first, q_odds, min_upper, tcap, out);
    } catch (const std::exception& e) {
        g_error = e.what();
        return 0;
    }
}
/// The CPU verifier's form: class c as 64-bit words.
uint32_t gb_gap_walk64(const std::vector<uint64_t>* map, uint32_t cw32, uint64_t seg_start, uint64_t seg_end,
                       uint64_t q_first, uint64_t q_odds, uint32_t min_upper, uint32_t tcap, uint32_t* out)
{
    return gap_walk([&](uint32_t c, uint32_t w) {
        const size_t i = w >> 1;
        return i < map[c].size() ? uint32_t(map[c][i] >> (32 * (w & 1))) : 0u;
    }, cw32, seg_start, seg_end, q_first, q_odds, min_upper, tcap, out);
}

// ---- a run's gaps (every segment walked): the "gaps" object of a result ------
GB_EXPORT uint64_t gb_prev_prime(uint64_t x);
namespace {
struct GapRun {
    uint64_t start = 0, last = 0;                 // last counted prime so far
    std::vector<uint64_t> hist = std::vector<uint64_t>(GAP_HBINS, 0);
    std::vector<std::pair<uint32_t, uint64_t>> tails;   // (g, lower) of every gap >= GAP_TAIL
    uint64_t gmax = 0, gmax_lo = 0;
    std::string json;
    void add(uint64_t g, uint64_t lower)
    {
        hist.at(g / 2) += 1;
        if (g >= GAP_TAIL) tails.push_back({uint32_t(g), lower});
        if (g > gmax || (g == gmax && lower < gmax_lo)) { gmax = g; gmax_lo = lower; }
    }
} g_gaps;
std::string u128s(unsigned __int128 v)
{
    std::string o;
    do { o.insert(o.begin(), char('0' + int(v % 10))); v /= 10; } while (v);
    return o;
}
} // namespace

/// Starts a run: the gap into its first prime comes from the prime before the
/// counting range [start - 3, ...]. Segment 0 is walked with gb_gap_walk's
/// min_upper = start + 1 - q_first (its gaps below start + 1 are not the run's).
GB_EXPORT void gb_gaps_begin(uint64_t start)
{
    g_gaps = GapRun{};
    g_gaps.start = start;
    g_gaps.last = gb_prev_prime(start - 3);
}
/// One segment's gb_gap_walk output (segments in ascending order; segment 0
/// walked with min_upper = start + 1 - q_first).
/// @return 1, or 0 on a malformed walk (gb_error()).
GB_EXPORT uint32_t gb_gaps_segment(uint64_t q_first, const uint32_t* w)
{
  try {
    GapRun& r = g_gaps;
    if (w[0] == ~0u) return 1;                    // no counted prime
    for (uint32_t i = 0; i < GAP_HBINS; ++i) r.hist[i] += w[5 + i];
    for (uint32_t i = 0; i < w[4]; ++i) {
        const uint64_t up = q_first + w[5 + GAP_HBINS + 2 * i], g = w[6 + GAP_HBINS + 2 * i];
        r.tails.push_back({uint32_t(g), up - g});
    }
    if (w[2]) {
        const uint64_t g = w[2], lo = q_first + w[3] - g;
        if (g > r.gmax || (g == r.gmax && lo < r.gmax_lo)) { r.gmax = g; r.gmax_lo = lo; }
    }
    const uint64_t fq = q_first + w[0];
    if (fq >= r.start + 1) r.add(fq - r.last, r.last);   // the gap into the segment's first prime
    r.last = q_first + w[1];
    return 1;
  } catch (const std::exception& e) {
    g_error = std::string("gap accumulation: ") + e.what() + " (start " + std::to_string(g_gaps.start) + ", last " +
              std::to_string(g_gaps.last) + ", segment origin " + std::to_string(q_first) + ", first " + std::to_string(w[0]) +
              ", histogram bins " + std::to_string(g_gaps.hist.size()) + ")";
    return 0;
  }
}
/// Ends the run: the gap into limit - 1 (prime, past the counting range).
/// @return the "gaps" object, compact JSON: {"n","d":[sum g^1..g^4],"hist":
///         [[g,count]...],"max":[g,"lower"],"tail":[[g,count,"first lower"]...]}
///         (numbers above 2^53 as strings).
GB_EXPORT const char* gb_gaps_finish(uint64_t limit)
{
  try {
    GapRun& r = g_gaps;
    if (limit >= r.start + 2 && gb::is_prime_u64(limit - 1)) r.add(limit - 1 - r.last, r.last);
    unsigned __int128 d[4] = {};
    uint64_t n = 0;
    std::string h;
    for (uint32_t i = 0; i < GAP_HBINS; ++i) {
        if (!r.hist[i]) continue;
        const unsigned __int128 g = 2 * i, c = r.hist[i];
        n += r.hist[i];
        d[0] += c * g; d[1] += c * g * g; d[2] += c * g * g * g; d[3] += c * g * g * g * g;
        h += (h.empty() ? "" : ",") + ("[" + std::to_string(2 * i) + "," + std::to_string(r.hist[i]) + "]");
    }
    std::sort(r.tails.begin(), r.tails.end());
    std::string t;
    for (size_t i = 0; i < r.tails.size();) {
        size_t j = i;
        while (j < r.tails.size() && r.tails[j].first == r.tails[i].first) ++j;
        t += (t.empty() ? "" : ",") + ("[" + std::to_string(r.tails[i].first) + "," + std::to_string(j - i) + ",\"" +
                                       std::to_string(r.tails[i].second) + "\"]");
        i = j;
    }
    r.json = "{\"n\":\"" + std::to_string(n) + "\",\"d\":[\"" + u128s(d[0]) + "\",\"" + u128s(d[1]) + "\",\"" +
             u128s(d[2]) + "\",\"" + u128s(d[3]) + "\"],\"hist\":[" + h + "],\"max\":[" + std::to_string(r.gmax) +
             ",\"" + std::to_string(r.gmax_lo) + "\"],\"tail\":[" + t + "]}";
    return r.json.c_str();
  } catch (const std::exception& e) {
    g_error = std::string("gap totals: ") + e.what();
    return "";                                      // not JSON: the caller fails closed
  }
}
/// Largest prime < x (x > 3).
GB_EXPORT uint64_t gb_prev_prime(uint64_t x)
{
    uint64_t q = (x - 1) | 1;
    if (q >= x) q -= 2;
    while (!gb::is_prime_u64(q)) q -= 2;
    return q;
}

/// p(n): the smallest odd prime p with n - p prime (0 = counterexample).
GB_EXPORT uint64_t gb_pn_exact(uint64_t n) { return gb::pn_exact(n); }

GB_EXPORT void* gb_malloc(uint32_t bytes) { return ::operator new(bytes, std::nothrow); }
GB_EXPORT void gb_free(void* p) { ::operator delete(p); }
