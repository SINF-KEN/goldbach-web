Golden fixtures for the coordinator's pure functions (`tests/golden.rs`).

They were generated once from the last Python coordinator (`server/server.py` at
commit 528714b) by running the old `tools/server_parity.py` corpus through it and
recording the outputs: result validation and its canonical JSON (59 cases, incl.
rejection messages), prime-gap validation, `is_prime` / `p(n)` / boundary counts
(506 numbers, primes.json), the board wide and narrow (6 inputs; the three
"production stats" variants are rebuilt from `test/stats_fixture.json`), Markdown
for agents (both pages and two snippets) and textwrap (300 cases).
The Rust server must reproduce them byte for byte. No Python is needed to run them.
