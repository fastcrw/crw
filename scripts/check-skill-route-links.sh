#!/usr/bin/env bash
# Mechanical guard against two drift classes in skills/ content:
#
#   1. MISSING SKILL: an `npx skills add fastcrw/crw@<name>` install line names a
#      skill directory (skills/<name>/) that does not exist.
#   2. DEAD ROUTE: a documented `METHOD /path`, or a bare `/v1`, `/v2` or
#      `/firecrawl` path named in backticks with no method attached (e.g. a
#      sentence mentioning "POST to `/v1/crawl`"), names an HTTP path that is
#      not registered in the axum router.
#
# Route ground truth is exactly crates/crw-server/src/routes/v1/mod.rs and
# routes/v2/mod.rs (each also mounted verbatim under /firecrawl by
# crates/crw-server/src/app.rs), nothing else. Route families outside v1/v2
# (e.g. /mcp, /health, /metrics) are out of scope for this check, so a mention
# of one of those is never flagged.
#
# A `METHOD /path` mention on a line that explicitly says the endpoint does
# NOT exist ("Not implemented", "no equivalent", ...) is the doc correctly
# describing an absence, not a false claim, and is skipped.
#
# `/v1/monitor*` is allowlisted outright: docs/docs/monitoring.md documents it
# (with its own base URL, https://fastcrw.com/api) as a SaaS-only
# control-plane feature that deliberately ships no route in crw-server's
# router, a real cross-repo architectural split, not doc drift. If that split
# ever changes, drop the allowlist entry in the same commit that adds the
# route to routes/v1/mod.rs.
#
# Skips gracefully (does not fail CI) if the router source files are absent.
#
# Portable: bash + python3.
set -euo pipefail

cd "${CHECK_REPO_ROOT:-$(dirname "$0")/..}"

V1_MOD="crates/crw-server/src/routes/v1/mod.rs"
V2_MOD="crates/crw-server/src/routes/v2/mod.rs"

if [ ! -f "$V1_MOD" ] || [ ! -f "$V2_MOD" ]; then
  echo "skip: ${V1_MOD} or ${V2_MOD} not found, cannot derive the real route set" >&2
  exit 0
fi

python3 - "$V1_MOD" "$V2_MOD" <<'PY'
import re
import sys
from pathlib import Path

v1_mod, v2_mod = (Path(p) for p in sys.argv[1:3])

def routes_in(path):
    text = re.sub(r"//[^\n]*", "", path.read_text())  # strip line comments
    return set(re.findall(r'\.route\(\s*"([^"]+)"', text))

real_routes = routes_in(v1_mod) | routes_in(v2_mod)
if not real_routes:
    print("error: parsed zero routes, regex is out of sync with routes/v{1,2}/mod.rs")
    sys.exit(2)
# Every v1/v2 route is also mounted verbatim under /firecrawl (app.rs nest).
real_routes |= {"/firecrawl" + r for r in real_routes}


def normalize(path):
    # Collapse any {param} or :param segment so a param-name mismatch (e.g.
    # {id} vs {job_id}, or a prose `:id` vs the router's `{id}`) never causes
    # a false positive.
    path = re.sub(r"/:[^/]+", "/{param}", path)
    return re.sub(r"\{[^}/]+\}", "{param}", path)


real_normalized = {normalize(r) for r in real_routes}

# See the header comment: documented but deliberately not in this repo's router.
ALLOWLISTED_PREFIXES = ("/v1/monitor",)


def strip_firecrawl(path):
    # A doc mentioning the /firecrawl-prefixed variant of an allowlisted path
    # (e.g. `/firecrawl/v1/monitor`) is the same legitimate mention as the
    # root form; compare the allowlist against the path with that prefix
    # removed so it isn't raised as a false dead-route failure.
    return path[len("/firecrawl") :] if path.startswith("/firecrawl/") else path


VERSION_TOKENS = {"v1", "v2", "firecrawl"}


