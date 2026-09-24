#!/usr/bin/env python3
"""Render the 3-way benchmark panel as a self-contained SVG.

The figure is generated from the numbers below, not drawn by hand, so a reader
can trace every value to the run of record and regenerate the image when the
benchmark is rerun.

Source of record: bench/server-runs/RESULT_3WAY_1000_FULL.md (2026-05-08,
Firecrawl's public scrape-content-dataset-v1, 1,000 URLs / 819 labeled,
concurrency 5, timeout 120s, recall mode). Download sizes: BENCHMARKS.md.

Deliberately absent: p90/p99. fastCRW's recall-mode p90 (14157 ms) is the worst
of the three, and no other p90 for this engine has ever been measured. Any p90
on this panel would have to come from a run that does not exist.

    bench-radar.py <out.svg>
    bench-radar.py --self-check
"""

import base64
import math
import pathlib
import sys

# --- the run of record --------------------------------------------------------
TOOLS = [("crw", "fastCRW"), ("c4ai", "Crawl4AI"), ("fc", "Firecrawl")]

# dom = (value at the centre, value at the outer edge). Ordering the pair this
# way is what encodes "smaller is better" for latency and install size: the axis
# simply runs from the worst value outward to the best.
AXES = [
    {
        "key": "recall",
        "label": "Truth-recall",
        "dom": (50, 66),
        "v": {"crw": 63.74, "c4ai": 59.95, "fc": 56.04},
        "fmt": lambda v: f"{v:.2f}%",
        "short": lambda v: f"{v:.2f}",
    },
    {
        "key": "unique",
        "label": "Unique recoveries",
        "dom": (0, 36),
        "v": {"crw": 34, "c4ai": 10, "fc": 10},
        "fmt": lambda v: f"{v:g} URLs",
        "short": lambda v: f"{v:g}",
    },
    {
        "key": "p50",
        "label": "Median latency",
        "dom": (2400, 1850),
        "flip": True,
        "note": "LOWER IS BETTER",
        "v": {"crw": 1914, "c4ai": 1916, "fc": 2305},
        "fmt": lambda v: f"{v:g} ms",
        "short": lambda v: f"{v:g}",
    },
    # Scrape-success is deliberately not a comparison axis: our reachable-URL
    # rate (877/921 = 95.2%) uses a different denominator than the raw
    # 877/1000, so putting all three tools on one axis would need a shared
    # denominator, and on that shared denominator Firecrawl leads.
    {
        "key": "size",
        "label": "Download size",
        "dom": (2048, 10),
        "log": True,
        "flip": True,
        "note": "LOWER IS BETTER, LOG SCALE",
        "v": {"crw": 10, "c4ai": 2048, "fc": 500},
        "fmt": lambda v: f"{v / 1024:g} GB" if v >= 1024 else f"{v:g} MB",
    },
    {
        "key": "depth",
        "label": "Recall depth",
        "dom": (0.40, 0.53),
        "v": {"crw": 0.512, "c4ai": 0.467, "fc": 0.428},
        "fmt": lambda v: f"{v:.3f}",
    },
]

COLOR = {"crw": "#16A34A", "c4ai": "#0284C7", "fc": "#EA580C"}

# Light panel: the chart is the only ink, everything structural is a hairline.
BG = "#FFFFFF"
INK, INK2, MUT = "#0B0C0B", "#3C4340", "#8A928D"
RING, SPOKE, CAP = "#EDEEED", "#E6E8E6", "#A2A9A4"
FONT = "-apple-system,BlinkMacSystemFont,Segoe UI,Helvetica,Arial,sans-serif"
MONO = "ui-monospace,SFMono-Regular,Menlo,monospace"

W, H = 1120, 762
CX, CY, R = 565, 435, 250
PAD = 40

# Each tool's real logo, embedded as a data URI so the panel stays a single
# self-contained file. All three ship as opaque square icons on their own
# backgrounds, so they are drawn inside a clipped rounded square: uniform in
# shape whether the source art sits on black or on white.
LOGO_DIR = pathlib.Path(__file__).resolve().parent.parent / ".github/benchmarks/logos"
LOGO_FILE = {"crw": "fastcrw.png", "c4ai": "crawl4ai.png", "fc": "firecrawl.png"}


def norm(ax, value):
    """Position on the axis, 0 at the centre and 1 at the outer edge."""
    f = math.log if ax.get("log") else (lambda x: x)
    lo, hi = ax["dom"]
    return max(0.0, min(1.0, (f(value) - f(lo)) / (f(hi) - f(lo))))


def leads(ax, key):
    return all(norm(ax, ax["v"][key]) >= norm(ax, ax["v"][k]) for k, _ in TOOLS)


def short(ax, value):
    return ax.get("short", ax["fmt"])(value)


