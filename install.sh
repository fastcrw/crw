#!/bin/sh
# CRW installer. Downloads the latest release binary for your platform.
# Usage:
#   curl -fsSL https://fastcrw.com/install | sh
#   wget -qO- https://fastcrw.com/install | sh
#
# Options (environment variables):
#   CRW_VERSION=v0.3.0    Install a specific version instead of latest
#   CRW_INSTALL_DIR=~/.local/bin   Custom install directory
#   CRW_BINARY=crw-mcp    Install crw-mcp (MCP server) or crw-server instead of
#                         the default crw CLI
#   CRW_API_KEY=crw_live_…  Connect to CRW Cloud and register the MCP server
#                         with every detected AI coding tool, in one command
#   CRW_NO_AGENTS=1       With CRW_API_KEY, skip the AI-tool registration

set -eu

main() {

REPO="fastcrw/crw"
INSTALL_DIR="${CRW_INSTALL_DIR:-/usr/local/bin}"
BINARY="${CRW_BINARY:-crw}"

# --- helpers ----------------------------------------------------------------

BOLD="$(tput bold 2>/dev/null || printf '')"
BLUE="$(tput setaf 4 2>/dev/null || printf '')"
GREEN="$(tput setaf 2 2>/dev/null || printf '')"
RED="$(tput setaf 1 2>/dev/null || printf '')"
RESET="$(tput sgr0 2>/dev/null || printf '')"

info()      { printf '%s==>%s %s\n' "${BLUE}${BOLD}" "${RESET}" "$*"; }
success()   { printf '%s==>%s %s\n' "${GREEN}${BOLD}" "${RESET}" "$*"; }
err()       { printf '%serror:%s %s\n' "${RED}${BOLD}" "${RESET}" "$*" >&2; exit 1; }

need() {
  command -v "$1" >/dev/null 2>&1 || err "'$1' is required but not found"
}

# Print a binary's version line, or nothing. Only the `crw` CLI has a --version
# flag; crw-mcp and crw-server reject it, so callers must treat an empty result
# as "unknown" and fall back to the plain binary name.
version_of() {
  "$1" --version 2>/dev/null | head -1
}

# --- detect downloader ------------------------------------------------------

download() {
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error \
         --proto '=https' --tlsv1.2 \
         --output "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget --https-only --quiet --output-document="$2" "$1"
  else
    err "curl or wget is required"
  fi
}

# --- detect platform --------------------------------------------------------

detect_platform() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"

  case "$OS" in
    Darwin)  PLATFORM="darwin" ;;
    Linux)   PLATFORM="linux"  ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM="win32" ;;
    *)       err "Unsupported OS: $OS. Try: cargo install $BINARY" ;;
  esac

  # Rosetta 2 detection: uname returns x86_64 under Rosetta on Apple Silicon
  if [ "$PLATFORM" = "darwin" ] && [ "$ARCH" = "x86_64" ]; then
    if sysctl -n sysctl.proc_translated 2>/dev/null | grep -q '^1$'; then
      info "Rosetta 2 detected, installing native arm64 binary"
      ARCH="arm64"
    fi
  fi

  case "$ARCH" in
    x86_64|amd64)  ARCH_LABEL="x64"   ;;
    aarch64|arm64) ARCH_LABEL="arm64"  ;;
    *)             err "Unsupported architecture: $ARCH. Try: cargo install $BINARY" ;;
  esac

  # Linux binaries are static musl builds, so they run on ANY libc (glibc and
  # musl/Alpine alike), so no libc gate is needed.

  if [ "$PLATFORM" = "win32" ]; then
    ASSET="${BINARY}-${PLATFORM}-${ARCH_LABEL}.zip"
  else
    ASSET="${BINARY}-${PLATFORM}-${ARCH_LABEL}.tar.gz"
  fi
}

# --- pick the download URL ----------------------------------------------------

# Unpinned, we hand the download straight to GitHub's own
# `releases/latest/download/<asset>` redirect instead of first asking the REST
# API which tag is latest. That API allows 60 unauthenticated requests per hour
# per IP, so on any shared address (CI runners, an office NAT, a cloud VM) the
# tag lookup was what failed, never the download itself. The redirect is not
# rate limited and needs no token.
pick_url() {
  if [ -n "${CRW_VERSION:-}" ]; then
    VERSION="$CRW_VERSION"
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
  else
    # Empty means "whatever the redirect gives us"; messages say "latest" and
    # the real tag is read back off the binary once it is installed.
    VERSION=""
    URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
  fi
}

