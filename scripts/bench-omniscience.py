#!/usr/bin/env python3
"""Renders the AA-Omniscience answer-accuracy strip in .github/benchmarks/.

Deliberately a SEPARATE figure from bench-radar.svg rather than a sixth axis on
it. The radar's five axes all come from one run of Firecrawl's public 1,000-URL
scrape dataset, with all three tools through the same matcher, and its caption
says exactly that. This is a different dataset, a different harness and a
different task, so putting it on the same polygon would make that caption false.
Crawl4AI is also absent here for a real reason rather than an oversight: it is a
scraper, not a search API, so it is not on the Artificial Analysis board at all
and there is no honest value to plot for it.

Numbers: fastCRW is our own run over all 600 questions, in a rebuild of
Artificial Analysis's published harness, validated by running a listed provider
through the same rebuild and reproducing its published score to within 1.5
points. Every other row is that provider's published score on the public board.
"""
import base64
import pathlib

# Board figures from artificialanalysis.ai/agents/search-api. Only the providers
# a reader is likely to recognise are shown; the full 14-product table lives on
# the benchmark page linked from the caption.
ROWS = [
    ("fastCRW", 90.0, True, "fastcrw.png"),
    ("Firecrawl", 73, False, "firecrawl.png"),
    ("Exa", 70, False, "exa.png"),
    ("You.com", 69, False, "youcom.png"),
    ("Parallel", 68, False, "parallel.png"),
    ("Tavily", 64, False, "tavily.png"),
    ("No search", 38, False, None),
]

LOGO_DIR = pathlib.Path(__file__).resolve().parent.parent / ".github/benchmarks/logos"

GREEN, RIVAL = "#16A34A", "#C9CEC9"
BG, RULE, CAP, MUT, INK = "#FFFFFF", "#E6E8E6", "#A2A9A4", "#6B7280", "#111827"
FONT = "-apple-system,BlinkMacSystemFont,Segoe UI,Helvetica,Arial,sans-serif"
MONO = "ui-monospace,SFMono-Regular,Menlo,monospace"

W, PAD, ROW, TOP = 1120, 40, 46, 148
LBL, GAP, ICON = 150, 16, 22
BARX = PAD + LBL + GAP
BARW = W - BARX - PAD - 86
H = TOP + len(ROWS) * ROW + 84


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def logo(name):
    """The provider's real icon as a base64 data URI, or None when not staged.

    Embedded rather than linked because the figure is rendered inside a README
    on github.com, where a remote image request would be proxied or blocked.
    """
    if not name:
        return None
    path = LOGO_DIR / name
    if not path.exists():
        return None
    return "data:image/png;base64," + base64.b64encode(path.read_bytes()).decode()


def text(x, y, s, fill, size, weight=400, anchor="start", font=FONT):
    return (
        f'<text x="{x:.1f}" y="{y:.1f}" fill="{fill}" font-family="{font}" '
        f'font-size="{size}" font-weight="{weight}" text-anchor="{anchor}">{esc(s)}</text>'
    )


def alt_text():
    """One sentence describing the whole chart, for screen readers and for the
    README's img alt. Built from ROWS so the two can never drift apart."""
    ours = next(v for n, v, us, _i in ROWS if us)
    rivals = ", ".join(f"{n} {v:g}" for n, v, us, _i in ROWS if not us and n != "No search")
    base = next((v for n, _v, _u, _i in ROWS if n == "No search" for v in [_v]), None)
    tail = f", and {base:g} with no search at all" if base is not None else ""
    return (
        f"AA-Omniscience answer accuracy. fastCRW {ours:.1f} percent, ahead of every product "
        f"on the Artificial Analysis Search Index: {rivals}{tail}."
    )


