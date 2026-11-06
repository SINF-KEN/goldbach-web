// goldbach-web CPU verifier: one core, no GPU. Verifies the evens of
// [start, limit] exactly like client/engine.js and prints the same result JSON
// (the canonical part of DenseEngine.finish(), byte for byte: its sha256
// equals test/golden.mjs's), so its slices match the browsers' results.
// Used by server/cpu_worker.py on the VPS, at idle priority.
//
//   cpu_worker START LIMIT [--segment S] [--primes FILE]
//     stdout: the result JSON (one line)
//     stderr: "progress DONE TOTAL" after every segment, then "done SECONDS"
//
// Memory: the base primes as 1-byte half-gaps (~110 MB, cached in FILE), the
// mod-30 class map of one segment (8/30 bit per integer), a small table of
// 30^-1 mod p for the medium primes. No per-prime state: every large prime's
// first multiple is recomputed per segment (one u64 division), which is what
// keeps this under ~200 MB at 5e18 (a bucket sieve would need 8 bytes/prime).
//
// Map: class c holds q = Q_c + 30 t (t < n_c), Q_c = q_first + 2 phi_c,
// exactly the WebGPU layout; bit set <=> q prime (all base primes >= 7 sieved,
// classes are coprime to 30). Checker: per class of evens, candidates in
// ascending order on 64-bit words, so the first hit is p(n) for every even.
#include <algorithm>
#include <chrono>
#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <map>
#include <stdexcept>
#include <string>
#include <vector>
#if defined(__AVX2__)
#include <immintrin.h>
#endif

extern "C" {
uint32_t gb_init_lite(uint64_t limit);
const char* gb_error();
uint32_t gb_prime_count();
const uint8_t* gb_gaps();
const uint32_t* gb_bases();
void gb_geo30(uint64_t q_first, uint64_t q_odds, uint32_t* out);
uint32_t gb_cand_list(uint32_t cr, uint32_t* out);
uint32_t gb_count30_limits(uint64_t seg_start, uint64_t seg_end, uint64_t q_first, uint64_t q_odds, uint32_t* out);
void gb_finish_counts(uint64_t start, uint64_t limit, int64_t* p30, int64_t* t30);
uint64_t gb_pn_exact(uint64_t n);
void gb_cache_header(uint32_t* out);
void gb_cache_done();
uint32_t gb_cache_open(uint64_t limit, const uint32_t* h);
uint32_t gb_init_cached(uint64_t limit, const uint32_t* h, uint32_t lite);
uint32_t gb_base_count();
uint32_t gb_prime_index(uint32_t bound);
uint32_t gb_mask_groups(uint32_t max_period, uint32_t* words, uint32_t cap_words, uint32_t* desc);
uint32_t gb_group_offsets(uint64_t base, uint32_t max_period, uint32_t* out);
void gb_gaps_begin(uint64_t start);
uint32_t gb_gaps_segment(uint64_t q_first, const uint32_t* walk);
const char* gb_gaps_finish(uint64_t limit);
}
uint32_t gb_gap_walk64(const std::vector<uint64_t>* map, uint32_t cw32, uint64_t seg_start, uint64_t seg_end,
                       uint64_t q_first, uint64_t q_odds, uint32_t min_upper, uint32_t tcap, uint32_t* out);

