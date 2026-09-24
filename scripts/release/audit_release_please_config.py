#!/usr/bin/env python3
"""Validate every extra-files entry in release-please-config.json.

Catches the class of regression that hid the npm optionalDependencies pin
problem: stale jsonpaths that point at fields that no longer exist (e.g.
`crates/crw-core/Cargo.toml::$.dependencies.crw-core.version`) silently
no-op during release-please runs and leave version surfaces stale.

Supports json + toml + bare string forms (codex review v6 C13).
"""
from __future__ import annotations

import json
import re
import sys
import tomllib
from pathlib import Path


def _toml_jsonpath_lookup(data: dict, jsonpath: str) -> bool:
    """Naive `$.a.b.c` walk over a TOML-loaded dict.

    release-please's TOML extra-files use simple dotted paths; we don't need
    the full jsonpath-ng grammar. Anything more exotic would need to be
    handled explicitly.
    """
    parts = re.findall(r"[\w-]+", jsonpath)
    cur = data
    for seg in parts:
        if isinstance(cur, dict) and seg in cur:
            cur = cur[seg]
        else:
            return False
    return True


def _json_jsonpath_lookup(data, jsonpath: str) -> bool:
    try:
        import jsonpath_ng  # type: ignore
    except ImportError:
        # Fallback: same naive walker. Good enough for `$.a['b'].c` shapes.
        # Strip $ and quotes/brackets, treat as dotted.
        flat = jsonpath.replace("$.", "").replace("$", "")
        flat = re.sub(r"\[['\"]?([^'\"\]]+)['\"]?\]", r".\1", flat)
        parts = [p for p in flat.split(".") if p]
        cur = data
        for seg in parts:
            if isinstance(cur, dict) and seg in cur:
                cur = cur[seg]
            else:
                return False
        return True
    return bool(jsonpath_ng.parse(jsonpath).find(data))


# Where centralized internal version pins live and the jsonpath segment that
# addresses them. Internal crate-to-crate versions are declared ONCE in the
# root `[workspace.dependencies]` table (members inherit via `workspace = true`),
# so the only version strings release-please must bump are these.
_WS_DEPS_PATH = "Cargo.toml"
_WS_DEPS_TABLE = "workspace.dependencies"


def _internal_version_pins() -> set[tuple[str, str, str]]:
    """Every internal `crw-*` pin in root `[workspace.dependencies]`.

    Returns (cargo_path, table, dep_name) tuples for entries that carry BOTH a
    `path` (so they resolve to a workspace crate) and a `version` requirement.
    Those version strings must be bumped in lockstep with the workspace version
    on every release, which means each one needs a release-please-config
    extra-files entry of shape `$.workspace.dependencies.<dep>.version`. A pin
    without an entry is the exact bug that broke the 0.12.0 release (crw-diff
    -> crw-core stayed at 0.11.0 after the bump). Since the migration to
    workspace inheritance, these pins are centralized in ONE file instead of
    scattered across every member manifest.
    """
    pins: set[tuple[str, str, str]] = set()
    root = tomllib.loads(Path(_WS_DEPS_PATH).read_text())
    ws_deps = root.get("workspace", {}).get("dependencies", {})
    for name, spec in ws_deps.items():
        if not name.startswith("crw-"):
            continue
        if isinstance(spec, dict) and "version" in spec and "path" in spec:
            pins.add((_WS_DEPS_PATH, _WS_DEPS_TABLE, name))
    return pins


def _config_covered_pins(cfg: dict) -> set[tuple[str, str, str]]:
    """(path, table, dep) covered by a `$.workspace.dependencies.<dep>.version`
    extra-file entry into the root manifest."""
    covered: set[tuple[str, str, str]] = set()
    for pkg in cfg.get("packages", {}).values():
        for ef in pkg.get("extra-files", []):
            if not isinstance(ef, dict) or ef.get("type") != "toml":
                continue
            path_str, jsonpath = ef.get("path"), ef.get("jsonpath")
            if not path_str or not jsonpath:
                continue
            parts = re.findall(r"[\w-]+", jsonpath)
            # New centralized shape: $.workspace.dependencies.<dep>.version
            if (
                len(parts) == 4
                and parts[0] == "workspace"
                and parts[1] == "dependencies"
                and parts[3] == "version"
            ):
                covered.add((path_str, _WS_DEPS_TABLE, parts[2]))
    return covered


_LOCK_PATH = "Cargo.lock"
_LOCK_NAME = re.compile(r"@\.name\.value == '([\w-]+)'")
_LOCK_TERM = r"@\.name\.value == '[\w-]+'"
# The exact shape verified against the release-please + jsonpath-plus versions
# bundled in the pinned action: anything else may silently match nothing.
_LOCK_SHAPE = re.compile(
    rf"^\$\.package\[\?\({_LOCK_TERM}(?: \|\| {_LOCK_TERM})*\)\]\.version$"
)


