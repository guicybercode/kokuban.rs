# Absolute release-build resource observation — 2026-09-13

[Run 34748583285](https://github.com/guicybercode/kokuban.rs/actions/runs/34748583285) completed on source `72dd68a1262d360d119fb9459ea12a47303bae5d`. This is one absolute observation of a development revision built with `cargo build --release --locked`. **Release describes the Rust build profile, not a new published release or the v0.2 binary. There is no A/B baseline and no cache-attributed performance gain.**

The runner used Ubuntu 24.04, kernel `6.17.0-1022-azure`, AMD EPYC 7763, four allowed logical CPUs, Rust 1.94.1 and Xvfb. The terminal was 80×24 cells / 720×408 pixels, DejaVu Sans Mono at 14 px, with 10,000 history lines configured. CPU was not pinned to one core.

The retained [independent audit](run-34748583285/release-resource-audit.json) reports PASS after checking recorded phases, process identities, output, the decoded video fixture and the three screenshots. This preservation step only checked existing file bytes/hashes and PNG dimensions; it did not rerun the benchmark or video decoder.

## Process resources

Values below are from the audit's recomputed phases. CPU seconds are changes in Kokuban's user+system process ticks; 100% means one logical core. RSS/HWM values are KiB.

| Phase | Wall seconds | CPU seconds | CPU % of one core | Final sampled RSS | Lifetime HWM |
|---|---:|---:|---:|---:|---:|
| Startup settle, after ready marker | 1.0004 | 0.00 | 0.00 | 15,068 | 24,328 |
| Idle | 5.0004 | 0.00 | 0.00 | 15,068 | 24,328 |
| Output sampling window | 0.1012 | 0.10 | 98.78 | 46,704 | 46,704 |
| Output settle | 1.0004 | 0.00 | 0.00 | 46,704 | 46,704 |
| Post-output idle | 5.0003 | 0.00 | 0.00 | 46,704 | 46,704 |

The finite ASCII payload was 2,621,440 bytes / 32,768 lines. PTY writing plus the final terminal DSR response took **78.49 ms**, whereas the separate resource sampling window lasted **101.24 ms**. Do not divide the CPU interval by the shorter PTY interval. The ready/settle phase is not a measurement of application startup latency.

CPU has 100 Hz (10 ms) tick resolution; zero observed ticks does not prove zero work. These measurements exclude children, the display server and observer. RSS is sampled rather than a continuous maximum, while `VmHWM` covers the process lifetime. The output can retain the configured scrollback; the post-output RSS is not evidence of a leak or a required return to startup RSS. DSR confirms terminal processing and response, not presentation of every output line.

## Video observation

The retained [FFV1/Matroska fixture](run-34748583285/linux-release-resource-measurements/video/fixture.mkv) contains 72 frames, 320×180 pixels, at 12 fps. mpv 0.37.0 sends Kitty direct escapes; shared-memory and file transfers are disabled. Playback paused at frame 18, resumed, and reached frame 71; both processes exited successfully.

| Active phase | Wall seconds | Kokuban CPU seconds / one-core % | mpv CPU seconds / one-core % |
|---|---:|---:|---:|
| Before pause | 1.5248 | 0.13 / 8.53% | 0.07 / 4.59% |
| After resume | 4.5333 | 0.39 / 8.60% | 0.21 / 4.63% |

Observed RSS/HWM peaks were **32,260 KiB for Kokuban** and **62,588 KiB for mpv**, in this separate video process run. The report records 242 active captures with all 72 distinct frame IDs and zero invalid active captures. Its 71 observed transitions give a **sampling lower bound of 11.72 Hz**, not a source-FPS measurement or a physical-display guarantee. Capture intervals were about 25 ms and capture subprocesses add overhead. This tiny, local, silent fixture does not characterize audio, SSH or general video workloads.

The three retained 720×408 PNGs are [frame 0](run-34748583285/linux-release-resource-measurements/video/frame-00.png), [paused frame 18](run-34748583285/linux-release-resource-measurements/video/paused.png) and [frame 71](run-34748583285/linux-release-resource-measurements/video/frame-71.png). Their video-region RGB bytes matched the fixture in the original audit. Other capture classifications remain as report records, not retained images.

## Preserved bytes and omissions

All **40 files / 601,895 original bytes** from the local audit snapshot are preserved losslessly under `run-34748583285/`: raw resource/video reports and logs, process markers, configs, Cargo inputs/tree, compiler/package metadata, GitHub metadata/log, exact workflow/smoke sources, audit source/result, fixture and all three PNGs. Empty logs and synchronization markers are intentional. [manifest.json](manifest.json) records each original path, encoding, original/stored byte counts and SHA-256 hashes. Only `github-run.log` is XZ-compressed, preserving its original whitespace; all other files are byte-for-byte copies.

The executable was **not retained** by the original workflow. Its reported size is 10,614,800 bytes and reported SHA-256 is `973afccb2061b80a591f2b364335d14900fb98a605a154d7973d7f89c032331f`; the audit could not independently rehash it. Video reused the same release path in a later step without a separate binary hash. The source checkout, dependency/font binaries and original GitHub ZIP bytes are also absent. Cargo inputs, exact revision, workflow source and reported ELF/dependency information remain available. The GitHub ZIP digest is API metadata, not an independently verified local ZIP hash.

This is a shared-CI-host observation with no CPU/RSS/throughput pass thresholds and no causal comparison against earlier resource runs. Original GitHub artifacts expire after seven days; the files committed here remain independently verifiable. The original audit script is retained for provenance and is not a portable package verifier.

From this directory, verify every stored file plus the README and manifest without builds, video decoding or network access:

```sh
shasum -a 256 -c SHA256SUMS
```

All entries should report `OK`. Read the preserved CI log with `xz -dc run-34748583285/github-run.log.xz`; its decoded hash is recorded in the manifest. The checksum list excludes itself; its exact bytes are versioned by Git.
