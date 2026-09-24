# Release Operations

How crw-opencore ships, what to do when something breaks, and how to keep the pipeline honest.

## Topology

`release-please` watches `main` for conventional commits and opens a Release PR. Merging it creates the `vX.Y.Z` tag, which fires `.github/workflows/release.yml`. That workflow fans out to:

| Target           | Job                  | Verification                                           |
| ---------------- | -------------------- | ------------------------------------------------------ |
| crates.io        | `publish-crates`     | `verify_crates.sh` — exact-version GET on every crate  |
| GitHub Release   | `publish-binaries`   | `asset-gate` (18 tar.gz/zip files attached)            |
| PyPI             | `publish-pypi`       | `verify_pypi.sh`                                       |
| npm              | `publish-npm`        | `verify_npm.sh` — existence + optionalDeps pin + smoke |
| GHCR (Docker)    | `publish-docker`     | `verify_docker.sh` — version + latest + major.minor    |
| MCP Registry     | `publish-mcp-registry` | `verify_mcp_registry.sh`                             |
| APT repo (fastcrw/apt-crw) | `update-apt`    | `verify_apt_homebrew.sh` — commit-status correlation   |
| Homebrew tap (fastcrw/homebrew-crw) | `update-homebrew` | same                                       |

Each tag also produces `release-audit-<version>.md` as a workflow artifact and attaches it to the GitHub Release.

## Source of truth

- **Tier order & publish flags:** `scripts/release/release_manifest.toml`
- **Workspace publishability:** `scripts/release/preflight.py` (run on every PR via `preflight-publish.yml`)
- **release-please extra-files validity + internal-pin completeness:** `scripts/release/audit_release_please_config.py`
- **Internal crate-to-crate versions:** centralized in root `[workspace.dependencies]` (members inherit via `{ workspace = true }`); `scripts/check-internal-dep-versions.sh` enforces they equal the workspace version. Guarded against regression by `scripts/release/test_guards.py`.
- **Tag → version derivation:** `release-context` job in `release.yml` (semver-validated)

### Version cohesion — why the bump PR can't ship a stale surface

Every version surface (workspace version, the 10 internal `[workspace.dependencies]` pins, npm main + optionalDependencies + platform packages, `pyproject.toml`, `server.json`) must move in lockstep on a release. The historical break (v0.9.0, v0.12.0) was a stale surface slipping through because the **release-please bump PR is bot-authored and bot PRs cannot trigger workflows** — so the checks only ran post-tag.

Fixed by authoring the bump PR with an elevated token (`release-please.yml` → `with.token`), so it triggers `preflight-publish.yml` + `ci.yml` on its own head SHA before merge. **Operational requirement: `preflight-publish` must be a _Required_ status check in branch protection for the `main` / release-please branch** — that is what makes a red preflight *block the merge* (and therefore the tag) rather than just annotate it. Detection without the Required check is not prevention.

## Recovery runbooks

### A registry failed mid-release

The pipeline is idempotent — re-run is safe.

- **crates.io**: `publish_crate.sh` treats "already uploaded" as success only when the local `.crate` sha256 matches the registry cksum. Mismatch = hard fail. Workaround: bump patch version.
- **PyPI**: `twine upload --skip-existing` ignores duplicates.
- **npm**: idempotent publish — checks `npm view <pkg>@<v>` first.
- **Docker**: GHCR is content-addressed; rebuild + push idempotent.
- **MCP Registry**: re-run `mcp-publisher publish`.
- **APT/Homebrew**: re-trigger via `gh workflow run release.yml --ref main -f tag=vX.Y.Z`.

### Temporarily disabling npm publishing

