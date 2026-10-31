# Performance

![PTY throughput comparison](screenshots/throughput-comparison.svg)

The chart is generated from the audited [four-terminal summary](linux-evidence/2026-09-13-four-terminals/summary.json) by `scripts/render-comparison-chart.py`. It shows PTY processing throughput on one Linux host, not frame latency. Methodology, harness and limits: [TERMINAL_COMPARISON.md](TERMINAL_COMPARISON.md) (same-host comparison) and [LINUX_PERFORMANCE.md](LINUX_PERFORMANCE.md) (Linux performance history).

## Scope

The measurements below were recorded on **September 13, 2026**, against the
development commits identified in each experiment. They document the work leading
to v0.3; they are **not fresh measurements of the published v0.3 binaries**.
Results depend on the workload, fonts, display backend and hardware. The linked
reports retain raw samples, build inputs, revisions and known regressions.

### Linux CPU and memory

One [release-build resource observation](linux-evidence/2026-09-13-release-resources/README.md)
measured `72dd68a` on Ubuntu 24.04, AMD EPYC 7763, Rust 1.94.1 and Xvfb. Kokuban
used an 80×24-cell / 720×408-pixel window, DejaVu Sans Mono 14 px and a configured
10,000-line history. The process could use four logical CPUs and was not pinned.

| Phase | Observation window | Kokuban CPU time | CPU, one core = 100% | Final sampled RSS |
| --- | ---: | ---: | ---: | ---: |
| Idle, before output | 5.0004 s | 0.00 s | 0.00% observed | 15,068 KiB (14.7 MiB) |
| Finite ASCII output | 0.1012 s | 0.10 s | 98.78% | 46,704 KiB (45.6 MiB) |
| Idle, after output | 5.0003 s | 0.00 s | 0.00% observed | 46,704 KiB (45.6 MiB) |

The output contained 2.5 MiB / 32,768 lines; writing it through the PTY and
receiving the final terminal response took **78.49 ms**. That interval differs
from the CPU sampling window above. CPU counters have 10 ms resolution, so zero
observed ticks do not prove zero work. RSS is a sample of resident process memory;
it excludes the shell, display server and observer, and retained scrollback can
keep it above the initial value. This single run has no before/after baseline.

In a separate process run at the same revision, a silent **320×180, 12 fps,
72-frame** clip sent by mpv through Kitty escapes used **8.53–8.60% of one core**
in Kokuban during the two active playback phases, with an observed RSS peak of
**32,260 KiB (31.5 MiB)**. mpv separately used 4.59–4.63% and peaked at 62,588 KiB.
All 72 frame IDs were observed, including pause/resume. The
[video evidence](linux-evidence/2026-09-13-release-resources/README.md#video-observation)
describes a small local fixture, not sustained playback, audio or physical-display
frame timing.

### Terminal output throughput

The [same-host comparison](linux-evidence/2026-09-13-four-terminals/README.md)
measured Kokuban `d54f801`, Ghostty **1.3.0-dev tip**, Alacritty 0.16.1 and Kitty
0.45.0. Values are medians in **MiB/s** from five processes per terminal, with
terminal order rotated between rounds and approximately 32 MiB per workload.

| Workload | Kokuban | Ghostty tip | Alacritty | Kitty |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 70.015 | 28.333 | 50.854 | 78.800 |
| ANSI colors/control sequences | 55.677 | 22.088 | 53.846 | 16.747 |
| Unicode | 36.853 | 26.646 | 51.779 | 57.365 |
| Short lines | 36.634 | 24.340 | 50.246 | 28.917 |

The native x86_64 EPYC 7763 run used an Ubuntu 26.04 container on an Ubuntu 24.04
runner, Weston 14 headless/Pixman, matched 80×24-cell / 720×408-pixel windows and
the alternate screen with no history. Software OpenGL was requested; each
terminal's effective GL renderer was not recorded. Kokuban led ANSI in this run;
Kitty led ASCII and Unicode, and Alacritty led short lines. These results measure
PTY processing through a final DSR (device-status report) reply. They do not
measure visible frame presentation or input-to-photon latency, and do not establish
a general ranking on desktop GPUs or Omarchy/Hyprland hardware.

### Measured optimization tradeoffs

- **Compact scrollback** (`24782c3` → `f0040ad`): five alternating before/after
  pairs on Linux x86_64 raised primary-screen short-line throughput from
  **6.532 to 23.560 MiB/s (+260.72%)** and reduced its median process CPU time
  from **4.80 to 1.30 s**. ANSI improved 37.88% and Unicode 8.60%, while primary
  ASCII fell 1.87%. Alternate-screen ANSI fell 1.92% and short lines fell 1.64%;
  the latter were slower in all five pairs. See the
  [full history results](linux-evidence/2026-09-13-compact-history/README.md).
- **Cached glyph color classification** (`24782c3` → `965843d`): incremental
  single-row CPU painting fell from **364.252 to 249.323 µs (−31.55%)** for ASCII
  after loading emoji, and from **306.363 to 217.787 µs (−28.91%)** for Unicode.
  Five pairs used 120×40 cells on Linux x86_64; 12 before/after frame pairs matched
  byte for byte. A full-screen repaint of pure ASCII was 0.31% slower, and each
  glyph entry grew from 24 to 28 bytes. Timing excludes snapshots, initial glyph
  rasterization, PTY and presentation. See the
  [renderer measurements](linux-evidence/2026-09-13-glyph-color/summary.md).
- **Warmed scalar glyph cache** (`b6cd9ad` → `ddccc9b`): in the longer round
  (10,000 iterations, 1,000 warmups, six alternating process pairs), median
  paired ASCII lookup time fell **79.29% on virtual Apple M1**, **54.92% on
  Neoverse-N2**, and **60.38% on Intel Xeon Platinum 8573C**. Grapheme lookup time
  increased 4.46%, 0.26% and 0.72%, respectively; the Intel increase occurred in
  all six pairs. The cache adds **14 KiB of inline Rust storage**, excluding
  heap allocations and RSS. These are cached-lookup microbenchmarks, not
  end-to-end terminal or rendering speedups. The
  [cache evidence and same-binary controls](glyph-cache-evidence/2026-09-13/README.md)
  retain each host/run separately; the x86 control ran on a different CPU.

Read [Linux performance methodology and history](LINUX_PERFORMANCE.md) and
the [glyph-cache measurement guide](GLYPH_CACHE_MEASUREMENTS.md) for full
sample ranges, reproduction instructions and timing limits. Results from different
revisions, hosts or display backends should not be combined into one speedup.

