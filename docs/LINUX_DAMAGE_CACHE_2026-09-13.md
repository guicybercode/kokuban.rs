# Linux damage filtering and ASCII glyph-cache measurements — 2026-09-13

The combined candidate reduces CPU repaint time substantially for ASCII and single-row incremental updates. It also has a small, consistent regression for full Unicode repaint on x86. These are measurements of candidate `ddccc9b`, compared directly with `d54f801`; they are not a claim that every workload improves.

**Status at this evidence snapshot:** the candidate is under review, with separate macOS glyph-lookup performance validation pending. [Linux and macOS CI passed on the exact candidate](https://github.com/guicybercode/kokuban.rs/actions/runs/34746934511). That verifies correctness checks and compilation, not macOS performance. The candidate is not integrated into the `8c12b23` main revision from which this report was prepared.

## Direct aggregate result

[Run 34747336355](https://github.com/guicybercode/kokuban.rs/actions/runs/34747336355) compares `d54f801e9dec41a6eb4f159397e45939c367531d` with `ddccc9bc4146629c34c089486d56897b0b3e3515`. Only `src/linux_window.rs` and `src/glyph_atlas.rs` differ. The x86 runner reports AMD EPYC 9V74; ARM reports Neoverse-N2. Both use one pinned logical CPU.

Each percentage below is the **median of six paired changes in time**, calculated as `100 × (after_ns / before_ns − 1)`. Negative means faster. The final column preserves the number of slower pairs, first x86 then ARM. “Full paint” forces a whole-frame repaint even when only one row changes. A full-screen edit covers the whole frame in either mode.

| Content | Edit | Paint mode | x86 time change | ARM time change | Slower pairs: x86; ARM |
|---|---|---|---:|---:|---:|
| ascii | single-row | full paint | -11.25% | -10.02% | 0/6; 0/6 |
| ascii | single-row | incremental | -71.25% | -55.76% | 0/6; 0/6 |
| ascii | full-screen | full paint | -11.23% | -10.09% | 0/6; 0/6 |
| ascii | full-screen | incremental | -11.23% | -10.18% | 0/6; 0/6 |
| ascii-after-emoji | single-row | full paint | -11.38% | -10.45% | 0/6; 0/6 |
| ascii-after-emoji | single-row | incremental | -71.21% | -55.78% | 0/6; 0/6 |
| ascii-after-emoji | full-screen | full paint | -11.30% | -10.47% | 0/6; 0/6 |
| ascii-after-emoji | full-screen | incremental | -11.33% | -10.31% | 0/6; 0/6 |
| unicode | single-row | full paint | +1.95% | -0.62% | 6/6; 2/6 |
| unicode | single-row | incremental | -38.49% | -35.74% | 0/6; 0/6 |
| unicode | full-screen | full paint | +1.53% | -0.59% | 6/6; 1/6 |
| unicode | full-screen | incremental | +1.28% | -0.82% | 6/6; 0/6 |

All ASCII modes and all three single-row incremental workloads improve in every pair on both architectures. Their AB and BA order medians have the same favorable sign. Overall, 21 of 24 mode medians improve, and 123 of 144 pairs are faster.

**The x86 Unicode regression is real in this sample:** all 18 nonselective pairs are slower. Median paired costs are +21.48 µs for single-row/full paint, +16.72 µs for full-screen/full paint, and +14.09 µs for full-screen/incremental. Those are the +1.95%, +1.53% and +1.28% rows above. Individual slowdowns reach +4.04%; none are discarded. This is an explicit tradeoff, not performance equivalence.

ARM Unicode nonselective medians are −0.59% to −0.82%, with three slower pairs across the three modes. That small, mixed result does not establish a Unicode full-repaint gain. Likewise, one x86 ASCII-after-emoji/full-screen/incremental pair improves only 1.98%, versus a median 11.33%; it remains in the data.

## What changed and what was timed

The damage filter restricts background work to affected rows and rejects glyph ink that cannot intersect the damage band before color resolution and composition/blitting. It still visits visible grid rows and calls `get_or_insert_cell`; a cache miss outside the band can still rasterize a glyph. **This is not evidence that cold rasterization is avoided.** Underlines and glyph overhang retain independent intersection handling.

The scalar glyph cache replaces hashed ASCII lookups with 512 direct slots: 128 ASCII values × four bold/italic combinations. Non-ASCII scalars retain keyed storage, and cached empty glyphs remain present. Grapheme handling remains separate. This shared cache also warrants the pending macOS lookup measurement; the Linux damage filter alone does not characterize Metal rendering.

The macro uses a **prewarmed glyph atlas**, 120×40 cells, 9×17 pixels per cell and a 1080×680 XRGB frame. DejaVu Sans Mono is requested at 14 px, with Noto CJK and Noto Color Emoji available. The three contents are ASCII, ASCII with an atlas previously populated with emoji, and a mixed Unicode fixture. The latter is not a pure non-ASCII-only workload.

Each architecture runs six balanced process pairs in order AB, BA, AB, BA, AB, BA. Each process measures all 12 modes, with 30 warmup frames and 300 timed frames per mode. A recorded observation is average nanoseconds per frame for those 300 iterations. These six pairs are descriptive samples, not six independent machines or a significance threshold.

The timed region covers CPU damage calculation and software paint. Snapshots, font shaping/rasterization warmup, PTY/parser throughput, window presentation, GPU work, physical display latency and application startup are outside this measurement. Do not translate these percentages into typing latency or terminal rankings.

Both alternating fixture states are compared against full repaint outside timing; final timed state is also checked. The retained reference frames are byte-identical between builds and architectures. They do not record every intermediate timed frame.

## Incremental experiments and rejected alternative

The evidence preserves all five runs, not just the strongest result:

| Run | Comparison | Interpretation |
|---|---|---|
| [34747336355](https://github.com/guicybercode/kokuban.rs/actions/runs/34747336355) | `d54f801 → ddccc9b` | Direct aggregate result above; primary basis for the integration tradeoff. |
| [34746964325](https://github.com/guicybercode/kokuban.rs/actions/runs/34746964325) | `b6cd9ad → ddccc9b` | ASCII-cache increment: all 24 medians improve; 143/144 pairs faster. The sole slowdown is x86 Unicode/full-screen/full paint, pair 3, +2.014%. |
| [34746560462](https://github.com/guicybercode/kokuban.rs/actions/runs/34746560462) | `d54f801 → b6cd9ad` | Filter increment: substantial selective gains, but all 54 nonselective ARM pairs slower; ASCII modes roughly +3.1–3.4%. |
| [34746780302](https://github.com/guicybercode/kokuban.rs/actions/runs/34746780302) | `b6cd9ad → 4250ca5` | **Rejected constant-specialization experiment.** ARM single-row incremental medians regress +15.15% ASCII, +14.37% ASCII-after-emoji and +8.16% Unicode, all 6/6. Nonselective ARM ASCII modes regress +3.38–3.92%. It is not part of `ddccc9b`. |
| [34746268790](https://github.com/guicybercode/kokuban.rs/actions/runs/34746268790) | one `55c48f4` binary/path used as A and B | Contextual A/A: mode medians −0.865% to +0.326% x86 and −0.442% to +0.553% ARM. It uses an earlier baseline and separate hosts. |

Do not add percentages between these runs or compare their absolute times as a causal code effect. Host models and scheduling differ. The historical A/A results are not an adjustment to subtract from the candidate or a transferable noise cutoff. The primary aggregate result is measured directly.

## Preserved evidence and offline verification

The [manifest](linux-evidence/2026-09-13-damage-cache/manifest.json) maps 713 original artifact paths to 262 lossless XZ objects, deduplicated by their uncompressed SHA-256. Both compressed and original byte counts/hashes are recorded. The package is approximately 1.8 MB, representing about 726 MB of mapped original bytes, principally repeated frame references.

It retains all ten raw 12-mode reports, 120 complete process logs, 1,440 timing records, full Cargo build messages/logs/dependency trees, original/prepared source manifests, source-archive and executable checksums, benchmark/injection source, runner/workflow source, font file hashes/match queries, run metadata and the independent audit reports. All 240 reference-frame paths remain reconstructible; identical frame bytes are stored once. [Summary JSON](linux-evidence/2026-09-13-damage-cache/summary.json) preserves each paired percentage, range, execution-order median and slower-pair count.

The original and prepared source archives and ELF executables were independently checked during the audits and their SHA-256 verified again during export. Their large byte contents are omitted from Git; exact source revisions, manifests, Cargo proof and verified sizes/hashes remain. Font file bytes were not retained by the original workflow. All 45 font paths shared between the two architectures have equal recorded hashes; x86 additionally has 12 Liberation files, so the installed font sets are not globally identical. The actual fixture pixels are identical.

From the repository root, verify the retained bytes, reparse every process log, recompute paired statistics, check fresh optimized Cargo records, and validate frame size/SHA/FNV:

```sh
python3 docs/linux-evidence/2026-09-13-damage-cache/verify.py
```

Expected result: `PASS` with 262 objects, 713 paths, 120 logs, 1,440 records and 240 frame references. This performs no build, benchmark or network request. Optionally reconstruct the original retained files into a **new** directory; allow about 726 MB for the expanded copies:

```sh
python3 docs/linux-evidence/2026-09-13-damage-cache/verify.py --extract /tmp/kokuban-damage-cache-evidence
```

The independent audit summaries distinguish verified executable/source bytes available during audit from this compact subset. Original GitHub artifacts use seven-day retention; their run URLs are not a promise of permanent artifact availability.

## Repeat the aggregate measurement

The [paired CPU frame workflow](../.github/workflows/linux-render-performance.yml) is already on main. With repository Actions permission, GitHub CLI authentication and the commits available remotely, dispatch the exact application refs:

```sh
gh workflow run linux-render-performance.yml --repo guicybercode/kokuban.rs --ref main \
  -f comparison=revisions \
  -f before=d54f801e9dec41a6eb4f159397e45939c367531d \
  -f after=ddccc9bc4146629c34c089486d56897b0b3e3515
```

Use the other before/after pairs in the table to repeat their comparisons. For a same-binary control on the aggregate baseline, select `comparison=same-binary`, set `before=d54f801e9dec41a6eb4f159397e45939c367531d`, and supply the same value for the required but ignored `after` input. The workflow builds one executable and uses its identical path for both labels.

The recorded original harness is `410d947` for the aggregate, ASCII-cache and rejected-specialization runs; `1bff6b8` for the filter increment and contextual A/A. Their benchmark function is identical. Rust is 1.94.1; native Ubuntu 24.04 package versions and font hashes are retained per run. A future main workflow or runner image may differ, so compare its recorded harness, build, geometry and font provenance before treating it as the same experiment.