npm publishing uses **trusted publishing** (OIDC): no token secret, nothing to
rotate. Each package on npmjs.com lists this repository and `release.yml` as its
trusted publisher (package page > Settings > Trusted Publisher > GitHub Actions:
user `us`, repository `crw`, workflow filename `release.yml`, no environment).
The packages are `crw-mcp`, `crw-mcp-darwin-arm64`, `crw-mcp-darwin-x64`,
`crw-mcp-linux-arm64`, `crw-mcp-linux-x64` and `crw-sdk`. A new platform
package needs that entry once before its first release. If npm publishing has
to be paused, set the repo variable `SKIP_NPM=true`
(`gh variable set SKIP_NPM --body true`): `publish-npm` and `publish-npm-sdk` are
then cleanly **skipped** (gray, not failed) and `verify-publish` records npm as
`skipped`. Re-enable with `gh variable delete SKIP_NPM`.

### A version was published with broken metadata (e.g. wrong npm optionalDeps)

crates.io and npm versions are **immutable**. Do not try to overwrite. Instead:

1. `cargo yank --version X.Y.Z <crate>` (per crate, if you must — yanked versions still work but stop new resolves).
2. `bash scripts/release/deprecate_npm_versions.sh X.Y.Z "<reason>"` to mark all 7 npm packages deprecated.
3. Land fixes on `main`. Conventional commits → release-please opens X.Y.Z+1 PR. Merge it.
4. Verify the new pipeline run leaves green checkmarks across every registry in the audit log.

### v0.4.0–v0.6.0 absence on crates.io

These tags were cut while the release pipeline silently failed (cargo publish output piped to `tee` which always returned 0). They are **not on crates.io and never will be** — crates.io is immutable. v0.6.1 is the first canonical 0.6.x release on crates.io. Earlier versions remain available on GitHub releases / npm / Docker / PyPI under their published artifacts (npm 0.6.0 has the broken optionalDeps pin and is deprecated; install `crw-mcp@^0.6.1`).

## Secret rotation

| Secret               | Used by                          | Rotation                                     |
| -------------------- | -------------------------------- | -------------------------------------------- |
| `CARGO_REGISTRY_TOKEN` | `publish-crates`               | crates.io account → API tokens → revoke + create new with `publish-update` scope |
| `PYPI_TOKEN`         | `publish-pypi`                   | pypi.org account → API tokens → scoped to project `crw` |
| npm (no secret)      | `publish-npm`, `publish-npm-sdk` | trusted publishing, configured per package on npmjs.com; nothing to rotate |
| `GH_DISPATCH_PAT`    | `dispatch-release`, `update-apt`, `update-homebrew` | GitHub fine-grained PAT with `actions:write` on this repo + `fastcrw/apt-crw` + `fastcrw/homebrew-crw` |

`GITHUB_TOKEN` is auto-provisioned and does not need rotation.

## Adding a new publishable crate

1. Add the crate to the workspace as usual (no `publish` field, or `publish = true`).
2. Decide its tier: which tier-N crates does it depend on? Add it to the next tier in `scripts/release/release_manifest.toml`.
3. If other crates depend on it, add **one** entry to the root `[workspace.dependencies]` table (`<crate> = { path = "crates/<crate>", version = "<workspace version>" }`) and have every consumer inherit it via `<crate> = { workspace = true }` (layer `features` / `optional` per-member; **never** inline `version =` in a member manifest). Add the matching `release-please-config.json` extra-file `$.workspace.dependencies.<crate>.version`. Run `bash scripts/check-internal-dep-versions.sh` and `uv run python scripts/release/audit_release_please_config.py` — both fail red if the pin drifts or its config entry is missing. (npm platform packages are discovered by glob, so adding one needs no guard edit.)
4. **First-publish ownership:** the user behind `CARGO_REGISTRY_TOKEN` becomes the initial owner. Verify the crate name is unclaimed — `curl -s https://crates.io/api/v1/crates/<name>` should return `{"errors":[{"detail":"Not Found"}]}`. After the first publish, optionally `cargo owner --add github:us:release-bots <name>`.
5. Open a PR. `preflight-publish.yml` runs the full workspace check — green means the crate is publishable.

## Adding a new internal (non-published) crate

1. `[package] publish = false` in its Cargo.toml.
2. Add to `scripts/release/release_manifest.toml` under `[unpublished]`.
3. Preflight enforces both must agree.
