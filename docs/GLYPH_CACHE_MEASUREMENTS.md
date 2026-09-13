# Warmed glyph-cache measurements

This benchmark measures elapsed time for cached glyph lookups on Linux x86-64,
Linux ARM64 and macOS ARM64. It does not measure rasterization, GPU work,
presentation or input latency.

Each deterministic, shuffled workload performs 384 lookups per iteration:

- `ascii-regular`: printable ASCII, regular style.
- `ascii-four-styles`: ASCII with regular, bold, italic and bold-italic styles.
- `mixed-unicode`: equal ASCII/non-ASCII scalar coverage, four styles.
- `unicode-scalars`: only non-ASCII scalars, four styles.
- `graphemes`: combining marks and emoji sequences, four styles.

The runner injects one identical ignored Rust test into fresh Git source
archives. It builds release test executables in separate, new Cargo targets,
then runs six process pairs alternating AB/BA. `same-binary` builds once and
executes the same path and SHA-256 under both labels. Each process populates
the cache and warms every workload before timing. Glyph counts, ordered entry
fingerprints and complete atlas-buffer fingerprints must remain equal; checks
and font loading occur outside the timer.

Dispatch from the repository with GitHub CLI:

```sh
gh workflow run glyph-cache-performance.yml --ref perf/glyph-cache-controls \
  -f comparison=revisions \
  -f before=b6cd9ad8b1204a4cb67587ecb042bd5cb8986266 \
  -f after=ddccc9bc4146629c34c089486d56897b0b3e3515 \
  -f iterations=10000 -f warmup=1000

gh workflow run glyph-cache-performance.yml --ref perf/glyph-cache-controls \
  -f comparison=same-binary \
  -f before=b6cd9ad8b1204a4cb67587ecb042bd5cb8986266 \
  -f iterations=10000 -f warmup=1000
```

Defaults are 1,000 timed iterations and 100 warmups; both accept integers
1–100,000. Rust is pinned to 1.94.1, dependencies use `Cargo.lock`, and source
revisions resolve to commits. Runner images and CPU models can change between
runs: artifacts retain OS/CPU/compiler metadata, effective font name, font-file hashes, source manifests,
archives, exact binaries, raw logs and report JSON. Compare revisions within
each runner; native `monospace` selection can differ across platforms.

`Instant` measures elapsed wall time, including scheduling interruptions, not
CPU cycles. Linux processes share one pinned CPU; macOS affinity is not
enforced. Longer samples can reduce short-window noise but cannot eliminate
host interference. Report the median paired percentage change, individual
pairs and A/A controls; negative changes mean less elapsed time. Keep separate
rounds separate. A/A noise describes that control host, not a universal
acceptance threshold.

`atlas_bytes` and `scalar_cache_bytes` use Rust `size_of_val`: inline structure
storage only, excluding backing heap allocations and process RSS.
