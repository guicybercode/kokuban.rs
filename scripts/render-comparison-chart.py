#!/usr/bin/env python3
"""Render a PTY throughput comparison as a static SVG.

Reads a ``summary.json`` produced by ``compare-terminal-performance.py`` (or its
audited copy) and writes a grouped bar chart (workload groups x terminals) using
only the standard library, so the figure can be regenerated anywhere without
matplotlib. Defaults render the audited 2026-09-13 Linux comparison; ``--summary``,
``--output``, ``--title`` and ``--subtitle`` render another run, such as macOS.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SUMMARY = (
    REPO_ROOT / "docs" / "linux-evidence" / "2026-10-31-four-terminals" / "summary.json"
)
DEFAULT_OUTPUT = REPO_ROOT / "docs" / "screenshots" / "throughput-comparison.svg"

# Known terminals in display order; a summary renders the subset it contains.
TERMINALS = ("kokuban", "ghostty", "alacritty", "kitty", "terminal")
TERMINAL_LABELS = {
    "kokuban": "Kokuban",
    "ghostty": "Ghostty",
    "alacritty": "Alacritty",
    "kitty": "Kitty",
    "terminal": "Terminal.app",
}
# Kokuban keeps the chalkboard green; the rivals use neutral greys of distinct
# lightness so the series stay apart under colour-vision deficiency. Every bar
# also carries a value label, so identity never depends on colour alone.
TERMINAL_COLORS = {
    "kokuban": "#1f3a2a",
    "ghostty": "#5b6670",
    "alacritty": "#8f99a3",
    "kitty": "#c5ccd3",
    "terminal": "#3a434c",
}
WORKLOADS = ("ascii", "ansi", "unicode", "short_lines")
WORKLOAD_LABELS = {
    "ascii": "ASCII",
    "ansi": "ANSI",
    "unicode": "Unicode",
    "short_lines": "Short lines",
}

TITLE = "PTY throughput, same Linux host (2026-10-31, medians of 5 runs)"
SUBTITLE_LINES = (
    "Kokuban 0.4.0 (27bbb15), Ghostty 1.3.0-dev tip, Alacritty 0.16.1, Kitty 0.45.0",
    "on GitHub Actions x86_64, Weston headless. Higher is better; not frame latency.",
)
SUBTITLE = " ".join(SUBTITLE_LINES)

BACKGROUND = "#fcfcfb"
INK = "#1f2328"
INK_MUTED = "#57606a"
GRID = "#e6e8eb"


def load_medians(summary_path: Path, terminals: tuple[str, ...] | None = None) -> dict[str, dict[str, float]]:
    """Return ``{terminal: {workload: median MiB/s}}`` from a summary.json file.

    Without ``terminals`` every known terminal present in the summary is used,
    in display order; a requested terminal that is missing raises ``KeyError``.
    """
    with summary_path.open(encoding="utf-8") as handle:
        summary = json.load(handle)
    summaries = summary["terminal_summaries"]
    selected = terminals if terminals is not None else tuple(t for t in TERMINALS if t in summaries)
    if not selected:
        raise ValueError(f"{summary_path} has no known terminal summaries")
    medians: dict[str, dict[str, float]] = {}
    for terminal in selected:
        workloads = summaries[terminal]["workloads"]
        medians[terminal] = {
            workload: float(workloads[workload]["mib_per_second"]["median"])
            for workload in WORKLOADS
        }
    return medians


def _nice_ceiling(value: float, step: float = 10.0) -> float:
    ceiling = step
    while ceiling < value:
        ceiling += step
    return ceiling


def render_svg(medians: dict[str, dict[str, float]], width: int = 880,
               title: str = TITLE, subtitle_lines: tuple[str, ...] = SUBTITLE_LINES) -> str:
    """Build the SVG document for the grouped bar chart."""
    terminals = tuple(medians)
    subtitle = " ".join(subtitle_lines)
    height = 520
    margin_left, margin_right = 72, 24
    margin_top, margin_bottom = 96, 92
    plot_w = width - margin_left - margin_right
    plot_h = height - margin_top - margin_bottom
    baseline = margin_top + plot_h

    max_value = max(v for per in medians.values() for v in per.values())
    y_max = _nice_ceiling(max_value * 1.12)

    def y_for(value: float) -> float:
        return baseline - (value / y_max) * plot_h

    group_w = plot_w / len(WORKLOADS)
    bar_gap = 2.0
    group_pad = group_w * 0.14
    bar_w = (group_w - 2 * group_pad - bar_gap * (len(terminals) - 1)) / len(terminals)

    parts: list[str] = []
    parts.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" '
        f'viewBox="0 0 {width} {height}" role="img" aria-labelledby="chart-title chart-desc" '
        f'font-family="-apple-system, BlinkMacSystemFont, Segoe UI, Helvetica, Arial, sans-serif">'
    )
    parts.append(f'<title id="chart-title">{title}</title>')
    parts.append(f'<desc id="chart-desc">{subtitle}</desc>')
    parts.append(f'<rect width="{width}" height="{height}" fill="{BACKGROUND}" rx="8"/>')
    parts.append(
        f'<text x="{margin_left}" y="34" font-size="18" font-weight="600" fill="{INK}">{title}</text>'
    )
    # Two short lines: one line at 12px would overflow the 880px canvas and be clipped.
    for index, line in enumerate(subtitle_lines):
        parts.append(
            f'<text x="{margin_left}" y="{54 + index * 16}" font-size="12" fill="{INK_MUTED}">{line}</text>'
        )

    # Grid and y axis.
    tick = 10.0 if y_max <= 100 else 20.0
    value = 0.0
    while value <= y_max + 1e-9:
        y = y_for(value)
        stroke = INK_MUTED if value == 0 else GRID
        parts.append(
            f'<line x1="{margin_left}" x2="{margin_left + plot_w}" y1="{y:.1f}" y2="{y:.1f}" '
            f'stroke="{stroke}" stroke-width="1"/>'
        )
        parts.append(
            f'<text x="{margin_left - 10}" y="{y + 4:.1f}" font-size="12" text-anchor="end" '
            f'fill="{INK_MUTED}">{value:.0f}</text>'
        )
        value += tick
    parts.append(
        f'<text transform="translate(22 {margin_top + plot_h / 2:.1f}) rotate(-90)" font-size="12" '
        f'text-anchor="middle" fill="{INK_MUTED}">MiB/s (median)</text>'
    )

    # Bars.
    for g_index, workload in enumerate(WORKLOADS):
        group_x = margin_left + g_index * group_w + group_pad
        for t_index, terminal in enumerate(terminals):
            v = medians[terminal][workload]
            x = group_x + t_index * (bar_w + bar_gap)
            top = y_for(v)
            h = baseline - top
            color = TERMINAL_COLORS[terminal]
            parts.append(
                f'<path d="M{x:.1f},{baseline:.1f} V{top + 4:.1f} a4,4 0 0 1 4,-4 '
                f'H{x + bar_w - 4:.1f} a4,4 0 0 1 4,4 V{baseline:.1f} Z" fill="{color}">'
                f"<title>{TERMINAL_LABELS[terminal]} - {WORKLOAD_LABELS[workload]}: {v:.3f} MiB/s</title></path>"
            )
            parts.append(
                f'<text x="{x + bar_w / 2:.1f}" y="{top - 6:.1f}" font-size="11" text-anchor="middle" '
                f'fill="{INK}">{v:.1f}</text>'
            )
        parts.append(
            f'<text x="{margin_left + g_index * group_w + group_w / 2:.1f}" y="{baseline + 22}" '
            f'font-size="13" text-anchor="middle" fill="{INK}">{WORKLOAD_LABELS[workload]}</text>'
        )

    # Legend.
    legend_y = height - 30
    legend_x = margin_left
    for terminal in terminals:
        label = TERMINAL_LABELS[terminal]
        parts.append(
            f'<rect x="{legend_x}" y="{legend_y - 10}" width="14" height="14" rx="3" '
            f'fill="{TERMINAL_COLORS[terminal]}" stroke="{INK_MUTED}" stroke-width="0.5"/>'
        )
        parts.append(
            f'<text x="{legend_x + 20}" y="{legend_y + 1}" font-size="12" fill="{INK}">{label}</text>'
        )
        legend_x += 20 + 8 * len(label) + 28

    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--summary", type=Path, default=DEFAULT_SUMMARY,
                        help="summary.json from compare-terminal-performance.py or an audited copy")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--width", type=int, default=880)
    parser.add_argument("--title", default=TITLE)
    parser.add_argument("--subtitle", action="append", metavar="LINE",
                        help="subtitle line; repeat for a second line (default: the Linux run's provenance)")
    parser.add_argument("--terminals", nargs="+", choices=TERMINALS,
                        help="terminals to draw, in this order (default: every known terminal in the summary)")
    args = parser.parse_args(argv)

    medians = load_medians(args.summary, tuple(args.terminals) if args.terminals else None)
    svg = render_svg(medians, width=args.width, title=args.title,
                     subtitle_lines=tuple(args.subtitle) if args.subtitle else SUBTITLE_LINES)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(svg, encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