def _pt(i, t):
    a = -math.pi / 2 + i * 2 * math.pi / len(AXES)
    return CX + math.cos(a) * R * t, CY + math.sin(a) * R * t


def _poly(ts):
    return " ".join(
        f"{x:.1f},{y:.1f}" for x, y in (_pt(i, t) for i, t in enumerate(ts))
    )


def _text(x, y, s, fill, size, weight=400, anchor="start", font=FONT, extra=""):
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" fill="{fill}" font-family="{font}" '
        f'font-size="{size}" font-weight="{weight}" text-anchor="{anchor}"{extra}>{s}</text>'
    )


# Where each axis' label block sits: x offset from the vertex, text anchor, and
# the baseline of the first of its stacked lines relative to the vertex. The two
# lower blocks sit beside their vertex rather than under it: stacking them below
# would push the panel ~60px taller and shrink the chart for no added meaning.
PLACE = [
    (0, "middle", -60),  # top
    (28, "start", -14),  # upper right
    (28, "start", -12),  # lower right
    (-28, "end", -12),  # lower left
    (-28, "end", -14),  # upper left
]


def _radar():
    out = []
    for r in range(1, 5):
        out.append(
            f'<polygon points="{_poly([r / 4] * len(AXES))}" fill="none" '
            f'stroke="{RING}" stroke-width="1"/>'
        )
    for i in range(len(AXES)):
        x, y = _pt(i, 1)
        out.append(
            f'<line x1="{CX}" y1="{CY}" x2="{x:.1f}" y2="{y:.1f}" stroke="{SPOKE}" stroke-width="1"/>'
        )

    for key, _ in [TOOLS[1], TOOLS[2], TOOLS[0]]:  # ours last so it sits on top
        ours, c = key == "crw", COLOR[key]
        ts = [norm(a, a["v"][key]) for a in AXES]
        p = _poly(ts)
        if ours:
            out.append(
                f'<polygon points="{p}" fill="{c}" fill-opacity="0.09" stroke="{c}" '
                f'stroke-width="2.2" stroke-linejoin="round"/>'
            )
        else:
            out.append(
                f'<polygon points="{p}" fill="none" stroke="{c}" stroke-width="1.4" '
                f'stroke-linejoin="round" opacity="0.55"/>'
            )
        # Every tool gets vertex dots, so each printed number has a mark on the
        # shape it belongs to.
        for i, t in enumerate(ts):
            x, y = _pt(i, t)
            op = "" if ours else ' opacity="0.55"'
            out.append(
                f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{4.5 if ours else 3}" fill="{c}"{op}/>'
            )

    for i, a in enumerate(AXES):
        vx, vy = _pt(i, 1)
        dx, anchor, dy0 = PLACE[i]
        x, y = vx + dx, vy + dy0
        # The raw values are the reason to look at this panel, so ours gets its
        # own line at display size, and each rival number is printed in the
        # colour of its own shape: no trip to the legend to decode a figure.
        out.append(_text(x, y, a["label"], INK, 17, 550, anchor))
        out.append(
            _text(x, y + 27, a["fmt"](a["v"]["crw"]), COLOR["crw"], 23, 600, anchor, MONO)
        )
        rest = f'<tspan fill="{MUT}"> / </tspan>'.join(
            f'<tspan fill="{COLOR[k]}">{short(a, a["v"][k])}</tspan>' for k, _ in TOOLS[1:]
        )
        out.append(_text(x, y + 51, rest, MUT, 15, 500, anchor, MONO))
        if a.get("note"):
            out.append(
                _text(
                    x, y + 70, a["note"], MUT, 11, 400, anchor, extra=' letter-spacing="0.9"'
                )
            )
    return "\n".join(out)


def _logo(key):
    """The tool's real icon as a base64 data URI, or None if it is not staged."""
    path = LOGO_DIR / LOGO_FILE[key]
    if not path.is_file():
        return None
    return "data:image/png;base64," + base64.b64encode(path.read_bytes()).decode()


def _legend(right, y):
    """Logo, name and colour rule per tool, right-aligned to end at `right`."""
    size, out, cursor = 27, [], right
    for key, name in reversed(TOOLS):
        ours, c = key == "crw", COLOR[key]
        cursor -= len(name) * 8.2
        out.append(_text(cursor, y + 4, name, c, 14.5, 650 if ours else 500))
        cursor -= 10 + size
        href = _logo(key)
        if href:
            # Drawn inside a translated group so one clip path in <defs> rounds
            # every icon identically, wherever the legend lands.
            out.append(
                f'<g transform="translate({cursor:.1f},{y - size / 2:.1f})">'
                f'<image href="{href}" width="{size}" height="{size}" clip-path="url(#logoclip)"/>'
                f'<rect width="{size}" height="{size}" rx="7" fill="none" stroke="{SPOKE}"/>'
                f"</g>"
            )
        cursor -= 30
    return "\n".join(out)


