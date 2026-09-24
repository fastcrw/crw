#!/usr/bin/env python3
"""Regression tests for the release version-sync guards.

The guards (`check-internal-dep-versions.sh` and `audit_release_please_config.py`)
are load-bearing: they are the only thing standing between a forgotten version
surface and a broken/empty release (the failure class that recurred across
v0.9.0, v0.12.0). A guard that silently passes is worse than no guard, so these
tests pin the load-bearing failure modes — drift, missing config entry,
anti-vacuity, invalid jsonpath — and the green happy path. If a future refactor
weakens a guard, one of these goes red instead of the release.

Each test builds a minimal fixture workspace in a tmpdir and runs the REAL
script entrypoints against it (the bash guard via CHECK_REPO_ROOT, the audit via
cwd), so we test what actually ships, not a reimplementation.

Dependency-free: runs under plain `uv run python scripts/release/test_guards.py`
(no pytest needed — robust in CI without a network install) and is ALSO
collectible by pytest if present, since the test_* functions take no fixtures.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
GUARD = REPO / "scripts" / "check-internal-dep-versions.sh"
AUDIT = REPO / "scripts" / "release" / "audit_release_please_config.py"

VERSION = "1.0.0"


def _write(p: Path, text: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


def make_fixture(tmp: Path) -> None:
    """A minimal but valid post-migration workspace: one internal dep (crw-core)
    centralized in [workspace.dependencies], consumed by crw-server via
    inheritance, plus crw-server's allowlisted self dev-dep."""
    _write(
        tmp / "Cargo.toml",
        f"""[workspace]
members = ["crates/crw-core", "crates/crw-server"]

[workspace.package]
version = "{VERSION}"

[workspace.dependencies]
crw-core = {{ path = "crates/crw-core", version = "{VERSION}" }}
""",
    )
    _write(
        tmp / "crates" / "crw-core" / "Cargo.toml",
        """[package]
name = "crw-core"
version.workspace = true

[dependencies]
""",
    )
    _write(
        tmp / "crates" / "crw-server" / "Cargo.toml",
        """[package]
name = "crw-server"
version.workspace = true

[dependencies]
crw-core = { workspace = true }

[dev-dependencies]
crw-server = { path = ".", features = ["test-utils"] }
""",
    )
    _write(
        tmp / "release-please-config.json",
        json.dumps(
            {
                "packages": {
                    ".": {
                        "extra-files": [
                            {
                                "type": "toml",
                                "path": "Cargo.toml",
                                "jsonpath": "$.workspace.package.version",
                            },
                            {
                                "type": "toml",
                                "path": "Cargo.toml",
                                "jsonpath": "$.workspace.dependencies.crw-core.version",
                            },
                        ]
                    }
                }
            },
            indent=2,
        ),
    )


def run_guard(fixture: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["bash", str(GUARD)],
        env={**os.environ, "CHECK_REPO_ROOT": str(fixture)},
        capture_output=True,
        text=True,
    )


def run_audit(fixture: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(AUDIT)],
        cwd=str(fixture),
        capture_output=True,
        text=True,
    )


def patch_toml(fixture: Path, old: str, new: str) -> None:
    p = fixture / "Cargo.toml"
    text = p.read_text()
    assert old in text, f"fixture anchor not found: {old!r}"
    p.write_text(text.replace(old, new))


def patch_config(fixture: Path, fn) -> None:
    p = fixture / "release-please-config.json"
    cfg = json.loads(p.read_text())
    fn(cfg)
    p.write_text(json.dumps(cfg, indent=2))


# --- happy path -----------------------------------------------------------


def test_green_baseline(tmp_path):
    make_fixture(tmp_path)
    g = run_guard(tmp_path)
    a = run_audit(tmp_path)
    assert g.returncode == 0, g.stdout + g.stderr
    assert a.returncode == 0, a.stdout + a.stderr


# --- guard failure modes --------------------------------------------------


def test_drift_pin_off_workspace_version(tmp_path):
    """A [workspace.dependencies] pin not equal to the workspace version → red."""
    make_fixture(tmp_path)
    patch_toml(
        tmp_path,
        'crw-core = { path = "crates/crw-core", version = "1.0.0" }',
        'crw-core = { path = "crates/crw-core", version = "0.9.0" }',
    )
    assert run_guard(tmp_path).returncode == 1


def test_anti_vacuity_no_centralized_pins(tmp_path):
    """If the centralized internal pins vanish, the guard must NOT pass vacuously."""
    make_fixture(tmp_path)
    patch_toml(
        tmp_path,
        '\ncrw-core = { path = "crates/crw-core", version = "1.0.0" }\n',
        "\n",
    )
    assert run_guard(tmp_path).returncode == 1


