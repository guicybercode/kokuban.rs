# Portable warmed glyph-cache evidence — 2026-09-13

Four independent CI runs compare `b6cd9ad8b1204a4cb67587ecb042bd5cb8986266` with
`ddccc9bc4146629c34c089486d56897b0b3e3515`, plus exact same-binary controls.
Each artifact contains six AB/BA process pairs and five workloads. No samples are pooled across runs.

| Round | Comparison | Iterations / warmup | Harness | Run |
| --- | --- | --- | --- | --- |
| Initial | revisions | 1000 / 100 | `af046a6` | [34747921774](https://github.com/guicybercode/kokuban.rs/actions/runs/34747921774) |
| Initial | same-binary | 1000 / 100 | `af046a6` | [34747922686](https://github.com/guicybercode/kokuban.rs/actions/runs/34747922686) |
| Long | revisions | 10000 / 1000 | `8d7b141` | [34748283639](https://github.com/guicybercode/kokuban.rs/actions/runs/34748283639) |
| Long | same-binary | 10000 / 1000 | `8d7b141` | [34748285006](https://github.com/guicybercode/kokuban.rs/actions/runs/34748285006) |

The values below are median **paired elapsed-time changes** (%); negative means faster.
They are not end-to-end terminal, rasterization, GPU or input-latency measurements.

| Run | Hardware | ASCII | Four styles | Mixed | Unicode scalars | Graphemes |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| 34747921774 | Apple M1 (Virtual) | -84.42 | -84.27 | -40.46 | -16.92 | +2.53 |
| 34747921774 | Neoverse-N2 | -54.82 | -54.91 | -29.58 | -4.67 | +0.57 |
| 34747921774 | AMD EPYC 7763 64-Core Processor | -74.46 | -74.70 | -48.43 | -18.54 | -2.28 |
| 34747922686 | Apple M1 (Virtual) | +2.27 | -1.32 | +4.45 | +4.89 | -9.39 |
| 34747922686 | Neoverse-N2 | +0.57 | +0.30 | -0.41 | +0.02 | +0.67 |
| 34747922686 | AMD EPYC 7763 64-Core Processor | +0.08 | -0.11 | +0.11 | +0.62 | +0.36 |
| 34748283639 | Apple M1 (Virtual) | -79.29 | -80.25 | -47.94 | -17.98 | +4.46 |
| 34748283639 | Neoverse-N2 | -54.92 | -54.69 | -28.34 | -3.39 | +0.26 |
| 34748283639 | INTEL(R) XEON(R) PLATINUM 8573C | -60.38 | -60.02 | -33.85 | -2.19 | +0.72 |
| 34748285006 | Apple M1 (Virtual) | -0.92 | +0.69 | +0.35 | -2.03 | -0.37 |
| 34748285006 | Neoverse-N2 | +0.45 | -0.46 | -0.30 | +0.62 | +0.11 |
| 34748285006 | AMD EPYC 7763 64-Core Processor | +0.22 | +0.03 | -0.60 | -0.17 | +0.40 |

The long x86 A/B ran on an Intel Xeon Platinum 8573C; the other x86 jobs used AMD EPYC 7763.
Their absolute times and A/A noise cannot be treated as the same host. macOS used a virtual Apple M1
without CPU affinity. Longer samples reduced A/A median dispersion, but large individual outliers
remain: long macOS A/A reached +17.71%; long A/B grapheme AB/BA medians disagree in sign.
ASCII gains occurred in all six pairs on every A/B host. Grapheme results are mixed;
the long Intel run was +0.72% slower in all six pairs. No universal neutrality claim is made.

Both A/B rounds observed `atlas_bytes` 488→14,824 and `scalar_cache_bytes` 48→14,384:
14 KiB additional inline Rust storage. These fields exclude heap allocations and RSS.

Each `run-*/text-artifacts.zip` preserves original text files byte for byte: complete reports,
12 raw logs per architecture, source-injection manifests, Cargo build messages/logs, fixture/runner/workflow
snapshots, CPU/kernel/compiler/font metadata and the original CI hash manifest.
`audit.json` records the first-pass source/archive/binary/log verification; GitHub run/artifact metadata
records job status, IDs and remote digests. `summary.json` retains unrounded separate-run summaries.

The compact record deliberately omits 18 executables and 36 source TARs. Their sizes and SHA-256 values
are declared in `manifest.json`; original CI artifacts had seven-day retention. Full archive manifests
and binary bytes were checked locally before packaging. `full-artifact-audit.py` preserves that auditor
and requires the original full downloads at its configured ROOT. Atlas pixel buffers and native font
files are not included: their fingerprints/hashes are recorded, rather than rehashed by this compact verifier.

Verify the checked-in package without extracting files or executing benchmark binaries:

```sh
python3 -B docs/glyph-cache-evidence/2026-09-13/verify.py
```

The verifier checks package/file hashes, raw logs, 720 timing records and paired medians.
See [the method guide](../../GLYPH_CACHE_MEASUREMENTS.md) for reproduction and timing limits.