namespace {

constexpr uint64_t LIMIT_MAX = 5000000000000000000ull;   // base primes for the campaign
constexpr uint32_t MAX_GB_PRIME = 9781;
constexpr uint32_t TAIL_T = 3000;                        // engine.js TAIL_T
constexpr uint32_t RIDGE_FLOOR = 7000;                   // engine.js RIDGE_FLOOR
constexpr uint32_t LITE_GROUP = 512;
constexpr uint32_t MEDIUM_MAX = 1u << 21;                // primes below: 30^-1 table, blocked marks (= BLOCK_BITS)
constexpr int W = 4;                                     // checker block: W u64 words of evens (= one AVX2 register)

[[noreturn]] void die(const std::string& m) { std::fprintf(stderr, "error: %s\n", m.c_str()); std::exit(1); }

// ---- base primes (cached) ------------------------------------------------------
void load_primes(const char* path)
{
    if (path) {
        if (FILE* f = std::fopen(path, "rb")) {
            uint32_t h[8];
            bool ok = std::fread(h, 4, 8, f) == 8 && gb_cache_open(LIMIT_MAX, h)
                && std::fread(const_cast<uint32_t*>(gb_bases()), 4, h[5], f) == h[5]
                && std::fread(const_cast<uint8_t*>(gb_gaps()), 1, h[4], f) == h[4]
                && gb_init_cached(LIMIT_MAX, h, 1);
            std::fclose(f);
            if (ok) return;
            std::fprintf(stderr, "prime cache %s unusable; generating\n", path);
        }
    }
    if (!gb_init_lite(LIMIT_MAX)) die(gb_error());
    if (path) {
        uint32_t h[8];
        gb_cache_header(h);
        const std::string tmp = std::string(path) + ".tmp";
        FILE* f = std::fopen(tmp.c_str(), "wb");
        bool ok = f && std::fwrite(h, 4, 8, f) == 8 && std::fwrite(gb_bases(), 4, h[5], f) == h[5]
                  && std::fwrite(gb_gaps(), 1, h[4], f) == h[4];
        if (f) ok = (std::fclose(f) == 0) && ok;
        if (ok) std::rename(tmp.c_str(), path); else std::remove(tmp.c_str());
        gb_cache_done();
    }
}

struct Prime { uint32_t p; uint32_t inv30; };            // medium primes (p < MEDIUM_MAX)

uint32_t powmod(uint64_t a, uint64_t e, uint64_t m)
{
    uint64_t r = 1 % m;
    a %= m;
    for (; e; e >>= 1, a = a * a % m) if (e & 1) r = r * a % m;
    return uint32_t(r);
}

// ---- one run ------------------------------------------------------------------
struct Cand { std::vector<uint32_t> p, k; };

struct Run {
    uint64_t start, limit, segment;
    uint32_t np = 0, nmed = 0;                           // primes; medium ones (index < nmed)
    std::vector<Prime> med;
    std::vector<uint32_t> nxt;                           // blocked medium sieve: next mark per (class, prime)
    Cand cand[15];
    // totals
    uint64_t evens = 0;
    int64_t p30[8] = {}, t30[3] = {};
    std::vector<uint64_t> hist[15];
    std::map<uint32_t, uint64_t> first;                  // p >= TAIL_T -> smallest n
    std::vector<std::pair<uint64_t, uint32_t>> ridges, beyond;   // (n, p)
    // segment map
    std::vector<uint64_t> map[8];
    uint32_t ncls[8] = {}, phi[8] = {};
};

const uint32_t RES30[8] = {1, 7, 11, 13, 17, 19, 23, 29};

[[gnu::noinline]] void sieve_segment(Run& R, uint64_t q_first, uint64_t q_odds)   // inlined into main it ran ~15 % slower (GCC 13)
{
    uint32_t g[16];
    gb_geo30(q_first, q_odds, g);
    int phase_class[15];
    std::fill(phase_class, phase_class + 15, -1);
    for (int c = 0; c < 8; ++c) { R.phi[c] = g[c]; R.ncls[c] = g[8 + c]; phase_class[g[c]] = c; }
    // pre-sieve: primes 7 .. 61 from the canonical group tables (the GPU tile
    // sieve's): word x of a class array = AND over groups g of table word
    // (x - Y_g) mod P_g, ~8 loads instead of ~43 single-bit marks per word; zero
    // above n_c (and zero padding words for the checker's reads)
    constexpr uint32_t MAX_PERIOD = 4096;
    static std::vector<uint64_t> gtab;                    // group tables, u64 words
    static uint32_t gdesc[64], ngroups = 0, mask_count = 0;
    if (!ngroups) {
        std::vector<uint32_t> w32(1u << 18);
        ngroups = gb_mask_groups(MAX_PERIOD, w32.data(), uint32_t(w32.size()), gdesc);
        if (!ngroups || ngroups > 16) die("pre-sieve: mask groups");
        const uint32_t nw64 = gdesc[2 * (ngroups - 1)] + gdesc[2 * ngroups - 1];
        gtab.resize(nw64);
        for (uint32_t i = 0; i < nw64; ++i) gtab[i] = uint64_t(w32[2 * i]) | uint64_t(w32[2 * i + 1]) << 32;
        mask_count = gb_prime_index(64);
    }
    for (int c = 0; c < 8; ++c) {
        const uint32_t n = R.ncls[c], nw = (n + 63) / 64;
        R.map[c].assign(nw + 16, 0);
        uint32_t ys[16];
        if (gb_group_offsets(q_first + 2 * uint64_t(R.phi[c]), MAX_PERIOD, ys) != ngroups) die("pre-sieve: group layout");
        uint32_t ph[16];
        for (uint32_t g = 0; g < ngroups; ++g) { const uint32_t P = gdesc[2 * g + 1]; ph[g] = (P - ys[g] % P) % P; }
        uint64_t* a = R.map[c].data();
        for (uint32_t x = 0; x < nw; ++x) {
            uint64_t v = ~0ull;
            for (uint32_t g = 0; g < ngroups; ++g) {
                v &= gtab[gdesc[2 * g] + ph[g]];
                if (++ph[g] == gdesc[2 * g + 1]) ph[g] = 0;
            }
            a[x] = v;
        }
        if (n % 64) a[n / 64] &= (1ull << (n % 64)) - 1;
    }
    // medium primes (7 <= p < MEDIUM_MAX): in class c the multiples of p are t = t0 (mod p);
    // they sweep each class array block by block (L2-sized; their next mark per
    // (class, prime) in R.nxt). Larger primes go through the large-prime buckets
    // below (they used to mark directly up to 2^24: a cache miss per mark, ~20 %
    // of the sieve time)
    const uint8_t* gaps = gb_gaps();
    const uint32_t* bases = gb_bases();
    constexpr uint64_t BLOCK_BITS = 1ull << 21;                       // 256 KB of a class array
    static_assert(MEDIUM_MAX <= BLOCK_BITS && MEDIUM_MAX >= (1u << 21));   // the large primes' remainder in double needs p >= 2^21
    const uint32_t nsmall = R.nmed;
    R.nxt.resize(8 * size_t(nsmall));
    for (uint32_t i = 0; i < R.nmed; ++i) {
        const uint32_t p = R.med[i].p;
        if (i < mask_count) { for (int c = 0; c < 8; ++c) R.nxt[c * size_t(nsmall) + i] = 0xFFFFFFFFu; continue; }   // 3 .. 61: pre-sieved
        const uint64_t r = q_first % p;
        for (int c = 0; c < 8; ++c) {
            const uint64_t qc = (r + 2 * R.phi[c]) % p;               // Q_c mod p
            R.nxt[c * size_t(nsmall) + i] = uint32_t((qc ? p - qc : 0) * uint64_t(R.med[i].inv30) % p);
        }
    }
    for (int c = 0; c < 8; ++c) {
        uint64_t* a = R.map[c].data();
        uint32_t* nx = R.nxt.data() + c * size_t(nsmall);
        const uint64_t n = R.ncls[c];
        for (uint64_t bs = 0; bs < n; bs += BLOCK_BITS) {
            const uint64_t be = std::min(n, bs + BLOCK_BITS);
            for (uint32_t i = 0; i < nsmall; ++i) {
                uint64_t t = nx[i];
                if (t >= be) continue;                                 // also the 0xFFFFFFFF of 3 and 5
                const uint64_t p = R.med[i].p;
                for (; t < be; t += p) a[t >> 6] &= ~(1ull << (t & 63));
                nx[i] = uint32_t(t);                                   // < n + p < 2^32
            }
        }
    }
    // large primes: first odd multiple from q_first mod p, then every odd
    // multiple p m with m coprime to 30 (dense bit b = (q - q_first) / 2)
    static const int8_t inv30tab[30] = {0, 1, 0, 0, 0, 0, 0, 13, 0, 0, 0, 11, 0, 7, 0, 0, 0, 23, 0, 19, 0, 0, 0, 17, 0, 0, 0, 0, 0, 29};
    // wheel over the multipliers coprime to 30 (1 7 11 13 17 19 23 29), so the
    // 7 of 15 odd multiples that are not in the map cost nothing: from an odd
    // m30, wskip[m30] odd steps (b += p each) reach the next one, wheel entry
    // widx[m30]; wstep[k]: odd steps from wheel entry k to k + 1
    static const uint8_t wstep[8] = {3, 2, 1, 2, 1, 2, 3, 1};
    static const uint8_t wskip[30] = {0, 0, 0, 2, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 2, 0, 1, 0, 0};
    static const uint8_t widx[30] = {0, 0, 0, 1, 0, 1, 0, 1, 0, 2, 0, 2, 0, 3, 0, 4, 0, 4, 0, 5, 0, 6, 0, 6, 0, 7, 0, 7, 0, 7};
    const uint32_t qf30 = uint32_t(q_first % 30);
    // Large-prime marks are random writes into the whole map (a cache miss each).
    // They go to buckets by (class, 2^16-bit region of it: 8 KB, in L1) instead,
    // as u16 offsets: a sequential append, and once a bucket is full (16 MB of
    // marks buffered in all) and at the end every bucket is applied, region by
    // region: a cache-hot clear per mark. Clearing bits commutes, so the map is
    // the same. Each round of applying sweeps the whole map, so the marks per
    // round set the cost: u16 marks hold twice as many as u32 (class << 29 | t)
    // in the same memory (~10 % less time at 2e9).
    constexpr uint32_t RBITS = 16, BUCKET_SLOTS = 8u << 20;            // 16 MB of u16 marks
    uint32_t nmax = 0;
    for (int c = 0; c < 8; ++c) nmax = std::max(nmax, R.ncls[c]);
    const uint32_t nreg = (nmax >> RBITS) + 1, nbk = 8 * nreg;
    const uint32_t cap = BUCKET_SLOTS / nbk;                           // marks per bucket
    const bool bucketed = cap >= 256;                                   // segments up to ~8e9
    static std::vector<uint16_t> marks;
    static std::vector<uint32_t> fill;                                  // next free slot per bucket
    if (bucketed) {
        if (marks.size() < size_t(nbk) * cap) marks.resize(size_t(nbk) * cap);
        fill.resize(nbk);
        for (uint32_t k = 0; k < nbk; ++k) fill[k] = k * cap;
    }
    auto flush = [&]() {
        for (uint32_t c = 0; c < 8; ++c)
            for (uint32_t r = 0; r < nreg; ++r) {
                const uint32_t k = c * nreg + r;
                uint64_t* a = R.map[c].data() + (size_t(r) << (RBITS - 6));
                for (uint32_t s = k * cap; s < fill[k]; ++s) a[marks[s] >> 6] &= ~(1ull << (marks[s] & 63));
                fill[k] = k * cap;
            }
    };
    // In batches of primes: first the first marks (a u64 division each, no
    // branches, so the divisions of consecutive primes overlap; the primes that
    // mark this segment are kept), then the marks (a mispredicted branch or two
    // per prime, which would otherwise also discard the next prime's division
    // in flight). In the batch the u64 division beats a double quotient with
    // corrections (~6 % less time, Cascade Lake, znver3 build)
    constexpr uint32_t BATCH = 1024;
    uint32_t bp[BATCH];
    uint64_t bb[BATCH];
    uint8_t bw[BATCH];
    uint32_t p = 0, i = 0;
    for (; i < R.nmed; ++i) p = (i % LITE_GROUP == 0) ? bases[i / LITE_GROUP] : p + 2u * gaps[i];
    while (i < R.np) {
        const uint32_t n = std::min(BATCH, R.np - i);
        uint32_t nk = 0;                                               // primes with a mark in the segment
        for (uint32_t j = 0; j < n; ++j, ++i) {
            p = (i % LITE_GROUP == 0) ? bases[i / LITE_GROUP] : p + 2u * gaps[i];
            const uint64_t r = q_first % p;
            uint64_t d = r ? p - r : 0;                                // first multiple: q_first + d
            d += (d & 1) * p;                                          // odd multiple (q_first odd)
            const uint32_t m30 = uint32_t((qf30 + d) % 30) * uint32_t(inv30tab[p % 30]) % 30;   // multiplier mod 30 (odd)
            const uint64_t b = (d >> 1) + uint64_t(wskip[m30]) * p;
            bp[nk] = p;
            bb[nk] = b;
            bw[nk] = widx[m30];
            nk += b < q_odds;
        }
        for (uint32_t j = 0; j < nk; ++j) {
            const uint64_t pj = bp[j];
            uint64_t b = bb[j];
            uint32_t k = bw[j];
            do {
                const uint64_t t = b / 15;
                const uint32_t c = uint32_t(phase_class[b - 15 * t]);
                if (bucketed) {
                    const uint32_t bk = c * nreg + uint32_t(t >> RBITS);
                    marks[fill[bk]] = uint16_t(t);
                    if (++fill[bk] == (bk + 1) * cap) flush();
                } else {
                    R.map[c][t >> 6] &= ~(1ull << (t & 63));
                }
                b += wstep[k] * pj;
                k = (k + 1) & 7;
            } while (b < q_odds);
        }
    }
    if (bucketed) flush();
}

inline uint64_t bits_at(const uint64_t* a, uint64_t t)                  // 64 bits from bit t
{
    const uint64_t w = t >> 6, s = t & 63;
    return s ? (a[w] >> s) | (a[w + 1] << (64 - s)) : a[w];
}

void count_segment(Run& R, uint64_t seg_start, uint64_t seg_end, uint64_t q_first, uint64_t q_odds)
{
    uint32_t lim[32];
    if (!gb_count30_limits(seg_start, seg_end, q_first, q_odds, lim)) die(gb_error());
    auto range_count = [](const uint64_t* a, uint64_t lo, uint64_t hi) {
        uint64_t n = 0;
        for (uint64_t t = lo; t < hi; ) {
            if ((t & 63) == 0 && t + 64 <= hi) { n += __builtin_popcountll(a[t >> 6]); t += 64; continue; }
            const uint64_t e = std::min(hi, (t | 63) + 1);
            const uint64_t m = (e - t == 64) ? ~0ull : (((1ull << (e - t)) - 1) << (t & 63));
            n += __builtin_popcountll(a[t >> 6] & m);
            t = e;
        }
        return n;
    };
    for (int c = 0; c < 8; ++c) R.p30[c] += int64_t(range_count(R.map[c].data(), lim[2 * c], lim[2 * c + 1]));
    for (int j = 0; j < 3; ++j) {
        const uint32_t k = lim[16 + 5 * j], k2 = lim[17 + 5 * j], d = lim[18 + 5 * j];
        const uint64_t lo = lim[19 + 5 * j], hi = lim[20 + 5 * j];
        const uint64_t* a = R.map[k].data();
        const uint64_t* b = R.map[k2].data();
        uint64_t n = 0;
        for (uint64_t t = lo & ~63ull; t < hi; t += 64) {
            uint64_t x = a[t >> 6] & bits_at(b, t + d);
            if (t < lo) x &= ~0ull << (lo - t);
            if (hi - t < 64) x &= (1ull << (hi - t)) - 1;
            n += __builtin_popcountll(x);
        }
        R.t30[j] += int64_t(n);
    }
}

void note_cpu(Run& R, uint64_t n, uint32_t p)                         // engine.js addCpu
{
    const uint32_t cr = uint32_t(n % 30) / 2;
    const auto& cp = R.cand[cr].p;
    const auto it = std::find(cp.begin(), cp.end(), p);
    const size_t j = size_t(it - cp.begin());
    if (it != cp.end()) {
        R.hist[cr][j] += 1;
        if (p >= TAIL_T) {
            auto it = R.first.find(p);
            if (it == R.first.end() || n < it->second) R.first[p] = n;
            if (p >= RIDGE_FLOOR) R.ridges.push_back({n, p});
        }
    } else {
        R.beyond.push_back({n, p});
        if (p >= RIDGE_FLOOR) R.ridges.push_back({n, p});
    }
}

void check_segment(Run& R, uint64_t seg_start, uint64_t seg_evens)
{
    const uint32_t sm30 = uint32_t(seg_start % 30);
    for (uint32_t cr = 0; cr < 15; ++cr) {
        const uint32_t e0 = ((2 * cr + 30 - sm30) % 30) / 2;
        const uint64_t ec = seg_evens > e0 ? (seg_evens - e0 + 14) / 15 : 0;
        const Cand& cd = R.cand[cr];
        const uint32_t nc = uint32_t(cd.p.size());
        // U_j = (9781 - 2 phi_k + 2 e0 - p) / 30: bit of q = n - p in class k is i + U_j
        std::vector<int64_t> U(nc);
        for (uint32_t j = 0; j < nc; ++j) {
            const int64_t V = int64_t(MAX_GB_PRIME) - 2 * int64_t(R.phi[cd.k[j]]) + 2 * int64_t(e0) - int64_t(cd.p[j]);
            if (V % 30) die("checker: candidate class mismatch");
            U[j] = V / 30;
        }
        std::vector<const uint64_t*> A(nc);                            // class array of candidate j
        for (uint32_t j = 0; j < nc; ++j) A[j] = R.map[cd.k[j]].data();
        const uint32_t jtail = uint32_t(std::lower_bound(cd.p.begin(), cd.p.end(), TAIL_T) - cd.p.begin());
        uint64_t* hist = R.hist[cr].data();
        for (uint64_t i0 = 0; i0 < ec; i0 += 64 * W) {
            uint64_t acc[W];
            for (int w = 0; w < W; ++w) {
                const uint64_t ib = i0 + 64 * w;
                acc[w] = ib >= ec ? ~0ull : (ec - ib >= 64 ? 0 : ~0ull << (ec - ib));
            }
            uint32_t j0 = 0;
#if defined(__AVX2__)
            // 4 words per step in one 256-bit register while t >= 0 and p < TAIL_T
            // (those need per-bit first / ridge tracking: the scalar loop below).
            // No branch on the new hits (taken about half the time, mispredicted:
            // ~30 % of the checker's time): every step counts them. A shift by 64
            // gives 0, so sh = 0 needs no branch either
            {
                __m256i vacc = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(acc));
                const __m256i ones = _mm256_set1_epi64x(-1);
                for (; j0 < jtail; ++j0) {
                    if (_mm256_testc_si256(vacc, ones)) break;                 // all resolved
                    const int64_t t0 = int64_t(i0) + U[j0];
                    if (t0 < 0) break;
                    const uint64_t* a = A[j0] + (uint64_t(t0) >> 6);
                    const int sh = int(uint64_t(t0) & 63);
                    const __m256i lo = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(a));
                    const __m256i hi = _mm256_loadu_si256(reinterpret_cast<const __m256i*>(a + 1));
                    const __m256i h = _mm256_or_si256(_mm256_srl_epi64(lo, _mm_cvtsi32_si128(sh)),
                                                      _mm256_sll_epi64(hi, _mm_cvtsi32_si128(64 - sh)));
                    const __m256i nw = _mm256_andnot_si256(vacc, h);
                    vacc = _mm256_or_si256(vacc, h);
                    const __m128i n0 = _mm256_castsi256_si128(nw), n1 = _mm256_extracti128_si256(nw, 1);
                    hist[j0] += uint64_t(__builtin_popcountll(uint64_t(_mm_cvtsi128_si64(n0)))
                                         + __builtin_popcountll(uint64_t(_mm_extract_epi64(n0, 1)))
                                         + __builtin_popcountll(uint64_t(_mm_cvtsi128_si64(n1)))
                                         + __builtin_popcountll(uint64_t(_mm_extract_epi64(n1, 1))));
                }
                _mm256_storeu_si256(reinterpret_cast<__m256i*>(acc), vacc);
            }
#endif
            for (uint32_t j = j0; j < nc; ++j) {
                if ((acc[0] & acc[1] & acc[2] & acc[3]) == ~0ull) break;
                const uint64_t* a = R.map[cd.k[j]].data();
                const int64_t t0 = int64_t(i0) + U[j];
                const uint32_t p = cd.p[j];
                uint64_t cnt = 0;
                for (int w = 0; w < W; ++w) {
                    const int64_t t = t0 + 64 * w;
                    uint64_t h;
                    if (t >= 0) h = bits_at(a, uint64_t(t));
                    else if (t > -64) h = a[0] << (-t);                    // q below the map: none
                    else h = 0;
                    const uint64_t nw = h & ~acc[w];
                    acc[w] |= h;
                    if (!nw) continue;
                    cnt += __builtin_popcountll(nw);
                    if (p >= TAIL_T) {
                        for (uint64_t x = nw; x; x &= x - 1) {
                            const uint64_t n = seg_start + 2 * (e0 + 15 * (i0 + 64 * w + __builtin_ctzll(x)));
                            auto it = R.first.find(p);
                            if (it == R.first.end() || n < it->second) R.first[p] = n;
                            if (p >= RIDGE_FLOOR) R.ridges.push_back({n, p});
                        }
                    }
                }
                R.hist[cr][j] += cnt;
            }
            // beyond the candidate list: exact CPU scan
            for (int w = 0; w < W; ++w)
                for (uint64_t x = ~acc[w]; x; x &= x - 1) {
                    const uint64_t n = seg_start + 2 * (e0 + 15 * (i0 + 64 * w + __builtin_ctzll(x)));
                    const uint64_t p = gb_pn_exact(n);
                    if (p == 0) { std::printf("{\"counterexample\":\"%" PRIu64 "\"}\n", n); die("GOLDBACH COUNTEREXAMPLE candidate"); }
                    note_cpu(R, n, uint32_t(p));
                }
        }
    }
}