# --- download & install -----------------------------------------------------

install() {
  CRW_TMPDIR="$(mktemp -d)"
  trap 'rm -rf "$CRW_TMPDIR"' EXIT

  # Check for existing installation
  if command -v "$BINARY" >/dev/null 2>&1; then
    INSTALLED="$(version_of "$BINARY")"
    info "Upgrading ${INSTALLED:-$BINARY} to ${VERSION:-latest}..."
  else
    info "Downloading ${BINARY} ${VERSION:-latest} (${PLATFORM}/${ARCH_LABEL})..."
  fi

  download "$URL" "${CRW_TMPDIR}/${ASSET}"

  info "Extracting..."
  if [ "$PLATFORM" = "win32" ]; then
    need unzip
    unzip -o "${CRW_TMPDIR}/${ASSET}" -d "$CRW_TMPDIR" >/dev/null
  else
    tar xzf "${CRW_TMPDIR}/${ASSET}" -C "$CRW_TMPDIR"
  fi

  # Verify the binary was extracted
  [ -f "${CRW_TMPDIR}/${BINARY}" ] || err "Archive did not contain '${BINARY}'"

  # Create install directory if needed
  if [ ! -d "$INSTALL_DIR" ]; then
    if [ -w "$(dirname "$INSTALL_DIR")" ]; then
      mkdir -p "$INSTALL_DIR"
    else
      sudo mkdir -p "$INSTALL_DIR"
    fi
  fi

  info "Installing to ${INSTALL_DIR}/${BINARY}..."
  if [ -w "$INSTALL_DIR" ]; then
    mv "${CRW_TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    chmod +x "${INSTALL_DIR}/${BINARY}"
  else
    sudo mv "${CRW_TMPDIR}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    sudo chmod +x "${INSTALL_DIR}/${BINARY}"
  fi

  # With the `latest` redirect the tag is never known up front, so report the
  # version the installed binary actually is rather than the label we asked for.
  INSTALLED_VERSION="$(version_of "${INSTALL_DIR}/${BINARY}")"
  success "${INSTALLED_VERSION:-$BINARY} installed to ${INSTALL_DIR}/${BINARY}"
  echo ""

  # Check if install dir is in PATH
  case ":$PATH:" in
    *":${INSTALL_DIR}:"*) ;;
    *) echo "  Note: ${INSTALL_DIR} is not in your PATH. Add it with:"
       echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
       echo "" ;;
  esac

  # First-run onboarding (crw CLI only). Basic local scraping needs no setup,
  # so installation must never drop users into an unsolicited wizard. An API
  # key supplied explicitly is still a deliberate one-command cloud connect;
  # otherwise print the runnable first command and keep setup clearly optional.
  if [ "$BINARY" = "crw" ]; then
    if [ -n "${CRW_API_KEY:-}" ]; then
      # One-command cloud connect: `curl … | CRW_API_KEY=crw_live_… sh`.
      # Non-interactive: validates the key and writes config.toml, no /dev/tty
      # needed (works in CI too).
      echo ""
      info "Connecting to CRW Cloud with your API key…"
      echo ""
      # Registration into detected AI tools is part of the same command; the
      # pasted CRW_API_KEY is the consent a piped installer cannot prompt for.
      # CRW_NO_AGENTS=1 opts out.
      if [ -n "${CRW_NO_AGENTS:-}" ]; then
        "${INSTALL_DIR}/${BINARY}" setup --api-key "${CRW_API_KEY}" --no-agents \
          || info "Cloud connect failed. Run 'crw setup --api-key <key>' to retry."
      else
        "${INSTALL_DIR}/${BINARY}" setup --api-key "${CRW_API_KEY}" \
          || info "Cloud connect failed. Run 'crw setup --api-key <key>' to retry."
      fi
    else
      echo ""
      echo "  Run:       crw https://example.com"
      echo "  Optional:  crw setup    # connect Cloud or add local JS/search"
      echo "  Help:      crw --help"
      echo ""
    fi
  else
    echo ""
    echo "  Run:  ${BINARY} --help"
    echo ""
  fi
}

# --- run --------------------------------------------------------------------

detect_platform
pick_url
install

}

# main() wrapper ensures the entire script is downloaded before execution
main
