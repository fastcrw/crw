#!/usr/bin/env bash
# Verify that the APT/Homebrew dispatch was consumed and produced the artifact.
#
# We verify the ARTIFACTS directly (the real signal), polling because the
# downstream repos build asynchronously after the dispatch:
#   - apt-crw:      pool/crw_<version>_amd64.deb committed to the repo.
#   - homebrew-crw: Formula/crw.rb pinned to <version>.
#
# An earlier version also waited 30min for a `crw-release-<version>` commit
# status on each repo, but those repos don't post that status reliably, so it
# produced false failures even when the package shipped (and the apt check
# looked at a GitHub *release* asset, while apt-crw actually publishes the .deb
# by committing it to pool/). The committed artifact is the source of truth.
#
# Usage: verify_apt_homebrew.sh <version> [correlation_id]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

v="${1:?version required}"
: "${2:-}"  # correlation_id accepted for call compatibility; no longer used

apt_ok() {
  # contents API returns 200 if the committed .deb exists on the default branch.
  gh api "repos/fastcrw/apt-crw/contents/pool/crw_${v}_amd64.deb" >/dev/null 2>&1
}
brew_ok() {
  gh api "repos/fastcrw/homebrew-crw/contents/Formula/crw.rb" 2>/dev/null \
    | jq -r .content | base64 -d 2>/dev/null | grep -q "version \"$v\""
}

end=$((SECONDS + 1200))  # 20min for the async downstream builds
while (( SECONDS < end )); do
  if apt_ok && brew_ok; then
    printf '✓ apt-crw pool/crw_%s_amd64.deb present\n' "$v"
    printf '✓ homebrew-crw Formula/crw.rb pinned to %s\n' "$v"
    exit 0
  fi
  sleep 30
done

# Timed out — report which artifact is still missing.
fail=0
if apt_ok; then
  printf '✓ apt-crw pool/crw_%s_amd64.deb present\n' "$v"
else
  printf '❌ apt-crw pool/crw_%s_amd64.deb missing after 20min\n' "$v"
  fail=1
fi
if brew_ok; then
  printf '✓ homebrew-crw Formula/crw.rb pinned to %s\n' "$v"
else
  printf '❌ homebrew-crw Formula/crw.rb not pinned to %s after 20min\n' "$v"
  fail=1
fi
exit "$fail"
