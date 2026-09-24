#!/usr/bin/env bash
# Verify the MCP registry has io.github.fastcrw/crw at the expected version.
#
# Usage: verify_mcp_registry.sh <version> [server_name=io.github.fastcrw/crw]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

v="${1:?version required}"
name="${2:-io.github.fastcrw/crw}"

# Ask for the ONE version we just published. `search=` alone is still paginated
# (30 per page, oldest first), so it returns our earliest 30 releases and the new
# one is never on page 1 — the check then fails for a version that IS published.
# It held only while we had <=30 releases: v0.25.0 passed as the 30th and last row
# on page 1, and v0.25.1 became the 31st and failed. `limit=` would just move the
# cliff to 100 (the cap). Filtering by version is O(1) and cannot page out.
end=$((SECONDS + 180))
while (( SECONDS < end )); do
  if curl -fsSL "https://registry.modelcontextprotocol.io/v0/servers?search=$name&version=$v" 2>/dev/null \
      | jq -e --arg n "$name" --arg v "$v" \
        'any(.servers[]?; .server.name == $n and .server.version == $v)' >/dev/null 2>&1; then
    notice "mcp-registry $name@$v present"
    exit 0
  fi
  sleep 10
done
die "mcp-registry $name@$v not visible after 3min"