def test_residual_inline_pin_rejected(tmp_path):
    """A member re-declaring an inline internal path pin (instead of inheriting) → red."""
    make_fixture(tmp_path)
    server = tmp_path / "crates" / "crw-server" / "Cargo.toml"
    server.write_text(
        server.read_text().replace(
            "crw-core = { workspace = true }",
            'crw-core = { path = "../crw-core", version = "1.0.0" }',
        )
    )
    assert run_guard(tmp_path).returncode == 1


def test_self_dev_dep_allowlisted(tmp_path):
    """The crw-server self dev-dep (path = '.', no version) must stay allowed."""
    make_fixture(tmp_path)
    # Baseline already contains the self dev-dep; guard must be green.
    assert run_guard(tmp_path).returncode == 0


# --- audit failure modes --------------------------------------------------


def test_missing_config_entry(tmp_path):
    """An internal pin with no release-please-config entry → audit red (it would
    go stale on the next bump — the exact v0.12.0 break)."""
    make_fixture(tmp_path)

    def drop_crw_core(cfg):
        ef = cfg["packages"]["."]["extra-files"]
        cfg["packages"]["."]["extra-files"] = [
            e
            for e in ef
            if e.get("jsonpath") != "$.workspace.dependencies.crw-core.version"
        ]

    patch_config(tmp_path, drop_crw_core)
    assert run_audit(tmp_path).returncode == 1


def test_invalid_jsonpath(tmp_path):
    """A config entry whose jsonpath doesn't resolve to a live field → audit red."""
    make_fixture(tmp_path)

    def add_bad(cfg):
        cfg["packages"]["."]["extra-files"].append(
            {
                "type": "toml",
                "path": "Cargo.toml",
                "jsonpath": "$.workspace.dependencies.crw-nonexistent.version",
            }
        )

    patch_config(tmp_path, add_bad)
    assert run_audit(tmp_path).returncode == 1


def test_audit_anti_vacuity(tmp_path):
    """If no internal pins exist in [workspace.dependencies], the audit's
    completeness check must refuse to pass vacuously."""
    make_fixture(tmp_path)
    patch_toml(
        tmp_path,
        '\ncrw-core = { path = "crates/crw-core", version = "1.0.0" }\n',
        "\n",
    )

    def drop_crw_core(cfg):
        cfg["packages"]["."]["extra-files"] = [
            e
            for e in cfg["packages"]["."]["extra-files"]
            if e.get("jsonpath") != "$.workspace.dependencies.crw-core.version"
        ]

    patch_config(tmp_path, drop_crw_core)
    assert run_audit(tmp_path).returncode == 1


def _add_lock(fixture: Path, crates: list[str], entry: bool = True) -> None:
    """Give the fixture a Cargo.lock and, when `entry`, a release-please entry
    whose filter names `crates`."""
    _write(
        fixture / "Cargo.lock",
        "version = 4\n"
        + "".join(
            f'\n[[package]]\nname = "{c}"\nversion = "{VERSION}"\n'
            for c in ("crw-core", "crw-server", "serde")
        ),
    )
    if not entry:
        return
    names = " || ".join(f"@.name.value == '{c}'" for c in crates)

    def add(cfg):
        cfg["packages"]["."]["extra-files"].append(
            {
                "type": "toml",
                "path": "Cargo.lock",
                "jsonpath": f"$.package[?({names})].version",
            }
        )

    patch_config(fixture, add)


def test_lock_entry_covering_every_member_is_green(tmp_path):
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core", "crw-server"])
    r = run_audit(tmp_path)
    assert r.returncode == 0, r.stderr


def test_lock_entry_missing_a_member(tmp_path):
    """A crate left out of the Cargo.lock filter keeps its old lock version
    after the bump: the drift that left the lock at 0.35.1 under 0.36.0."""
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core"])
    r = run_audit(tmp_path)
    assert r.returncode == 1 and "crw-server" in r.stderr


def test_lock_entry_not_targeting_version(tmp_path):
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core", "crw-server"])

    def retarget(cfg):
        cfg["packages"]["."]["extra-files"][-1]["jsonpath"] = cfg["packages"]["."][
            "extra-files"
        ][-1]["jsonpath"].replace(".version", ".checksum")

    patch_config(tmp_path, retarget)
    assert run_audit(tmp_path).returncode == 1


def test_lock_entry_with_unverified_filter_syntax(tmp_path):
    """Right names, but a filter shape release-please was never verified with
    (here `&&`, which matches nothing): the lock would silently stay stale."""
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core", "crw-server"])

    def use_and(cfg):
        ef = cfg["packages"]["."]["extra-files"][-1]
        ef["jsonpath"] = ef["jsonpath"].replace(" || ", " && ")

    patch_config(tmp_path, use_and)
    assert run_audit(tmp_path).returncode == 1