def is_namespace_mention(path):
    # A bare version/compat-namespace reference ("under `/v1/*`", "the
    # `/firecrawl/v2` surface", "everything under `/v1`") names no concrete
    # endpoint and is never a real router entry, so it is out of scope rather
    # than a false dead-route claim.
    if path.endswith("*"):
        return True
    segments = [s for s in path.split("/") if s]
    return bool(segments) and all(s in VERSION_TOKENS for s in segments)

NEGATIONS = (
    "not implemented",
    "no equivalent",
    "not supported",
    "no such",
    "does not exist",
    "cloud-only",
    "roadmap",
)

skills_dir = Path("skills")
skill_names = {p.name for p in skills_dir.iterdir() if p.is_dir()} if skills_dir.is_dir() else set()

files = list(skills_dir.rglob("*.md")) if skills_dir.is_dir() else []
files += list(Path("docs").rglob("*.md")) if Path("docs").is_dir() else []

ROUTE_RE = re.compile(r"\b(?:GET|POST|PUT|PATCH|DELETE) (/(?:v1|v2|firecrawl)/[a-zA-Z0-9/_{}:-]*)")
# A bare path in backticks with no method attached, e.g. "POST to `/v1/crawl`
# returns a job ID" or a sentence just naming `/v1/extract` on its own.
# Anchored to backticks on both sides so a path embedded in a URL (which is
# never wrapped as bare backtick-path text starting with /v1 in this corpus)
# is never mistaken for a route mention.
BARE_PATH_RE = re.compile(r"`(/(?:v1|v2|firecrawl)[a-zA-Z0-9/_{}:*.-]*)`")
SKILL_RE = re.compile(r"fastcrw/crw@([a-zA-Z0-9-]+)")

dead_routes = []  # (file, line_no, path)
missing_skills = []  # (file, line_no, name)

for f in files:
    try:
        lines = f.read_text().splitlines()
    except (UnicodeDecodeError, OSError):
        continue
    # A negated intro sentence ("... have no equivalent ... and are not
    # planned:") commonly introduces a bullet list of the actual dead paths,
    # one per line, so the negation words never appear on the bullet's own
    # line. Carry the intro's negation onto the bullet lines that follow it
    # (blank lines don't break the carry; the next non-bullet paragraph line
    # does) so that style of list is read correctly instead of as drift.
    list_negated = False
    for i, line in enumerate(lines, start=1):
        stripped = line.strip()
        low = line.lower()
        this_negated = any(n in low for n in NEGATIONS)
        if stripped and not stripped.startswith(("-", "*")):
            list_negated = this_negated and stripped.endswith(":")
        negated = this_negated or (list_negated and stripped.startswith(("-", "*")))
        paths = {m.group(1) for m in ROUTE_RE.finditer(line)}
        paths |= {m.group(1) for m in BARE_PATH_RE.finditer(line)}
        for raw in paths:
            path = raw.rstrip("/")
            if is_namespace_mention(path):
                continue
            allowlisted = strip_firecrawl(path).startswith(ALLOWLISTED_PREFIXES)
            if not negated and not allowlisted and normalize(path) not in real_normalized:
                dead_routes.append((str(f), i, path))
        for name in SKILL_RE.findall(line):
            if name not in skill_names:
                missing_skills.append((str(f), i, name))

problems = []
if missing_skills:
    problems.append("skill install lines naming a directory that does not exist:")
    for f, i, name in missing_skills:
        problems.append(f"    {f}:{i}: skills/{name}/ does not exist")
if dead_routes:
    problems.append("documented routes not registered in the axum router:")
    for f, i, path in dead_routes:
        problems.append(f"    {f}:{i}: {path}")

if problems:
    print("FAIL: skill/route drift found:\n")
    for p in problems:
        print(f"  {p}")
    sys.exit(1)

print(
    f"ok: {len(real_routes)} known routes, {len(skill_names)} skill dirs, "
    "no dead routes or missing skill dirs referenced"
)
PY
