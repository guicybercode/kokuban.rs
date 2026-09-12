# System font regression — 2026-09-12

The same functional check fails with the old default at `4799708` and passes
with the Linux font fix at `ccbbb6e`. This is Linux aarch64 release-binary
evidence under X11/Xvfb, with 40×16 cells, font size 14 and scale 1.

| Requested family | Before | After |
| --- | --- | --- |
| Omitted | DejaVu Sans, 13×17 | DejaVu Sans Mono, 9×17 |
| DejaVu Sans Mono | DejaVu Sans Mono, 9×17 | DejaVu Sans Mono, 9×17 |
| DejaVu Sans | DejaVu Sans, 13×17 | DejaVu Sans, 13×17 |
| Menlo | DejaVu Sans, 13×17 | DejaVu Sans, 13×17 |

A private Fontconfig directory contains only the two installed DejaVu fonts.
A scan alias makes **Menlo genuinely selectable as DejaVu Sans**, while
`monospace` resolves to DejaVu Sans Mono. Thus the old default cannot pass
through a missing-font fallback. The new default's entire RGB frame equals
the monospace control; explicit families still win. Both explicit controls
also produce identical pixels across revisions.

[Raw evidence](font-evidence.tar.xz) retains eight initial XWD frames, their
terminal logs, the expected before-failure traceback, environment and binary
hashes, and both release-build logs. The [after report](after-report.json)
is unchanged from the harness. The [independent pixel audit](pixel-audit.json)
checks geometry and RGB hashes against that report and the before traceback;
the [manifest](manifest.json) pins sources, harness and retained bytes.
No font launch logs a fallback; existing font-kit `invalid platform ID`
warnings remain visible in the raw logs.

These local captures use the harness at `ccbbb6e`. Its first CI attempt failed
because the fixture could encounter provisional window geometry. The harness
at `1c90e0c` waits for font metrics, PTY dimensions and the X11 frame to agree
before printing. The [subsequent CI run](https://github.com/guicybercode/kokuban.rs/actions/runs/34722785959)
passed both Linux and macOS jobs, including the font and theme smoke on Linux.
The retained local frames are from the earlier harness, not that CI run.

Run `xvfb-run -a python3 scripts/linux-theme-smoke.py target/release/kokuban`
with Fontconfig, DejaVu Sans and DejaVu Sans Mono installed to exercise the
check. This validates selection when opening a window; it does not establish
live font replacement or a physical Omarchy/Hyprland session.