std::string result_json(Run& R)
{
    gb_finish_counts(R.start, R.limit, R.p30, R.t30);
    int64_t primes = 0, twins = 0;
    for (auto v : R.p30) primes += v;
    for (auto v : R.t30) twins += v;
    std::map<uint32_t, uint64_t> by_p;
    for (int cr = 0; cr < 15; ++cr)
        for (size_t j = 0; j < R.cand[cr].p.size(); ++j) if (R.hist[cr][j]) by_p[R.cand[cr].p[j]] += R.hist[cr][j];
    for (auto& b : R.beyond) by_p[b.second] += 1;
    uint64_t tail = 0; uint32_t pmax = 0;
    for (auto& [p, v] : by_p) if (p >= TAIL_T) { tail += v; pmax = std::max(pmax, p); }
    std::sort(R.ridges.begin(), R.ridges.end());
    std::sort(R.beyond.begin(), R.beyond.end());
    std::string s;
    char buf[96];
    auto add = [&](const char* fmt, auto... a) { std::snprintf(buf, sizeof buf, fmt, a...); s += buf; };
    add("{\"start\":\"%" PRIu64 "\",\"limit\":\"%" PRIu64 "\",\"evens\":\"%" PRIu64 "\",", R.start, R.limit, R.evens);
    add("\"primes\":\"%" PRId64 "\",\"twins\":\"%" PRId64 "\",\"primes30\":[", primes, twins);
    for (int c = 0; c < 8; ++c) add("%s\"%" PRId64 "\"", c ? "," : "", R.p30[c]);
    s += "],\"twins30\":[";
    for (int j = 0; j < 3; ++j) add("%s\"%" PRId64 "\"", j ? "," : "", R.t30[j]);
    add("],\"tailT\":%u,\"tailEvens\":%" PRIu64 ",\"pmax\":%u,\"histByP\":[", TAIL_T, tail, pmax);
    bool f = true;
    for (auto& [p, v] : by_p) { add("%s[%u,%" PRIu64 "]", f ? "" : ",", p, v); f = false; }
    s += "],\"histByClass\":[";
    for (int cr = 0; cr < 15; ++cr) {
        s += cr ? ",[" : "[";
        f = true;
        for (size_t j = 0; j < R.cand[cr].p.size(); ++j)
            if (R.hist[cr][j]) { add("%s[%u,%" PRIu64 "]", f ? "" : ",", R.cand[cr].p[j], R.hist[cr][j]); f = false; }
        s += "]";
    }
    s += "],\"first\":[";
    f = true;
    for (auto& [p, n] : R.first) { add("%s[%u,\"%" PRIu64 "\"]", f ? "" : ",", p, n); f = false; }
    s += "],\"beyond\":[";
    f = true;
    for (auto& [n, p] : R.beyond) { add("%s{\"n\":\"%" PRIu64 "\",\"p\":%u}", f ? "" : ",", n, p); f = false; }
    add("],\"ridgeFloor\":%u,\"ridges\":[", RIDGE_FLOOR);
    f = true;
    for (auto& [n, p] : R.ridges) { add("%s{\"n\":\"%" PRIu64 "\",\"p\":%u}", f ? "" : ",", n, p); f = false; }
    // prime gaps (core.cpp: the same object as the browsers', byte for byte)
    s += "],\"gaps\":";
    s += gb_gaps_finish(R.limit);
    s += "}";
    return s;
}

} // namespace

