# Rejected row-baseline reuse variant — 2026-09-13

**Decision: keep `6a49dfb` out of main.** The measured implementation saves modest CPU repaint time on ARM and for full ASCII frames on x86, but regresses x86 incremental updates and Unicode repaint. This rejects this variant; it is not proof that reusing glyph baselines is always harmful. No universal performance threshold or statistical significance claim is implied.

[Run 34748686411](https://github.com/guicybercode/kokuban.rs/actions/runs/34748686411) compares `ddccc9bc4146629c34c089486d56897b0b3e3515` with `6a49dfbafab15b27b3b554672fbc5fa9eb436d37`, using common harness `62c99adc6fc1dbf12d89ee717f5823644c162d47`. Only `src/linux_window.rs` differs: the rounded baseline is calculated once per grid row and reused for ink filtering/painting. Both builds already contain the accepted damage filter and ASCII cache.

The static correctness review found no blocker. The experiment verified identical reference pixels, original/prepared source manifests against 1,379 Git blobs per original snapshot, fresh optimized Cargo build records and the retained ELF binaries. The benchmark logs have 733 filtered tests before and 734 after; that records one added test, not execution of the entire test suite.

## Paired results

These are **medians of six paired time changes**, `100 × (after / before − 1)`; negative is faster. “Full” forces a whole-frame paint. Both modes cover the whole frame for full-screen edits. The runners were Intel Xeon 6973P-C and ARM Neoverse-N2, each pinned to logical CPU 0.

| Content | Edit | Mode | x86 time change | ARM time change | Slower pairs, x86 / ARM |
|---|---|---|---:|---:|---:|
| ASCII | single row | full | −1.720% | −0.804% | 0/6 / 0/6 |
| ASCII | single row | incremental | +0.799% | −1.338% | 5/6 / 0/6 |
| ASCII | full screen | full | −1.831% | −0.562% | 0/6 / 0/6 |
| ASCII | full screen | incremental | −1.604% | −0.520% | 1/6 / 0/6 |
| ASCII after emoji | single row | full | −1.575% | −0.790% | 1/6 / 0/6 |
| ASCII after emoji | single row | incremental | +1.034% | −1.305% | 6/6 / 0/6 |
| ASCII after emoji | full screen | full | −1.330% | −0.680% | 2/6 / 0/6 |
| ASCII after emoji | full screen | incremental | −1.409% | −0.775% | 2/6 / 0/6 |
| Unicode | single row | full | +0.671% | −1.542% | 6/6 / 0/6 |
| Unicode | single row | incremental | +2.721% | −0.367% | 6/6 / 1/6 |
| Unicode | full screen | full | +0.751% | −2.083% | 6/6 / 0/6 |
| Unicode | full screen | incremental | +0.991% | −2.052% | 6/6 / 0/6 |

The x86 incremental costs are +0.366 µs ASCII, +0.472 µs ASCII after emoji and +2.146 µs Unicode (paired medians). All 18 x86 nonselective Unicode pairs regress, with +5.913 to +8.693 µs median costs. Overall, x86 has six favorable and six unfavorable mode medians, with 31/72 pairs faster. ARM improves in all 12 mode medians and 71/72 pairs; its sole slowdown is Unicode incremental, +0.335%.

Execution order is AB, BA, AB, BA, AB, BA. For every mode, both order medians have the same sign as its overall paired median. [summary.json](summary.json) retains all individual pairs, AB/BA medians, absolute times/deltas, slower-pair counts and ranges; nothing is removed. Notable x86 outliers include ASCII-after-emoji/full-screen/incremental pair 3 at **+13.503%**, Unicode/single-row/incremental pair 3 at **+10.624%**, and ASCII/full-screen/full pair 4 at **−6.668%**. They remain in the reported medians.

## Scope and preserved evidence

Each process measures 12 modes with 30 warmup and 300 timed frames per mode. The fixed geometry is 120×40 cells, 9×17 pixels each, in a 1080×680 XRGB frame. The atlas is prewarmed with DejaVu Sans Mono at 14 px; CJK and color emoji fonts are available. Rust is 1.94.1. The 45 font paths shared between architectures have equal recorded hashes; x86 additionally has 12 Liberation files. Installed font sets are not globally identical, although all reference pixels match.

This measures CPU damage/software repaint, excluding PTY throughput, cold glyph rasterization, presentation, GPU and physical input-to-screen latency. Six paired processes on one runner per architecture are descriptive samples, not independent machines. There is no same-binary control in this run; earlier controls cannot be used as corrections or transferable noise thresholds. Do not add these percentages to previous filter/cache results or compare absolute times across hosts as a causal effect.

The package maps **161 original paths to 90 lossless XZ objects**, about 0.7 MB including metadata. It retains both raw reports, all 24 complete logs / 288 timing records, all 48 reference-frame paths, Cargo messages/build logs/trees, source manifests, runner/workflow/injection source, font/package metadata, download-integrity records and independent audit results. The 48 frame paths contain six distinct byte sequences, each 2,937,600 bytes; both scene states differ and before/after pixels match across architectures and the earlier audited macro. Intermediate timed frames were not retained.

The original audit script is directly readable as [audit-source.py](audit-source.py), and both audit/download scripts are also preserved as objects. That audit used the full temporary workspace, Git history and earlier golden frames. It is retained for review, not as the portable verification entry point.

Four ELF executables, eight source tars and two downloaded ZIPs are omitted from Git. Their verified original/stored sizes and SHA-256 values are in [manifest.json](manifest.json); ZIP hashes matched the GitHub API and source tars were gzip-compressed with decoded hashes checked. The compact package cannot independently rehash those omitted bytes. Font binaries were not retained. GitHub artifacts have seven-day retention; the committed subset is independent of their expiry.

## Verify without rerunning the experiment

From this directory:

```sh
python3 verify.py
```

Expected: `PASS` for 90 objects, 161 paths, 24 logs, 288 records and 48 frame references. The verifier checks compressed/original hashes, Cargo provenance, source manifests, paired/order statistics and frame size/SHA/FNV. It does not build, benchmark, access the network or validate omitted binaries.

Optionally reconstruct all retained original paths into a **new** directory; allow about 150 MB for expanded copies:

```sh
python3 verify.py --extract /tmp/kokuban-row-baseline-evidence
```
