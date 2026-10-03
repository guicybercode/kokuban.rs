# Compact history: audited Linux x86 measurements

Before: `24782c344714727be4f8981cd0d1d10f047aa9f4`. After and harness: `f0040ad66cbb49d76b387bbe0539c014bc801d28`.

Both runs passed the independent audit of all 87 original files: exact Cargo manifests/locks, three harness hashes against Git, workflow and CI root compilations with separate targets, distinct recorded binary hashes, five alternating pairs, four 32 MiB requested loads, 300 protocol RTT observations, identical configuration within each profile and observed 80×24 cells / 720×408 pixels. Payload bytes/hashes and all throughput arithmetic were recomputed. Raw artifact files were retained byte for byte.

The dispatch used the immutable abbreviation `f0040ad`; the workflow resolved it before archiving. Artifact revisions, report refs and CI head record the full SHA. The adapted auditor verifies local unique resolution and the workflow/log resolution command. It changes no original artifacts.

Both runs used AMD EPYC 7763, observed harness CPU affinity [0], Ubuntu 24.04, native Wayland, Weston headless Pixman and DejaVu Sans Mono 14 px. They are separate runs: compare before/after within each.

## primary: run 34743854765

| Load | Before MiB/s | After MiB/s | Change | Slower pairs | Ranges overlap | CPU s before→after | RSS KiB after load before→after |
|---|---:|---:|---:|---:|---|---|---|
| ascii | 57.161 | 56.091 | -1.87% | 2/5 | True | 0.51→0.51 | 47764→47488 |
| ansi | 44.517 | 61.380 | +37.88% | 0/5 | False | 0.66→0.48 | 47764→47488 |
| unicode | 30.814 | 33.464 | +8.60% | 0/5 | True | 0.98→0.90 | 69944→68664 |
| short_lines | 6.532 | 23.560 | +260.72% | 0/5 | False | 4.80→1.30 | 72244→69524 |

Observed throughput ranges (before → after):
- ascii: 53.153–57.440 → 55.003–58.930 MiB/s.
- ansi: 44.034–46.318 → 59.292–62.055 MiB/s.
- unicode: 30.147–31.684 → 31.656–34.073 MiB/s.
- short_lines: 6.296–6.763 → 23.294–23.928 MiB/s.

Protocol RTT median: 56.427→55.620 µs. Full samples, paired deltas, CPU/RSS ranges and recorded executable hashes are in `summary.json`.

## alternate: run 34743855262

| Load | Before MiB/s | After MiB/s | Change | Slower pairs | Ranges overlap | CPU s before→after | RSS KiB after load before→after |
|---|---:|---:|---:|---:|---|---|---|
| ascii | 80.389 | 80.375 | -0.02% | 3/5 | True | 0.35→0.36 | 15816→15784 |
| ansi | 68.737 | 67.417 | -1.92% | 4/5 | True | 0.42→0.43 | 15816→15784 |
| unicode | 38.537 | 39.160 | +1.61% | 2/5 | True | 0.77→0.77 | 37028→36968 |
| short_lines | 34.473 | 33.906 | -1.64% | 5/5 | True | 0.88→0.90 | 37028→39804 |

Observed throughput ranges (before → after):
- ascii: 80.106–80.842 → 78.015–81.180 MiB/s.
- ansi: 67.764–69.568 → 66.324–68.124 MiB/s.
- unicode: 38.250–39.564 → 38.784–39.595 MiB/s.
- short_lines: 34.052–34.610 → 33.594–34.144 MiB/s.

Protocol RTT median: 56.420→58.404 µs. Full samples, paired deltas, CPU/RSS ranges and recorded executable hashes are in `summary.json`.

## Interpretation and limits

Primary history throughput improved substantially for short lines and ANSI. Unicode improved in all five pairs, though its before/after overall ranges narrowly overlap. Primary ASCII has a lower after median, with two slower pairs and overlapping ranges. Alternate is mixed: short lines are slower in all five pairs and ANSI in four; these regressions must be retained in the integration decision. Overlapping ranges do not establish noise or equivalence.

RSS is a process snapshot at each load boundary, after previous loads and warmups in the same process; it is not isolated history allocation or peak memory. CPU counters have coarse granularity. The strong primary throughput/CPU gains coexist with smaller alternate regressions; no blanket improvement claim is justified.

Correctness CI 34743769372 was independently queried: full candidate head, completed/success and successful macOS/Linux jobs. Its metadata is retained separately; paired workflows themselves test the Python harness and build production code.

Every one of the 20 successful terminal processes logged the 100 ms XDG Settings Portal color-scheme timeout; diagnostics and complete logs are retained. No other terminal failure is inferred from that warning.

Only PTY processing plus DSR is measured. This does not verify frame presentation, input-to-photon latency, workload text/pixel fidelity, physical Omarchy/Hyprland or superiority over competing terminals. Original executable bytes and artifact ZIP/API digests were not supplied: executable hashes are mutually consistent CI/report records, not independently rehashed binaries.

The auditor and manifests retain exact provenance and hashes; no source code, repository files, Git state or raw artifacts were modified by this audit.
