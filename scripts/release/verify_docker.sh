#!/usr/bin/env bash
# Verify ghcr.io/fastcrw/crw image exists for $version, latest, and major.minor,
# with both linux/amd64 and linux/arm64 manifests.
#
# Usage: verify_docker.sh <version> [image=ghcr.io/fastcrw/crw]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

v="${1:?version required}"
image="${2:-ghcr.io/fastcrw/crw}"
major_minor=$(printf '%s' "$v" | cut -d. -f1-2)

# Poll, for the same reason the npm checks do: a registry accepts the push
# before every tag is readable. The 0.36.0 audit read `latest` as missing while
# `$v` and `$major_minor`, pushed in the same operation, both passed, and
# `latest` resolved to the right digest minutes later. A single shot reports a
# healthy release as broken.
#
# The deadline is shared across tags rather than per tag, so a genuinely failed
# release is still reported in five minutes and not in fifteen.
deadline=$(( $(date +%s) + 300 ))
manifest_for() {
  local ref="$1" out=""
  while :; do
    out=$(docker manifest inspect "$ref" 2>/dev/null || echo "")
    [ -n "$out" ] && break
    [ "$(date +%s)" -ge "$deadline" ] && break
    sleep 10
  done
  printf '%s' "$out"
}

fail=0
for tag in "$v" "latest" "$major_minor"; do
  manifest=$(manifest_for "${image}:${tag}")
  if [ -z "$manifest" ]; then
    printf '❌ %s:%s missing\n' "$image" "$tag"
    fail=1
    continue
  fi
  for arch in amd64 arm64; do
    # Compare as `<os>/<arch>` to match docker conventions.
    if printf '%s' "$manifest" \
        | jq -e --arg a "linux/$arch" '
          .manifests[]?
          | (.platform.os + "/" + .platform.architecture) as $p
          | select($p == $a)
          ' >/dev/null 2>&1; then
      printf '✓ %s:%s linux/%s\n' "$image" "$tag" "$arch"
    else
      printf '❌ %s:%s missing linux/%s\n' "$image" "$tag" "$arch"
      fail=1
    fi
  done
done
exit "$fail"
