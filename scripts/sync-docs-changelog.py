#!/usr/bin/env python3

from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "CHANGELOG.md"
TARGET = ROOT / "docs" / "docs" / "changelog.md"

INTRO = """# Changelog

This page is generated from the root [`CHANGELOG.md`](https://github.com/fastcrw/crw/blob/main/CHANGELOG.md), which is maintained by release-please during releases.

:::note
The source of truth is the repository root changelog. Do not edit this docs page manually.
:::

"""


def strip_top_heading(markdown: str) -> str:
    lines = markdown.splitlines()
    if lines and lines[0].strip() == "# Changelog":
        lines = lines[1:]
        while lines and not lines[0].strip():
            lines = lines[1:]
    return "\n".join(lines).rstrip() + "\n"


def main() -> None:
    if not SOURCE.exists():
        print(f"Skipping: {SOURCE.relative_to(ROOT)} not found")
        return
    source_body = strip_top_heading(SOURCE.read_text())
    TARGET.write_text(INTRO + source_body)
    print(f"Synced {TARGET.relative_to(ROOT)} from {SOURCE.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
