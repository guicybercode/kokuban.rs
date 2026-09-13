# PTY processing with primary history, alternate screen and an A/A control

The integrated damage filter and ASCII cache have a measured **primary-screen
`short_lines` cost: +9.127% elapsed time in all five pairs** in run 34749172188.
The separate A/A control varies by +1.173% at its median, but uses a different
CPU; it cannot explain away that A/B result. The alternate-screen repeats do
not reproduce the same cost. These observations warrant investigation of the
primary workload and preserve a tradeoff alongside the earlier repaint gains.
They do not measure keyboard-to-screen latency or establish the cause of the
cost.

## Four preserved runs

All A/B runs compare
[`d54f801e9dec41a6eb4f159397e45939c367531d`](https://github.com/guicybercode/kokuban.rs/commit/d54f801e9dec41a6eb4f159397e45939c367531d)
with the integrated
[`62c99adc6fc1dbf12d89ee717f5823644c162d47`](https://github.com/guicybercode/kokuban.rs/commit/62c99adc6fc1dbf12d89ee717f5823644c162d47).
Only `src/glyph_atlas.rs` and `src/linux_window.rs` differ among production
sources; `Cargo.toml` and `Cargo.lock` match. The A/A builds `d54f801` once and
uses that same executable path and SHA256 for both labels.

| Run | Screen / configured history | Harness | CPU model |
| --- | --- | --- | --- |
| [34749172188](https://github.com/guicybercode/kokuban.rs/actions/runs/34749172188) | Primary / 10,000 lines, A/B | `ef8f63f1f5f3e9012d8b9f33b3183742059be47a` | AMD EPYC 7763 |
| [34749322755](https://github.com/guicybercode/kokuban.rs/actions/runs/34749322755) | Alternate / 0 lines, A/B | `ec3b4ea36f83a9dba519f150b4997f8e2fc57335` | AMD EPYC 7763 |
| [34749492893](https://github.com/guicybercode/kokuban.rs/actions/runs/34749492893) | Alternate / 0 lines, A/B, unplanned repeat | `72dd68a1262d360d119fb9459ea12a47303bae5d` | AMD EPYC 7763 |
| [34749802898](https://github.com/guicybercode/kokuban.rs/actions/runs/34749802898) | Primary / 10,000 lines, same-executable A/A | `3ed42aae591bb53eeb3b28fb0270387b0e0c5a3d` | Intel Xeon Platinum 8573C |

The first alternate run was created despite an HTTP 500 response and a delayed
run listing. A second submission occurred before the first became visible. This
scheduling account comes from the maintainer's session instructions, preserved
in both `scheduling-note.json` files. Both runs remain separate; neither is
selected away or pooled. Their four retained harness source files have identical
bytes, despite different harness commits. A shared CPU model does not prove the
same host instance.

## Results and interpretation

Values below are the **median of five paired elapsed changes**,
`100 * (after / before - 1)`. Positive means slower. They are not ratios of
separately computed medians. Every pair, order median, range, CPU/RSS observation
and protocol distribution is retained in [summary.json](summary.json), with the
original reports and audits inside the run archives.

| Workload | Primary A/B | Alternate A/B #1 | Alternate A/B #2 | Primary A/A |
| --- | ---: | ---: | ---: | ---: |
| ASCII | +3.451% | +1.122% | +1.014% | +0.039% |
| ANSI | −4.232% | −2.082% | −3.068% | −0.916% |
| Unicode | −2.037% | +1.676% | −0.246% | +0.209% |
| Short lines | **+9.127%** | −0.379% | −0.381% | +1.173% |

Primary `short_lines` costs range from **+8.715% to +9.819%**, with order medians
+9.015% for AB and +9.473% for BA. The median paired elapsed increase is
**105.319 ms** per approximately 32 MiB workload; median reported terminal CPU
rises from **1.10 s to 1.21 s**. Paired throughput declines by **8.364%**. This
consistent cost remains material even though other workloads or repaint tests
improve.

The A/A `short_lines` range is **−1.671% to +4.284%**, with three of five pairs
slower, AB median −0.671% and BA +1.988%. Its different CPU and CI instance
prevent subtracting its median or treating its range as a causal noise threshold
for the primary A/B. A/A ASCII also retains a +6.438% individual outlier.

The approximately −0.38% alternate `short_lines` medians are small and
order-sensitive: AB/BA medians are −1.320%/+0.301% in the first run and
+1.803%/−1.548% in the second. Unicode changes direction between alternate runs.
These data do not establish a stable alternate-screen speedup. Primary versus
alternate across separate runs is also not a controlled estimate of the cost
of history storage. Do not sum these percentages with earlier repaint gains,
combine runs into a larger sample, or rank terminals from this experiment.

## Measurement and provenance limits

Each run uses five pairs in order **AB, BA, AB, BA, AB**: three AB and two BA,
not a fully balanced design. Ten fresh terminal processes produce 40 workload
observations and 300 recorded protocol RTTs per run. The package therefore
contains **40 processes, 160 workload observations and 1,200 RTTs**. Thirty RTTs
per process follow five excluded RTT warmups; RTT samples within a process are
not independent process replications.

The environment is native Wayland under Weston 13.0.0 headless/Pixman, with
`DISPLAY` absent and affinity pinned to CPU 0. PTY dimensions remain **80 × 24
cells and 720 × 408 pixels** (9 × 17 pixels per cell). Fontconfig selects DejaVu
Sans Mono at 14 px; package records include DejaVu 2.37-8, Fontconfig 2.15.0 and
FreeType 2.13.2. Rust is 1.94.1. Exact package strings and font-selection output
are retained; these workflows did not retain font file hashes.

Each deterministic workload is approximately 32 MiB. Its full warmup is followed
by a clear-history/clear-screen operation before the timed write and final DSR
barrier. All 160 observations agree on payload size/SHA256, dimensions and the
expected `ESC[1;5R` reply. The verifier regenerates payload hashes without writing
payload files or executing the application. In alternate mode the effective
history configuration is zero, even though the report retains the unused
10,000-line CLI default. Actual history contents, rendered pixels and presentation
are not independently inspected here.

Write plus final DSR includes PTY processing and scheduling; it does not isolate
CPU repaint, GPU work or physical presentation. Process CPU counters have 10 ms
tick resolution, and raw terminal tick endpoints were not uploaded. RSS samples
after warmup/history operations are not peak RSS, leak tests or a memory delta
attributable solely to the cache. Each terminal startup logs one nonfatal
100 ms portal lookup timeout before measured work; Weston logs a finalization
warning after shutdown SIGTERM. All successful observations and those logs remain
in the package.

The three older A/B workflows retain separate build paths, source refs, Cargo
inputs, dependency trees, build completion logs and runner-reported ELF hashes.
They **did not upload executable bytes, source archives or Cargo compiler-artifact
JSON**, so their binary/source association cannot be independently reconstructed
from those artifacts alone. Their reported hashes differ by revision. The later
A/A independently rehashed its retained x86-64 ELF, checked one fresh optimized
non-test Cargo artifact, and validated all **1,379 Git blobs** in the source
archive against `d54f801`. That stronger A/A proof does not retroactively prove
the older A/B executions. A/A source/ELF bytes are omitted from this compact Git
package, with their original hashes and sizes retained explicitly.

## Offline verification and retained bytes

From the repository root:

```sh
python3 docs/linux-evidence/2026-09-13-pty-controls/verify.py
```

Python's standard library verifies each compressed archive and raw file against
[manifest.json](manifest.json), matches the original GitHub artifact digest
metadata, checks configurations and executable labels, regenerates payload
hashes, and recalculates paired, order and per-revision distributions. It makes
no network request and runs no build or terminal benchmark. To inspect raw files,
extract a chosen `runs/run-*.tar.xz` with a standard tar/XZ reader. Archive paths
are relative to the original run directory.

The four lossless archives preserve **394 raw paths**, including reports, all
40 process logs/configurations/results, CI logs, metadata, original independent
audits and exact harness sources. The three original audit programs are retained
under [audit-source/](audit-source/); their environment-specific paths describe
the original audit and are not prerequisites for `verify.py`. Original ZIPs and
the A/A ELF/source archive are omitted with measured SHA256 and byte counts.
Omissions cannot be rehashed from this package alone. The original artifact
retention is seven days; run links and recorded metadata are provenance, not a
promise that GitHub will retain downloads indefinitely. Historical local paths
inside raw audits refer to the original download, not additional published files.
