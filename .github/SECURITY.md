# Security Policy

## Supported versions

| Version | Supported |
|---------|-----------|
| Latest release (`main`) | Yes |
| Older releases | No — please upgrade |

We release frequently. The safest course is always to run the latest published
version from [crates.io](https://crates.io/crates/crw-server) or the latest
GitHub release.

---

## Reporting a vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Report privately through either channel:

- **GitHub Private Vulnerability Reporting** (preferred): open a report from the
  [Security tab](https://github.com/fastcrw/crw/security/advisories/new) of this
  repository. This routes straight to the maintainers and keeps the details private.
- **Email**: **security@fastcrw.com**

Include as much detail as you can:

- A description of the vulnerability and its potential impact.
- Steps to reproduce or a minimal proof-of-concept.
- The version(s) of fastCRW affected.
- Any suggested mitigations, if you have them.

We will acknowledge your report within **48 hours** and aim to ship a patch within
**14 days** for confirmed high-severity issues. We will keep you updated throughout
the process and credit you in the release notes (unless you prefer to stay
anonymous).

---

## Disclosure process

1. You email `security@fastcrw.com` with the details.
2. We confirm receipt and open a private tracking issue.
3. We reproduce the issue, assess severity, and develop a fix.
4. We coordinate a disclosure date with you.
5. We publish a patched release and a public advisory on the same day.

We follow a **90-day coordinated disclosure** window by default. If a vulnerability
is already being actively exploited, we may move faster.

---

## Verifying a release

Every release publishes a `SHA256SUMS` file covering all binary assets, plus a
keyless [Sigstore](https://www.sigstore.dev/) signature over that file
(`SHA256SUMS.sigstore.json`).

The Python SDK, and the npm launcher on Windows, download a prebuilt binary on
first use. Before extracting it they fetch `SHA256SUMS` for their own release
tag and compare it against the archive they received, and they abort if it does
not match or if the release publishes no checksums at all. They ask only for the
tag they were published alongside, never a mutable `latest`.

On macOS and Linux, `npm install crw-mcp` normally pulls a prebuilt platform
package instead, so the download path is usually unused there. Those binaries are
checked against the same `SHA256SUMS` in CI before the packages are published,
and npm's own integrity hash covers the install. An install that skips optional
dependencies still falls back to the verified download path.

Two limits worth stating plainly:

- Once a binary is in the local cache, later runs execute it without re-hashing.
  Anything that can write to your cache directory can therefore substitute it.
  Closing that properly needs a digest baked into the wrapper at build time,
  which we do not do yet.
- `SHA256SUMS` is fetched from the same release as the archive, so this protects
  against transfer corruption, truncation, and a mismatched or re-uploaded
  asset. It is not a defence against someone who can write to the release
  itself; that is what the signature is for, and verifying it is a manual step
  today.

To check a download yourself (requires cosign v3 or newer for step 2):

```sh
# 1. Integrity: the archive matches what the release records.
curl -fsSLO https://github.com/fastcrw/crw/releases/download/vX.Y.Z/SHA256SUMS
sha256sum -c SHA256SUMS --ignore-missing
# macOS has no sha256sum; check just the file you downloaded:
#   shasum -a 256 crw-darwin-arm64.tar.gz
#   grep crw-darwin-arm64.tar.gz SHA256SUMS

# 2. Authenticity: the checksums were produced by our release workflow.
curl -fsSLO https://github.com/fastcrw/crw/releases/download/vX.Y.Z/SHA256SUMS.sigstore.json
cosign verify-blob \
  --bundle SHA256SUMS.sigstore.json \
  --certificate-identity-regexp '^https://github\.com/(us|fastcrw)/crw/\.github/workflows/release\.yml@refs/(tags/v.*|heads/main)$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  SHA256SUMS
```

The identity is a regexp because keyless signing binds the certificate to the
workflow ref: normally the release tag, or `main` when a release is re-run
manually for recovery.

Note on older releases: `v0.21.2` and `v0.21.3` carry per-asset `.sig`/`.pem`
files instead, and releases from `v0.22.0` through `v0.27.0` are unsigned. The
cosign version is now pinned explicitly.

---

## Scope

In scope for this policy:

- The `crw-server` binary and all workspace crates in this repository.
- The MCP server (`crw-mcp`).
- The TypeScript SDK (`sdks/typescript`).

Out of scope:

- The managed cloud platform at `fastcrw.com` and `api.fastcrw.com` — report
  cloud/SaaS issues to the same address; they are triaged separately.
- Third-party headless browsers (Chromium, Lightpanda) invoked by the renderer.
  Please report those upstream.

---

## License note

fastCRW is licensed under AGPL-3.0. The license does not limit your right to
report security vulnerabilities or receive fixes. If you are running fastCRW in a
context where the AGPL obligations are relevant to your deployment, please read
`LICENSE` and feel free to contact us at `security@fastcrw.com` with any questions.