int main(int argc, char** argv)
{
    if (argc < 3) die("usage: cpu_worker START LIMIT [--segment S] [--primes FILE]");
    Run R;
    R.start = std::strtoull(argv[1], nullptr, 10);
    R.limit = std::strtoull(argv[2], nullptr, 10);
    R.segment = 1000000000;
    const char* pfile = nullptr;
    for (int a = 3; a + 1 < argc; a += 2) {
        if (!std::strcmp(argv[a], "--segment")) R.segment = std::strtoull(argv[a + 1], nullptr, 10);
        else if (!std::strcmp(argv[a], "--primes")) pfile = argv[a + 1];
        else die(std::string("unknown option ") + argv[a]);
    }
    if (R.start % 2 || R.limit % 2 || R.segment % 2 || R.limit < R.start || R.limit > LIMIT_MAX)
        die("even START <= LIMIT <= 5e18 and an even segment");
    if (R.start < 1000000000000000000ull) die("range too low for this checker (q_first must exceed p_max^2)");
    if (R.segment < 2 * MAX_GB_PRIME || R.segment > 60000000000ull) die("segment out of range");
    const auto t0 = std::chrono::steady_clock::now();
    load_primes(pfile);
    R.np = gb_prime_count();
    // medium primes and their 30^-1 mod p
    {
        const uint8_t* gaps = gb_gaps();
        const uint32_t* bases = gb_bases();
        uint32_t p = 0;
        for (uint32_t i = 0; i < R.np; ++i) {
            p = (i % LITE_GROUP == 0) ? bases[i / LITE_GROUP] : p + 2u * gaps[i];
            if (p >= MEDIUM_MAX) break;
            R.med.push_back({p, p > 5 ? powmod(30, p - 2, p) : 0});
        }
        R.nmed = uint32_t(R.med.size());
    }
    // candidate lists per class of evens (ascending), as engine.js prepareCheck
    for (uint32_t cr = 0; cr < 15; ++cr) {
        uint32_t out[1280];
        const uint32_t n = gb_cand_list(cr, out);
        for (uint32_t j = 0; j < n; ++j) { R.cand[cr].p.push_back(out[j] & 0xFFFF); R.cand[cr].k.push_back(out[j] >> 16); }
        R.hist[cr].assign(n, 0);
    }
    const uint64_t total = (R.limit - R.start) / 2 + 1;
    gb_gaps_begin(R.start);
    for (uint64_t s0 = R.start;; s0 += R.segment) {
        const uint64_t seg_end = (R.limit - s0 <= R.segment) ? R.limit : s0 + R.segment - 2;
        const uint64_t q_first = s0 - MAX_GB_PRIME;
        const uint64_t q_odds = (seg_end - 3 - q_first) / 2 + 1;
        const uint64_t seg_evens = (seg_end - s0) / 2 + 1;
        sieve_segment(R, q_first, q_odds);
        count_segment(R, s0, seg_end, q_first, q_odds);
        {
            // prime gaps of the segment (core.cpp gap walk); segment 0 starts at start + 1
            static std::vector<uint32_t> walk(5 + 1024 + 2 * 65536);
            const uint32_t cw32 = uint32_t(2 * R.map[0].size());
            const uint64_t mu = s0 == R.start ? R.start + 1 - q_first : 0;
            if (!gb_gap_walk64(R.map, cw32, s0, seg_end, q_first, q_odds, uint32_t(mu), 65536, walk.data()) ||
                !gb_gaps_segment(q_first, walk.data()))
                die("gap walk failed");
        }
        const uint64_t before = [&] { uint64_t s = R.beyond.size(); for (auto& h : R.hist) for (auto v : h) s += v; return s; }();
        check_segment(R, s0, seg_evens);
        const uint64_t after = [&] { uint64_t s = R.beyond.size(); for (auto& h : R.hist) for (auto v : h) s += v; return s; }();
        if (after - before != seg_evens) die("segment: evens not accounted exactly once");
        R.evens += seg_evens;
        std::fprintf(stderr, "progress %" PRIu64 " %" PRIu64 "\n", R.evens, total);
        if (seg_end == R.limit) break;
    }
    std::printf("%s\n", result_json(R).c_str());
    std::fprintf(stderr, "done %.1f\n", std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count());
    return 0;
}
