---
name: goldbach
description: Read the live state of the Goldbach Verification Network (goldbach.net01.io), which checks Goldbach's conjecture for every even number from 4e18 to 5e18; find progress, record minimal Goldbach partitions, prime and twin-prime counts and the live paper's statistics, or the minimal Goldbach partition of a given even number.
---

# Goldbach Verification Network

goldbach.net01.io verifies that every even number n from 4×10^18 to 5×10^18 is a sum of two primes.
A native GPU verifier works up from 4×10^18, volunteer browsers work down from 5×10^18. All data
is public; no key, account or header is needed.

## Quick answers

- Progress, speed, time left, top records, devices: `GET https://goldbach.net01.io/board`
  (plain text, 76 columns; `?narrow=1` gives 44). Quote it as is: it is written to be read.
- The same as JSON: `GET https://goldbach.net01.io/api/stats` (accepts gzip). Big integers are
  strings. `range`: the whole interval; `native`: the GPU verifier (`resume` = how far it has
  checked from 4e18); `web`: the browsers (`lowest` = how far down from 5e18); `ridges`: record p(n).
- The live paper's data (tables and figures of Oliveira e Silva, Herzog and Pardi, Math. Comp. 83,
  2014, above 4×10^18): `GET https://goldbach.net01.io/api/results`.
- One even number: `GET https://goldbach.net01.io/api/partition?n=<even n>` returns p(n), the partition,
  and `verifiedByNative` (has the GPU verifier covered it yet). Exact, for any even 4 ≤ n < 2^64.
- Everything in one file, with citation guidance: `https://goldbach.net01.io/llms-full.txt`.
- Any page as Markdown: send `Accept: text/markdown` to `/` or `/paper`.

## Terms

- p(n): the smallest prime p such that n − p is also prime (the minimal Goldbach partition).
- A ridge: an even n with an unusually large p(n); the board lists the top 30 found so far.
- S(p): the least even n with p(n) = p. P(g): the first prime followed by a gap of exactly g.
- A slice: a block of 10^12 integers (browsers) or 5×10^13 (the native GPU verifier).

## MCP

An MCP server (Streamable HTTP, no authentication) is at `https://goldbach.net01.io/mcp` with the
tools `goldbach_board`, `goldbach_stats`, `goldbach_paper_data` and `goldbach_partition` (p(n) and
its partition for any even 4 ≤ n < 2^64, computed exactly on request). Card:
`https://goldbach.net01.io/.well-known/mcp/server-card.json`.

## Good manners

Data refreshes about every 15 seconds (the paper data every 10 minutes). Poll no more than once a
minute. Every POST is limited to 2 a second per address.