def _audit_lock_entry(data: dict, jsonpath: str) -> list[str]:
    """Cargo.lock is an array of `[[package]]` tables, so its entry selects the
    workspace crates with a jsonpath filter (`@.name.value`, because
    release-please's TOML parser wraps every value) that the dotted walk above
    cannot follow. Check instead that the filter names exactly the workspace
    crates: a missing one keeps its old lock version after a release bump, an
    extra one (a registry dependency) gets the release version written over it.
    The filter matches by name only, so each name must also be locked exactly
    once, as the workspace crate (no `source`)."""
    root = tomllib.loads(Path(_WS_DEPS_PATH).read_text())
    members = {
        tomllib.loads((Path(m) / "Cargo.toml").read_text())["package"]["name"]
        for m in root.get("workspace", {}).get("members", [])
    }
    names = set(_LOCK_NAME.findall(jsonpath))
    if not _LOCK_SHAPE.match(jsonpath):
        shape = "$.package[?(@.name.value == '<crate>' || ...)].version"
        return [f"{_LOCK_PATH}::{jsonpath}: must be {shape}"]
    errors = [
        f"{_LOCK_PATH}: workspace crate {n} missing from the jsonpath filter "
        f"(its lock version would go stale on the next bump)"
        for n in sorted(members - names)
    ]
    errors += [
        f"{_LOCK_PATH}: {n} in the jsonpath filter is not a workspace crate "
        f"(release-please would overwrite its locked version)"
        for n in sorted(names - members)
    ]
    for n in sorted(names & members):
        entries = [pkg for pkg in data.get("package", []) if pkg.get("name") == n]
        if len(entries) != 1 or "source" in entries[0]:
            errors.append(
                f"{_LOCK_PATH}: {n} must be locked exactly once, as the "
                f"workspace crate (a same-name registry entry would be bumped too)"
            )
    return errors


def main(config_path: Path = Path("release-please-config.json")) -> int:
    if not config_path.exists():
        print(f"::error::{config_path} not found", file=sys.stderr)
        return 1
    cfg = json.loads(config_path.read_text())

    errors: list[str] = []

    # Completeness: every internal version pin must have an extra-files entry,
    # so release-please keeps inter-crate version requirements in lockstep with
    # the workspace version. Missing entries silently break the build on the
    # first bump after a crate/dep is added.
    covered = _config_covered_pins(cfg)
    required = _internal_version_pins()
    # Anti-vacuity: an empty required set means the centralized internal pins
    # vanished (botched migration, renamed table, parse error). Without this the
    # completeness loop below would iterate zero times and pass silently —
    # exactly the vacuous-green failure this guard exists to prevent.
    if not required:
        errors.append(
            "no internal crw-* pins found in root [workspace.dependencies] "
            "(expected the centralized internal crates) — completeness check "
            "would be vacuous; refusing to pass"
        )
    for path_str, table, dep in sorted(required):
        if (path_str, table, dep) not in covered:
            errors.append(
                f"{path_str}::$.{table}.{dep}.version: internal version pin not "
                f"tracked in release-please-config.json extra-files "
                f"(would go stale on the next version bump)"
            )
    # A committed Cargo.lock with no extra-files entry lags one release behind:
    # release-please bumps Cargo.toml, and the lock only catches up whenever
    # someone happens to commit it after a local build.
    extra_files = [
        ef
        for pkg in cfg.get("packages", {}).values()
        for ef in pkg.get("extra-files", [])
    ]
    if Path(_LOCK_PATH).exists() and not any(
        isinstance(ef, dict)
        and ef.get("path") == _LOCK_PATH
        and ef.get("type") == "toml"
        for ef in extra_files
    ):
        errors.append(
            f"{_LOCK_PATH}: not tracked by a type=toml release-please-config.json extra-file "
            f"(workspace crate versions in the lock would go stale on every bump)"
        )
    for pkg_name, pkg in cfg.get("packages", {}).items():
        for ef in pkg.get("extra-files", []):
            # Bare string form: just a path, no jsonpath.
            if isinstance(ef, str):
                if not Path(ef).exists():
                    errors.append(f"{ef}: file missing (string-form extra-file)")
                continue

            path_str = ef.get("path")
            if not path_str:
                errors.append(f"{pkg_name}: extra-file missing 'path'")
                continue
            p = Path(path_str)
            if not p.exists():
                errors.append(f"{path_str}: file missing")
                continue

            t = ef.get("type", "generic")
            jsonpath = ef.get("jsonpath")

            if t == "json":
                if not jsonpath:
                    errors.append(f"{path_str}: type=json missing jsonpath")
                    continue
                try:
                    data = json.loads(p.read_text())
                except json.JSONDecodeError as e:
                    errors.append(f"{path_str}: invalid JSON: {e}")
                    continue
                if not _json_jsonpath_lookup(data, jsonpath):
                    errors.append(f"{path_str}::{jsonpath} (json): jsonpath not found")
            elif t == "toml":
                if not jsonpath:
                    errors.append(f"{path_str}: type=toml missing jsonpath")
                    continue
                try:
                    data = tomllib.loads(p.read_text())
                except tomllib.TOMLDecodeError as e:
                    errors.append(f"{path_str}: invalid TOML: {e}")
                    continue
                if path_str == _LOCK_PATH:
                    errors.extend(_audit_lock_entry(data, jsonpath))
                elif not _toml_jsonpath_lookup(data, jsonpath):
                    errors.append(f"{path_str}::{jsonpath} (toml): jsonpath not found")
            elif t in ("generic", "yaml", "xml"):
                # generic uses regex against file contents; cannot statically
                # validate without re-implementing release-please. Skip.
                continue
            else:
                errors.append(f"{path_str}: unknown extra-file type '{t}'")

    if errors:
        print("::error::release-please-config.json audit failed:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1

    print(f"::notice::release-please-config.json audit OK ({sum(len(p.get('extra-files', [])) for p in cfg.get('packages', {}).values())} entries)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