def render():
    # Derived from ROWS rather than written out, so adding a provider can never
    # leave the accessible description describing an older version of the chart.
    head = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
        f'viewBox="0 0 {W} {H}" role="img" aria-label="{esc(alt_text())}">'
    )
    p = [
        head,
        f'<rect width="{W}" height="{H}" fill="{BG}"/>',
        text(PAD, 52, "AA-Omniscience", INK, 30, 700),
        text(PAD, 80, "600 factual questions. One attempt, no partial credit, same grader for every provider.", MUT, 15),
        text(PAD, 104, "fastCRW answers 90.0% of them, ahead of every product on the public board.", GREEN, 15, 600),
    ]

    # The axis runs 0-100 and is never cropped: the gap is large enough that it
    # does not need help, and a clipped axis is the first thing a reader distrusts.
    for g in (25, 50, 75, 100):
        x = BARX + BARW * g / 100
        p.append(
            f'<line x1="{x:.1f}" y1="{TOP - 10}" x2="{x:.1f}" y2="{TOP + len(ROWS) * ROW - 10}" '
            f'stroke="{RULE}" stroke-width="1"/>'
        )
        p.append(text(x, TOP - 18, str(g), CAP, 11.5, 400, "middle", MONO))

    for i, (name, val, us, icon) in enumerate(ROWS):
        y = TOP + i * ROW
        bw = BARW * val / 100
        href = logo(icon)
        if href:
            p.append(
                f'<image href="{href}" x="{PAD}" y="{y + 2}" width="{ICON}" height="{ICON}" '
                f'preserveAspectRatio="xMidYMid meet"/>'
            )
        # Left-aligned as one block, icon then name, matching the radar's legend.
        # A right-aligned name would leave a ragged gap after each icon.
        p.append(text(PAD + ICON + 10, y + 18, name, INK if us else MUT, 15, 700 if us else 400))
        p.append(
            f'<rect x="{BARX}" y="{y + 1}" width="{bw:.1f}" height="24" rx="3" '
            f'fill="{GREEN if us else RIVAL}"/>'
        )
        p.append(
            text(BARX + bw + 12, y + 18, f"{val:.1f}" if us else f"{val:g}",
                 INK if us else MUT, 17 if us else 14, 700 if us else 500, "start", MONO)
        )

    fy = TOP + len(ROWS) * ROW + 26
    p.append(f'<line x1="{PAD}" y1="{fy - 18}" x2="{W - PAD}" y2="{fy - 18}" stroke="{RULE}" stroke-width="1"/>')
    p.append(text(
        PAD, fy + 2,
        "Rival figures published on artificialanalysis.ai/agents/search-api. Logos are each provider's own mark.",
        CAP, 11.5,
    ))
    p.append(text(
        PAD, fy + 20,
        "fastCRW measured on all 600 questions in a rebuild of the same harness, validated by reproducing a listed "
        "provider's published score to within 1.5 points.",
        CAP, 11.5,
    ))
    p.append(text(W - PAD, fy + 20, "github.com/fastcrw/crw", CAP, 11.5, 400, "end", MONO))
    p.append("</svg>")
    return "\n".join(p)


def main():
    # Guardrail: this figure exists to show a win, and a silently-flipped number
    # would invert the story without anything failing. Assert it outright.
    best_rival = max(v for _, v, us, _icon in ROWS if not us)
    ours = next(v for _, v, us, _icon in ROWS if us)
    assert ours > best_rival, f"fastCRW {ours} is not ahead of the best rival {best_rival}"

    # A missing logo file degrades silently to a nameless row, so fail instead.
    want = [icon for _n, _v, _u, icon in ROWS if icon]
    missing = [i for i in want if not (LOGO_DIR / i).exists()]
    assert not missing, f"logos not staged in {LOGO_DIR}: {missing}"

    out = pathlib.Path(__file__).resolve().parent.parent / ".github/benchmarks/bench-omniscience.svg"
    out.write_text(render())
    print(f"wrote {out} ({out.stat().st_size} bytes), fastCRW {ours} vs best rival {best_rival}")
    print(f'alt: {alt_text()}')


if __name__ == "__main__":
    main()