def test_lock_entry_without_toml_type(tmp_path):
    """A `generic` entry is skipped by the audit and cannot bump lock versions."""
    make_fixture(tmp_path)
    _add_lock(tmp_path, [], entry=False)
    patch_config(
        tmp_path,
        lambda cfg: cfg["packages"]["."]["extra-files"].append({"path": "Cargo.lock"}),
    )
    assert run_audit(tmp_path).returncode == 1


def test_lock_entry_naming_a_registry_dependency(tmp_path):
    """`serde` in the filter would get the release version written over it."""
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core", "crw-server", "serde"])
    r = run_audit(tmp_path)
    assert r.returncode == 1 and "serde" in r.stderr


def test_lock_with_a_same_name_registry_crate(tmp_path):
    """The name filter would bump a registry `crw-core` too, breaking its checksum."""
    make_fixture(tmp_path)
    _add_lock(tmp_path, ["crw-core", "crw-server"])
    lock = tmp_path / "Cargo.lock"
    lock.write_text(
        lock.read_text()
        + '\n[[package]]\nname = "crw-core"\nversion = "0.1.0"\n'
        + 'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
    )
    r = run_audit(tmp_path)
    assert r.returncode == 1 and "exactly once" in r.stderr


def test_untracked_lock(tmp_path):
    make_fixture(tmp_path)
    _add_lock(tmp_path, [], entry=False)
    r = run_audit(tmp_path)
    assert r.returncode == 1 and "Cargo.lock: not tracked" in r.stderr


# ── crates.io checksum lookup ────────────────────────────────────────────
#
# `publish_crate.sh` treats "already uploaded" as success only when the local
# .crate sha matches the published one. The lookup used to read
# `version.cksum` from the crates.io v1 API, and that field is no longer in the
# response (verified 2026-09-10: absent for every crate). The helper returned an
# empty string with rc 0, the caller compared it straight against the local sha,
# and the v0.34.0 release died on "content mismatch — local=8e9006f... remote="
# for content that was in fact byte-identical, telling the maintainer to bump the
# version. These pin the two halves of that: the lookup must fail loudly rather
# than return empty, and the caller must not call an unreadable checksum a
# mismatch.
#
# `curl` and `cargo` are stubbed on PATH, so the REAL bash runs with no network.

LIB = REPO / "scripts" / "release" / "lib.sh"
PUBLISH_CRATE = REPO / "scripts" / "release" / "publish_crate.sh"

INDEX_BODY = (
    '{"name":"crw-mcp-proto","vers":"0.33.0","cksum":"aaaa","yanked":false}\n'
    '{"name":"crw-mcp-proto","vers":"0.34.0","cksum":"bbbb","yanked":false}\n'
)


def _stub_bin(dirpath: Path, name: str, body: str) -> None:
    """Put an executable stub on PATH."""
    f = dirpath / name
    _write(f, "#!/usr/bin/env bash\n" + body)
    f.chmod(0o755)


def _curl_stub(bindir: Path, index_body: str | None, present: bool = True) -> str:
    """A curl that answers the index URL and the v1 API URL, and nothing else.

    `index_body is None` simulates an unreachable index (curl -f exit 22). The
    body is written to a file rather than inlined, so the stub stays free of
    heredocs whose terminator would have to survive shell quoting.
    """
    if index_body is None:
        serve_index = "exit 22"
    else:
        body_file = bindir / "index_body.txt"
        _write(body_file, index_body)
        serve_index = f'cat "{body_file}"'
    num = '{"version":{"num":"0.34.0"}}' if present else '{"errors":[]}'
    return f"""
url="${{@: -1}}"
case "$url" in
  *index.crates.io*) {serve_index} ;;
  *api/v1/crates*)   printf '%s' '{num}' ;;
  *) exit 22 ;;
esac
"""


def _cksum(tmp_path: Path, crate: str, version: str, index_body, present=True):
    """Call the real `crate_version_cksum` with a stubbed curl."""
    bindir = tmp_path / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    _stub_bin(bindir, "curl", _curl_stub(bindir, index_body, present))
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    return subprocess.run(
        ["bash", "-c", f'set -euo pipefail; source "{LIB}"; crate_version_cksum "{crate}" "{version}"'],
        capture_output=True, text=True, env=env,
    )


