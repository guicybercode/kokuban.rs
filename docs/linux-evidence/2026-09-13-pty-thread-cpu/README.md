# PTY CPU by thread: 2026-09-13

[Run 34751544546](https://github.com/guicybercode/kokuban.rs/actions/runs/34751544546)
passed with harness `8c1762e58bec79e71d991d3d7bd7a49e57c3abb0`.
It compares `d54f801e9dec41a6eb4f159397e45939c367531d` against
`62c99adc6fc1dbf12d89ee717f5823644c162d47`, using five pairs (three AB,
two BA), 32 MiB requested per workload, primary screen, 10,000 history lines,
80×24 cells / 720×408 pixels, DejaVu Sans Mono 14, images disabled, and
one-second settling. Ubuntu 24.04 / AMD EPYC 9V74 / CPU 0, Rust 1.94.1;
Weston headless with pixman. Hardware, font selection, package versions,
commands, configuration and full job log are retained in `raw-evidence.zip`.

## Results

Positive change means longer elapsed time. CPU columns are medians of the
individual label samples, in milliseconds; elapsed change is the median of
the five paired ratios, not a ratio of medians.

| Workload | Paired elapsed change | After slower | Process CPU before→after | Reader CPU before→after | Main CPU before→after |
|---|---:|---:|---:|---:|---:|
| ASCII | −0.088% | 2/5 | 410→400 | 400→400 | 0→0 |
| ANSI | −3.182% | 0/5 | 360→350 | 360→350 | 0→0 |
| Unicode | +0.459% | 5/5 | 640→630 | 630→630 | 0→0 |
| Short lines | +0.098% | 3/5 | 850→850 | 850→840 | 0→0 |

Short-line pair changes were −2.917%, +0.236%, +1.383%, −0.424%,
+0.098%; AB median +0.098%, BA median −0.094%. Its earlier +9.127%
loss in [run 34749172188](https://github.com/guicybercode/kokuban.rs/actions/runs/34749172188)
did **not** recur. Both retained executable SHA-256 values match that earlier
run exactly, but the earlier CPU was EPYC 7763 and thread observation was off.
The earlier report is included separately as context; no samples are pooled.
This does not isolate hardware, scheduling or observer effects, nor identify
the cause of the earlier loss.

The reader accounts for most observed CPU. Main-thread deltas were zero or
one 10-ms tick in every sample; zero does not prove no work. No thread was
new, missing, unreadable or replaced in the 80 snapshots. All 320 raw stat
records and 160 deltas were independently reparsed/recomputed. Each sweep
took 0.177–0.577 ms outside the existing elapsed clock. Sequential thread
reads, process reads and write→DSR cover nearby, distinct intervals, with
10-ms counter quantization. Their totals need not agree. Observation can
still perturb scheduling. This is PTY processing/DSR evidence; the marker
reply does not validate workload text, displayed pixels, GPU or presentation.

## Verification and contents

Run `python3 -B docs/linux-evidence/2026-09-13-pty-thread-cpu/verify.py` from
the repository. It checks every retained file hash and recomputes payload
hashes, geometry/DSR/config checks, identities, raw thread counters, timing
relationships and paired summaries without launching Kokuban.

`audit.json` includes all 40 observations and per-pair/thread values. The full
download audit additionally hashed both ELF files and both source archives,
matched all 1,379/1,956 source files and modes to their Git blobs, and checked
182 Cargo compiler artifacts per build with `fresh=false`, distinct fresh
target directories and release profiles. Runtime differences are confined to
`src/glyph_atlas.rs` and `src/linux_window.rs`; Cargo files match.

The compact ZIP retains original reports, individual samples/cases, raw stat
text, logs, Cargo messages, source manifests and pinned harness snapshots.
Two executables and two source archives are omitted, with their sizes/hashes
in `manifest.json`; replay cannot rehash those absent bytes. To repeat that
check against the original unpacked download and repository history:

```sh
python3 -B docs/linux-evidence/2026-09-13-pty-thread-cpu/audit.py \
  /path/to/run-34751544546 --git-repository /path/to/kokuban.rs
```

GitHub's artifact ZIP digest is retained as API metadata: `gh run download`
unpacked the archive, so that container digest was not independently checked.
No build, measured executable, benchmark or CI dispatch ran during this audit.
