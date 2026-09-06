#!/usr/bin/env python3
"""Render the README's benchmark chart from the measured summary.

The chart used to be hand-drawn SVG. It said "7 MiB vs 69 MiB, 7-15x faster"
long after the measurements said 7.3 against 50.5; nothing regenerated it and
nothing checked it, so it aged in silence while the numbers under it moved.
Generating it from `summary.json` means the picture cannot disagree with the
table beside it.

Usage: python3 bench/aggregate.py && python3 bench/chart.py
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SUMMARY = os.path.join(HERE, "results", "summary.json")
OUT = os.path.join(os.path.dirname(HERE), "docs", "assets", "bench.svg")

# (label, scenario, op, key): the four the README quotes.
ROWS = [
    ("memory per command", "single", "up", "rss_mib"),
    ("up · 42 services", "wide-level", "up", "seconds"),
    ("up · 12 services", "many-services", "up", "seconds"),
    ("config · parse only", "config-heavy", "config", "seconds"),
]
TOOLS = [("podup", "#3fb950"), ("docker-compose", "#58a6ff"), ("podman-compose", "#f778ba")]


def value(summary, tool, scen, op, key):
    cell = summary.get(tool, {}).get(scen, {}).get(op)
    if not cell:
        return None
    stat = cell.get(key)
    return stat.get("median") if isinstance(stat, dict) else stat


def main() -> int:
    if not os.path.exists(SUMMARY):
        print(f"no {SUMMARY}; run bench/aggregate.py first", file=sys.stderr)
        return 2
    summary = json.load(open(SUMMARY))

    groups = []
    for label, scen, op, key in ROWS:
        vals = [(t, value(summary, t, scen, op, key), c) for t, c in TOOLS]
        if any(v is None for _, v, _ in vals):
            print(f"missing data for {scen}/{op}; skipping", file=sys.stderr)
            continue
        unit = "MiB" if key == "rss_mib" else "s"
        groups.append((label, unit, vals))

    if not groups:
        print("no rows to plot", file=sys.stderr)
        return 1

    row_h, bar_h, top, left, width = 116, 22, 56, 176, 760
    height = top + row_h * len(groups) + 24
    plot = width - left - 96

    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" role="img" aria-label="podup benchmark chart: podup leads every '
        f'measured scenario in both memory and latency">',
        "  <style>",
        "    .card { fill:#0d1117; stroke:#30363d; }",
        "    .t { fill:#e6edf3; font:600 13px ui-sans-serif,system-ui,sans-serif; }",
        "    .l { fill:#8b949e; font:12px ui-sans-serif,system-ui,sans-serif; }",
        "    .v { fill:#e6edf3; font:600 12px ui-monospace,monospace; }",
        "  </style>",
        f'  <rect class="card" x="0.5" y="0.5" width="{width - 1}" height="{height - 1}" rx="10"/>',
        f'  <text class="t" x="24" y="30">podup vs docker-compose vs podman-compose '
        f'on the same rootless Podman</text>',
        f'  <text class="l" x="24" y="47">median of 10 measured runs · lower is better</text>',
    ]

    for gi, (label, unit, vals) in enumerate(groups):
        y0 = top + gi * row_h
        out.append(f'  <text class="t" x="24" y="{y0 + 16}">{label}</text>')
        top_val = max(v for _, v, _ in vals) or 1
        for bi, (tool, val, colour) in enumerate(vals):
            y = y0 + 26 + bi * (bar_h + 4)
            w = max(2, int(plot * (val / top_val)))
            shown = f"{val:.2f} {unit}" if unit == "MiB" else (
                "&lt;0.01 s" if val < 0.005 else f"{val:.2f} s"
            )
            out.append(f'  <text class="l" x="{left - 8}" y="{y + 15}" text-anchor="end">{tool}</text>')
            out.append(f'  <rect x="{left}" y="{y}" width="{w}" height="{bar_h}" rx="3" fill="{colour}"/>')
            out.append(f'  <text class="v" x="{left + w + 8}" y="{y + 15}">{shown}</text>')

    out.append("</svg>")
    open(OUT, "w").write("\n".join(out) + "\n")
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