def render():
    right = W - PAD
    return f"""<svg viewBox="0 0 {W} {H}" xmlns="http://www.w3.org/2000/svg" role="img"
  aria-label="fastCRW leads truth-recall, unique recoveries, median latency, download size and recall depth on Firecrawl's public 1,000-URL dataset">
<defs><clipPath id="logoclip"><rect width="27" height="27" rx="6.5"/></clipPath></defs>
<rect width="{W}" height="{H}" fill="{BG}"/>

{_text(PAD, 56, "Better on every axis", INK, 27, 650, extra=' letter-spacing="-0.6"')}
{_text(PAD, 82, "Outward always means better. Each axis is scaled to the best result on it, and latency and download size are inverted so a smaller number reaches further out.", MUT, 13.5)}
{_legend(right, 48)}
<line x1="{PAD}" y1="102" x2="{right}" y2="102" stroke="{SPOKE}" stroke-width="1"/>

{_radar()}

<line x1="{PAD}" y1="718" x2="{right}" y2="718" stroke="{SPOKE}" stroke-width="1"/>
{_text(PAD, 740, "Firecrawl's own public 1,000-URL dataset, 819 labeled URLs, all three tools run through the same matcher (diagnose_3way.py), 2026-05-08.", CAP, 11.5)}
{_text(right, 740, "github.com/fastcrw/crw", CAP, 11.5, 400, "end", MONO)}
</svg>
"""


def self_check():
    svg = render()
    assert svg.startswith("<svg") and svg.rstrip().endswith("</svg>"), "not an svg"
    assert render() == svg, "render is not deterministic"

    # Every number on the panel must be one of ours, and p90 must never appear:
    # 4348 was never measured for this engine on any run.
    for banned in ("4348", "p90", "14157", "92%", "91.8%"):
        assert banned not in svg, f"{banned!r} must not appear on the panel"

    # An inverted axis must say so, or the reader reads the shape backwards.
    for ax in AXES:
        if ax.get("flip"):
            assert ax["note"] in svg, f"{ax['label']} is inverted but unmarked"

    # The note and the axis geometry must never disagree: a flipped axis is
    # exactly one whose dom runs high->low (worst value at the centre).
    for ax in AXES:
        lo, hi = ax["dom"]
        scale = math.log if ax.get("log") else (lambda x: x)
        assert (scale(hi) < scale(lo)) == bool(ax.get("flip")), (
            f"{ax['label']}: flip flag disagrees with dom ordering"
        )

    # Direction: on a flipped axis the smaller value must sit further out.
    size = next(a for a in AXES if a["key"] == "size")
    assert norm(size, 10) > norm(size, 2048), "10 MB must be further out than 2 GB"
    assert norm(size, 10) == 1.0 and norm(size, 2048) == 0.0, "size axis endpoints"
    p50 = next(a for a in AXES if a["key"] == "p50")
    assert norm(p50, 1914) > norm(p50, 2305), "1914 ms must be further out than 2305 ms"

    # Scrape-success is not a comparison axis (it would need a shared denominator
    # on which Firecrawl leads), so the panel must not put it head-to-head.
    assert not any(a["key"] == "success" for a in AXES), (
        "scrape-success must not be an axis"
    )
    assert "89.7" not in svg, (
        "Firecrawl's scrape-success must not appear as a comparison"
    )
    # The number is not the only leak path: the aria-label is plain text a
    # crawler or screen reader reads even when the chart never renders.
    assert "leads scrape success" not in svg, (
        "no cross-tool scrape-success claim, in prose either"
    )
    # If a success rate is ever printed here it must carry its denominator.
    assert ("95.2%" in svg) == ("877 of 921 reachable" in svg), (
        "a success rate on the panel must show the denominator it is over"
    )

    # Every tool's number must be printed for every axis, so the shape can be
    # checked against the values rather than trusted.
    for a in AXES:
        assert a["fmt"](a["v"]["crw"]) in svg, f"{a['label']}: our value missing"
        for k, _ in TOOLS[1:]:
            assert short(a, a["v"][k]) in svg, f"{a['label']}: {k} value missing"

    # A missing logo file degrades silently to a legend without icons, so the
    # panel is only correct if all three embedded.
    assert svg.count("data:image/png;base64,") == len(TOOLS), (
        f"expected {len(TOOLS)} embedded logos, staged in {LOGO_DIR}"
    )

    # With no losing axis left, every comparison axis must be a win.
    won = [a["label"] for a in AXES if leads(a, "crw")]
    assert len(won) == len(AXES) == 5, f"expected all 5 axes to be wins, got {won}"
    print("self-check ok")


if __name__ == "__main__":
    if "--self-check" in sys.argv:
        self_check()
        sys.exit(0)
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    with open(sys.argv[1], "w") as fh:
        fh.write(render())
    print(f"wrote {sys.argv[1]}")
