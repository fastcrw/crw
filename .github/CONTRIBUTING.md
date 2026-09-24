# Contributing to fastCRW

Thanks for your interest in making fastCRW better. This document covers everything
you need to go from zero to a merged pull request.

---

## Table of contents

- [Prerequisites](#prerequisites)
- [Building the workspace](#building-the-workspace)
- [Running checks locally](#running-checks-locally)
- [Git hooks](#git-hooks)
- [TypeScript SDK](#typescript-sdk)
- [Commit style (conventional commits)](#commit-style-conventional-commits)
- [Pull request process](#pull-request-process)
- [Additional scripts](#additional-scripts)

---

## Prerequisites

| Tool | Minimum version | Install |
|------|----------------|---------|
| Rust + Cargo | stable (edition 2024) | `curl https://sh.rustup.rs -sSf \| sh` |
| rustfmt | bundled with rustup | `rustup component add rustfmt` |
| clippy | bundled with rustup | `rustup component add clippy` |
| Python 3 | 3.9+ | only needed for release scripts |
| Node.js | 18 or 20 | only needed for the TypeScript SDK |

Clone the repo and you are ready to go — no Docker, no Redis, no external services
required for the core Rust build.

---

## Building the workspace

```bash
# Full workspace build (all crates, all targets)
cargo build --workspace --all-targets

# Or via Make
make build
```

The workspace members are declared in `Cargo.toml` at the repo root:
`crw-core`, `crw-diff`, `crw-renderer`, `crw-extract`, `crw-crawl`, `crw-search`,
`crw-server`, `crw-mcp`, `crw-mcp-proto`, `crw-browse`, `crw-cli`.

> **Note:** CI does not run a standalone `cargo build` step. Clippy (run with
> `--all-targets -D warnings`) compile-checks every target including benchmarks and
> examples, and `cargo test` builds all test targets. A redundant dev-profile build
> is therefore skipped in CI — but it is useful locally to get fast compiler errors
> before running the full check suite.

---

## Running checks locally

The `check` target mirrors what CI runs. Always pass it before opening a PR.

```bash
make check          # fmt-check + clippy + tests (same as CI)

# Individual steps
make fmt-check      # cargo fmt --all -- --check
make fmt            # cargo fmt --all  (auto-fix)
make clippy         # cargo clippy --workspace --all-targets -- -D warnings
make test           # cargo test --workspace
```

CI also runs several lightweight guard scripts:

```bash
bash scripts/check-no-process-exit.sh        # no unguarded process::exit in browser code
bash scripts/check-internal-dep-versions.sh  # internal dep pins match workspace version
python3 scripts/release/audit_release_please_config.py
python3 scripts/release/test_guards.py
```

These are fast enough to run locally before pushing.

---

## Git hooks

A pre-commit hook is provided that runs `fmt-check`, `clippy`, `check-no-process-exit.sh`, and `cargo test`
only when Rust or TOML files are staged.

```bash
make hooks   # copies scripts/pre-commit → .git/hooks/pre-commit
```

The hook exits early (no-op) when no Rust/TOML files are staged, so it does not
slow down documentation-only commits.

---

## TypeScript SDK

The TypeScript SDK lives in `sdks/typescript/` and is tested separately in CI
against Node.js 18 and 20.

```bash
cd sdks/typescript
npm ci
npm run build
npm test
```

---

## Commit style (conventional commits)

All commits merged to `main` must follow [Conventional Commits](https://www.conventionalcommits.org/).
Release-please reads these to determine version bumps and to generate the changelog
automatically — manual version edits and manual changelog edits are never needed.

| Prefix | Effect |
|--------|--------|
| `feat:` | MINOR bump — new feature |
| `fix:` | PATCH bump — bug fix |
| `feat!:` / `fix!:` / `BREAKING CHANGE:` footer | MAJOR bump |
| `chore:`, `docs:`, `refactor:`, `test:`, `ci:`, `perf:` | no version bump |

**Rules:**
- Subject line: 72 characters max, imperative mood, English.
- Body and footer are optional but welcome for non-trivial changes.
- Scopes are optional: `feat(mcp):`, `fix(renderer):`.

Example:

```
feat(mcp): add crw_parse_file tool to MCP server

Exposes the /v2/parse endpoint via the MCP protocol so AI agents can
convert arbitrary file URLs to Markdown without a separate HTTP call.
```

---

## Pull request process

1. **Fork and branch** — create a feature branch with a descriptive name:
   `feat/short-description` or `fix/short-description`.

2. **Run `make check`** — all three steps (fmt, clippy, test) must pass locally
   before you push.

3. **Open the PR against `main`** — fill in the PR template. Link any related issue.

4. **CI must be green** — the `check` and `sdk-ts` jobs in CI are required. A PR
   with a red CI will not be reviewed.

5. **Sign the CLA** — on your first PR a bot asks you to sign our
   [Contributor License Agreement](../CLA.md). It takes one comment and covers all
   of your future contributions. PRs can't be merged until it's signed.

6. **One approval required** — a maintainer will review. Address feedback by pushing
   new commits (no force-pushes to open PRs, please).

7. **Merge** — maintainers merge with rebase-and-merge to keep a linear history.

8. **Releases** — release-please opens a Release PR automatically as commits
   accumulate on `main`. Merging that PR creates the tag and GitHub Release. You
   do not need to do anything version-related.

---

## Additional scripts

| Script | Purpose |
|--------|---------|
| `scripts/pre-commit` | Git pre-commit hook (install via `make hooks`) |
| `scripts/check-no-process-exit.sh` | Guards against unguarded `process::exit` in browser-spawning code |
| `scripts/check-internal-dep-versions.sh` | Ensures internal dep pins match the workspace version |
| `scripts/check-mcp-example-json.sh` | Validates MCP example JSON |
| `scripts/check-openapi.sh` | Validates the OpenAPI spec |
| `scripts/sync-docs-changelog.py` | Syncs CHANGELOG into docs (`make sync-docs-changelog`) |

---

## Questions?

Open a [Discussion](https://github.com/fastcrw/crw/discussions) or join the
[Discord](https://discord.gg/kkFh2SC8). For cloud-specific questions, visit
[fastcrw.com](https://fastcrw.com).