def test_crate_index_path_follows_the_registry_protocol(tmp_path):
    out = subprocess.run(
        ["bash", "-c", f'source "{LIB}"; for n in a ab abc crw-core CRW-Core; do crate_index_path "$n"; echo; done'],
        capture_output=True, text=True,
    ).stdout.split()
    assert out == ["1/a", "2/ab", "3/a/abc", "cr/w-/crw-core", "cr/w-/crw-core"], out


def test_crate_cksum_reads_the_requested_version_from_the_index(tmp_path):
    r = _cksum(tmp_path, "crw-mcp-proto", "0.34.0", INDEX_BODY)
    assert r.returncode == 0, r.stderr
    assert r.stdout == "bbbb", f"got {r.stdout!r} (must not pick another version)"


def test_crate_cksum_fails_when_the_version_is_absent(tmp_path):
    r = _cksum(tmp_path, "crw-mcp-proto", "9.9.9", INDEX_BODY)
    assert r.returncode != 0, "an absent version must fail, not return empty-success"
    assert r.stdout == "", r.stdout


def test_crate_cksum_fails_when_the_index_is_unreachable(tmp_path):
    r = _cksum(tmp_path, "crw-mcp-proto", "0.34.0", None)
    assert r.returncode != 0, "an unreachable index must fail, not return empty-success"
    assert r.stdout == "", r.stdout


def test_unreadable_cksum_is_not_reported_as_a_content_mismatch(tmp_path):
    """The v0.34.0 failure, pinned end to end through the real script."""
    bindir = tmp_path / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    work = tmp_path / "work"
    (work / "target" / "package").mkdir(parents=True, exist_ok=True)
    _write(work / "target" / "package" / "crw-mcp-proto-0.34.0.crate", "payload")

    _stub_bin(bindir, "curl", _curl_stub(bindir, None))  # index unreachable
    _stub_bin(bindir, "cargo", """
case "${1:-}" in
  publish) echo "error: crate crw-mcp-proto@0.34.0 already exists on crates.io index" >&2; exit 101 ;;
  package) exit 0 ;;
  *) exit 0 ;;
esac
""")
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    r = subprocess.run(
        ["bash", str(PUBLISH_CRATE), "crw-mcp-proto", "0.34.0", "--source-dir", str(work)],
        capture_output=True, text=True, env=env,
    )
    combined = r.stdout + r.stderr
    assert r.returncode != 0, "an unverifiable upload must still fail the release"
    assert "checksum could not be read" in combined, combined
    assert "content mismatch" not in combined, "an unknown checksum is not a mismatch"
    assert "Bump the version" not in combined, "must not advise bumping on an unreadable checksum"


def test_matching_cksum_is_an_idempotent_skip(tmp_path):
    """The happy path the v0.34.0 run should have taken."""
    bindir = tmp_path / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    work = tmp_path / "work"
    (work / "target" / "package").mkdir(parents=True, exist_ok=True)
    crate_file = work / "target" / "package" / "crw-mcp-proto-0.34.0.crate"
    _write(crate_file, "payload")
    import hashlib

    real = hashlib.sha256(crate_file.read_bytes()).hexdigest()
    body = f'{{"name":"crw-mcp-proto","vers":"0.34.0","cksum":"{real}","yanked":false}}\n'

    _stub_bin(bindir, "curl", _curl_stub(bindir, body))
    _stub_bin(bindir, "cargo", """
case "${1:-}" in
  publish) echo "error: crate crw-mcp-proto@0.34.0 already exists on crates.io index" >&2; exit 101 ;;
  package) exit 0 ;;
  *) exit 0 ;;
esac
""")
    env = dict(os.environ, PATH=f"{bindir}:{os.environ['PATH']}")
    r = subprocess.run(
        ["bash", str(PUBLISH_CRATE), "crw-mcp-proto", "0.34.0", "--source-dir", str(work)],
        capture_output=True, text=True, env=env,
    )
    combined = r.stdout + r.stderr
    assert r.returncode == 0, combined
    assert "cksum matches" in combined, combined


def _run_standalone() -> int:
    """Minimal runner so the suite works without pytest installed.

    pytest's `tmp_path` is a builtin fixture, so under pytest these same
    test_* functions run unchanged; here we supply a fresh tmpdir per test.
    """
    tests = sorted(
        (name, fn)
        for name, fn in globals().items()
        if name.startswith("test_") and callable(fn)
    )
    failures = 0
    for name, fn in tests:
        with tempfile.TemporaryDirectory() as d:
            try:
                fn(Path(d))
                print(f"  PASS {name}")
            except Exception as e:  # noqa: BLE001 — test runner surfaces all
                failures += 1
                print(f"  FAIL {name}: {e}")
    total = len(tests)
    print(f"\n{total - failures}/{total} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(_run_standalone())
