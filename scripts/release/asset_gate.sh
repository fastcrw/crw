#!/usr/bin/env bash
# Verify the GitHub Release for $tag has all 18 expected binary assets
# (3 binaries × 6 platforms), then publish SHA256SUMS and its signature.
# Runs after publish-binaries, gates the downstream registry publishes.
#
# Uploads SHA256SUMS + SHA256SUMS.sigstore.json to the release; idempotent.
#
# Usage: asset_gate.sh <tag>
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

tag="${1:?tag required (e.g. v0.6.1)}"

expected=()
for plat_arch in darwin-x64 darwin-arm64 linux-x64 linux-arm64; do
  for bin in crw crw-server crw-mcp; do
    expected+=("${bin}-${plat_arch}.tar.gz")
  done
done
for plat_arch in win32-x64 win32-arm64; do
  for bin in crw crw-server crw-mcp; do
    expected+=("${bin}-${plat_arch}.zip")
  done
done

mapfile -t actual < <(gh release view "$tag" --json assets -q '.assets[].name')

fail=0
for e in "${expected[@]}"; do
  if printf '%s\n' "${actual[@]}" | grep -qx "$e"; then
    printf '✓ %s\n' "$e"
  else
    printf '❌ missing: %s\n' "$e"
    fail=1
  fi
done
[ "$fail" -eq 0 ] || exit "$fail"

# SHA256SUMS goes up before signing: the wrappers need the checksums, not the
# signature, so a Sigstore outage must not leave an uninstallable release. A
# failed signature still fails the job, so it cannot go unnoticed.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for name in "${expected[@]}"; do
  gh release download "$tag" -D "$work" -p "$name"
done

# `gh release download` verifies nothing, so a truncated fetch would otherwise be
# hashed, signed and published as authoritative. Check what we downloaded against
# the digests GitHub reports. A missing digest is fatal: silently skipping the
# check is how signing stayed broken for eleven releases.
gh release view "$tag" --json assets \
  -q '.assets[] | select(.digest != null and .digest != "")
      | "\(.digest|ltrimstr("sha256:"))  \(.name)"' > "$work/API_SUMS"
[ -s "$work/API_SUMS" ] \
  || { echo "::error::gh reported no asset digests at all; this runner's gh may predate the field"; exit 1; }
for name in "${expected[@]}"; do
  awk -v n="$name" '$2 == n { found = 1 } END { exit !found }' "$work/API_SUMS" \
    || { echo "::error::release reports no digest for ${name}"; exit 1; }
done
( cd "$work" && sha256sum -c --ignore-missing --quiet API_SUMS )

# Hash the bytes we downloaded, never the API's digest: signing GitHub's claim
# about bytes we never observed would be a strictly weaker attestation.
# Generated from inside $work so entries are bare basenames for `sha256sum -c`.
( cd "$work" && sha256sum "${expected[@]}" > SHA256SUMS )
gh release upload "$tag" "$work/SHA256SUMS" --clobber
printf '✓ SHA256SUMS (%d assets)\n' "${#expected[@]}"

for attempt in 1 2 3 4; do
  cosign sign-blob --yes \
    --bundle "$work/SHA256SUMS.sigstore.json" "$work/SHA256SUMS" && break
  if [ "$attempt" -eq 4 ]; then
    echo "::error::cosign sign-blob failed after 4 attempts"
    exit 1
  fi
  echo "::warning::cosign attempt ${attempt}/4 failed; retrying in $((attempt * 20))s"
  sleep "$((attempt * 20))"
done

# Verify what we just signed, in the job that already holds the OIDC token.
# The identity pattern below is also published in .github/SECURITY.md; keep the
# two in step, or the docs hand users a pattern CI no longer enforces.
# This is the check that would have caught the v0.22.0 output-contract break on
# the first release instead of eleven later, and it exercises the identity
# regexp published in SECURITY.md.
cosign verify-blob --bundle "$work/SHA256SUMS.sigstore.json" \
  --certificate-identity-regexp '^https://github\.com/(us|fastcrw)/crw/\.github/workflows/release\.yml@refs/(tags/v.*|heads/main)$' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$work/SHA256SUMS"

gh release upload "$tag" "$work/SHA256SUMS.sigstore.json" --clobber
printf '✓ signature uploaded and verified\n'
