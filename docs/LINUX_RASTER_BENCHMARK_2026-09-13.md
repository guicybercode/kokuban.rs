# Linux raster candidate measurements — 2026-09-13

The packed-alpha candidate reduces **prewarmed CPU repaint time** in the measured Linux scenes, with a small regression for incremental ASCII updates to one row. This records measured candidate behavior; it does not establish input-to-screen latency, hardware GPU performance, general text-processing gains, or a ranking against other terminals.

## Sources and evidence

| Measurement | Successful run | Sources |
| --- | --- | --- |
| Real-font frame painting, Linux x86_64 and ARM64 | [34744867099](https://github.com/guicybercode/kokuban.rs/actions/runs/34744867099) | Common application/harness `1f2736405c9205a0ac765f98b5977f5d9a8f5581`; only `software_raster.rs` replaced between baseline and candidate |
| Kernel microbenchmarks and same-source controls | [34744980615](https://github.com/guicybercode/kokuban.rs/actions/runs/34744980615) | Driver `aceed7113731b662a9ea5cc408e4d5079afdeab5`; its candidate raster file is identical to `228bc137` |
| PTY/DSR processing, alternate screen | [34744420289](https://github.com/guicybercode/kokuban.rs/actions/runs/34744420289) | Complete baseline and candidate applications |
| PTY/DSR processing, primary screen | [34744421170](https://github.com/guicybercode/kokuban.rs/actions/runs/34744421170) | Complete baseline and candidate applications |

Baseline: [`199b987281dbb97dd481c0ff752a3bf80dc996cd`](https://github.com/guicybercode/kokuban.rs/commit/199b987281dbb97dd481c0ff752a3bf80dc996cd). Candidate: [`228bc1379cdf4f1ac179476b1fddba30bf2bba7a`](https://github.com/guicybercode/kokuban.rs/commit/228bc1379cdf4f1ac179476b1fddba30bf2bba7a).

The frame run preserves source manifests, source snapshots, compiler output, fresh release test binaries and their SHA256 hashes. Before/after manifests differ only at `src/software_raster.rs`; the common application and benchmark code match the recorded Git revision. These are release-optimized test executables, not newly published release assets.

## Real-font frame method

Ubuntu 24.04 runners used AMD EPYC 7763 (x86_64) and Neoverse-N2 (ARM64), Rust **1.94.1**, and CPU affinity **0**. Each architecture ran five alternating AB/BA pairs for both a same-binary A/A control and the candidate comparison. Each process exercised **12 modes**: ASCII, ASCII after an offscreen emoji entered the atlas, and Unicode; one-row or full-screen edits; forced full painting or incremental damage calculation. Each mode had 30 untimed warmup frames and 300 timed frames.

The grid was **120 × 40 cells**, with **9 × 17 pixels per cell**, producing **1080 × 680 pixels**. A one-row edit damaged rows of pixels `[340, 357)`; a full-screen edit damaged `[0, 680)`. Unicode included CJK, combining marks, emoji sequences and flags, with 2,800 monochrome and 800 color glyph cells in its initial scene. ASCII had 4,800 monochrome cells.

Fontconfig matched `DejaVu Sans Mono`, `sans:lang=ja`, and `emoji` to `DejaVuSansMono.ttf`, `NotoSansCJK-Regular.ttc`, and `NotoColorEmoji.ttf`. Recorded packages were `fonts-dejavu-core=2.37-8`, `fonts-noto-cjk=1:20230817+repack1-3`, `fonts-noto-color-emoji=2.047-0ubuntu0.24.04.1`, Fontconfig `2.15.0-1.1ubuntu2`, and FreeType `2.13.2+dfsg-1ubuntu0.1`. **The original artifacts retain package versions and font queries, but no font-file hashes or font binaries.**

Snapshot construction, font rasterization, reference comparisons, PTY traffic and window presentation are outside the timed interval. Incremental modes include damage calculation and CPU painting. The harness checks both transition directions against full-paint references and verifies the final pixels after each warmup/timed loop; it does not capture every intermediate timed frame.

Independent artifact checks found 40 complete process logs, 480 timing records and 144,000 timed repaints. All 96 retained reference files have exactly `1080 × 680 × 4` bytes; SHA256 and fixture checksums agree, each fixture's two states differ, and pixels match between baseline/candidate, A/A, and both architectures. Offscreen emoji insertion leaves the ASCII reference pixels unchanged. The original parser's incomplete fixture/dimension checks were addressed separately; these stronger checks were also applied directly to the original artifacts.

## Frame results

Values are the **median paired time change**, `median(100 × (after / before − 1))`; negative means less CPU time. They are not ratios of independently calculated medians.

| Content | Edit | Painting | x86_64 time change | ARM64 time change |
| --- | --- | --- | ---: | ---: |
| ASCII | One row | Full | −13.32% | −29.95% |
| ASCII | One row | Incremental | **+1.05%** | **+2.29%** |
| ASCII | Full screen | Full | −13.31% | −30.36% |
| ASCII | Full screen | Incremental | −13.39% | −30.24% |
| ASCII after emoji | One row | Full | −12.20% | −27.41% |
| ASCII after emoji | One row | Incremental | −0.33% | −0.50% |
| ASCII after emoji | Full screen | Full | −11.97% | −27.35% |
| ASCII after emoji | Full screen | Incremental | −12.11% | −27.46% |
| Unicode | One row | Full | −16.72% | −28.38% |
| Unicode | One row | Incremental | −0.16% | −2.10% |
| Unicode | Full screen | Full | −17.47% | −27.84% |
| Unicode | Full screen | Incremental | −17.23% | −27.99% |

The ASCII one-row incremental case was slower in **all five pairs** on both architectures: median paired increases of **2.70 µs x86_64** and **4.55 µs ARM64** per paint. A/A mode medians ranged from −0.73% to +0.17% time on x86_64 and −1.12% to +0.60% on ARM64. These descriptive controls do not justify dismissing the selective regression as noise.

Large repaint gains persist in both execution orders. One x86_64 candidate outlier remains in the results: the first ASCII-after-emoji/full-screen/forced-full pair had **−21.49% throughput**, while its other four pairs improved. The A/A data also retain an isolated +8.01% throughput pair in ASCII-after-emoji/full-screen/incremental mode. No pair was removed.

The measured tradeoff supports the repaint optimization: full painting saves roughly 125–240 µs per paint on x86_64 and 255–356 µs on ARM64, while the selective ASCII regression remains documented. Application-level benefit depends on the actual repaint workload.

## Microbenchmarks and processing checks

The microbenchmark run exercised 40 cases per platform: five kernels at widths `1, 7, 8, 9, 15, 16, 17, 32`, with seven alternating pairs, 100 frames and three warmup frames. It retained immediate pixel-equivalence checks as well as final pixels, avoiding correctness claims based only on repeated alpha-blending convergence. These synthetic kernels use no real fonts.

Image-kernel median paired throughput changes ranged from **+3.50% to +14.36% on Linux x86_64** and **+26.03% to +35.98% on Linux ARM64**. The macOS ARM64 microbenchmark regressed by 2.88–12.57% for that kernel. At the measured source, production macOS rendering uses Metal and has no production caller of `software_raster`; this synthetic result is not a measured macOS terminal regression.

Microbenchmark A/A controls are **not uniformly neutral**. Examples include x86_64 alpha/width7 at 0.9246× (AB median 0.9223×, BA 1.0855×), ARM64 opaque/width16 and width17 at 0.9166× and 0.9235×, and macOS opaque/width9 at 1.1780× despite identical functions. Linux width9 controls were close to 1×. Consequently these microbenchmarks are diagnostics; acceptance is supported by the real-font frame comparisons and pixel checks, rather than a global claim from the 40-case suite.

The processing runs used five alternating pairs, approximately 32 MiB per workload, 80 × 24 cells / 720 × 408 pixels, CPU 0 and headless Weston/Pixman. Median paired throughput changes for ASCII, ANSI, Unicode and short lines were respectively **−0.56%, −1.06%, +1.22%, +1.33%** on the alternate screen and **−0.20%, +0.53%, +0.05%, +1.06%** on the primary screen. They establish no broad text-processing gain. DSR acknowledges processing, not rendered pixels or presentation latency.

## Reproduce and retain evidence

The commands below dispatch the workflows on their measurement branches. Those branches can advance; each new run must retain its actual common-source/driver revision and should be treated as a new experiment. The measured workflow definitions remain available at [frame `1f273640`](https://github.com/guicybercode/kokuban.rs/blob/1f2736405c9205a0ac765f98b5977f5d9a8f5581/.github/workflows/linux-frame-repaint.yml), [micro `aceed711`](https://github.com/guicybercode/kokuban.rs/blob/aceed7113731b662a9ea5cc408e4d5079afdeab5/.github/workflows/software-raster-performance.yml), and [processing `228bc137`](https://github.com/guicybercode/kokuban.rs/blob/228bc1379cdf4f1ac179476b1fddba30bf2bba7a/.github/workflows/linux-paired-performance.yml).

```sh
baseline=199b987281dbb97dd481c0ff752a3bf80dc996cd
candidate=228bc1379cdf4f1ac179476b1fddba30bf2bba7a
gh workflow run linux-frame-repaint.yml --repo guicybercode/kokuban.rs \
  --ref perf/frame-repaint-control -f before="$baseline" -f after="$candidate"
gh workflow run software-raster-performance.yml --repo guicybercode/kokuban.rs \
  --ref perf/packed-alpha -f before="$baseline" -f after="$candidate"
for screen in alternate primary; do
  gh workflow run linux-paired-performance.yml --repo guicybercode/kokuban.rs \
    --ref perf/frame-repaint-control -f before="$baseline" -f after="$candidate" \
    -f screen="$screen" -f comparison=revisions
done
```

The workflows retain raw artifacts for **seven days**. Download them from the run links or with `gh run download RUN_ID --repo guicybercode/kokuban.rs --dir evidence/RUN_ID` before expiry. The local independent audit JSON files used for this report are working evidence, not permanent public URLs; the tables above preserve the summarized observations after artifact expiry.
