#!/bin/sh

# Aspens SDK installer — installs the `aspens-cli` and `aspens-repl` binaries
# from the latest GitHub release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/aspensprotocol/sdk/main/install.sh | sh
#
# Environment overrides:
#   INSTALL_DIR=/path       install location
#                           (default: /usr/local/bin on Linux, $HOME/.local/bin on macOS)
#   ASPENS_VERSION=v0.6.0   install a specific release tag (default: latest)

set -e

REPO="aspensprotocol/sdk"
BINARIES="aspens-cli aspens-repl"

# Detect OS + architecture and pick the install directory.
detect_platform() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"

  case "$OS" in
    Linux)  PLATFORM_OS="linux"; DEFAULT_INSTALL_DIR="/usr/local/bin" ;;
    Darwin) PLATFORM_OS="macos"; DEFAULT_INSTALL_DIR="$HOME/.local/bin" ;;
    *)
      echo "Error: unsupported operating system: $OS"
      exit 1
      ;;
  esac

  case "$ARCH" in
    x86_64|amd64|x64)           PLATFORM_ARCH="x86_64" ;;
    arm64|aarch64|armv8*|arm8*) PLATFORM_ARCH="aarch64" ;;
    *)
      echo "Error: unsupported architecture: $ARCH (only x86_64 or aarch64 are available)"
      exit 1
      ;;
  esac

  TARGET="${PLATFORM_OS}-${PLATFORM_ARCH}"
  INSTALL_DIR="${INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
}

# Resolve the release tag to install (explicit override, else latest).
resolve_version() {
  if [ -n "${ASPENS_VERSION:-}" ]; then
    VERSION="$ASPENS_VERSION"
    return
  fi
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | sed -E 's/.*"([^"]+)".*/\1/')
  if [ -z "$VERSION" ]; then
    echo "Error: could not determine the latest release version"
    exit 1
  fi
}

# Best-effort SHA256 check against the release SHA256SUMS. No-op when the
# checksums file or a sha256 tool is unavailable; hard-fails on a real mismatch.
verify_checksum() { # <file> <asset-name>
  file="$1"
  name="$2"
  [ -n "$SHA256SUMS" ] || return 0
  expected=$(awk -v n="$name" '$2 == n {print $1}' "$SHA256SUMS")
  [ -n "$expected" ] || return 0
  if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$file" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$file" | awk '{print $1}')
  else
    return 0
  fi
  if [ "$expected" != "$actual" ]; then
    echo "Error: checksum mismatch for ${name}"
    echo "  expected ${expected}"
    echo "  actual   ${actual}"
    exit 1
  fi
}

install() {
  detect_platform
  resolve_version

  echo "Installing aspens-cli + aspens-repl ${VERSION} for ${TARGET} -> ${INSTALL_DIR}"

  TMP_DIR=$(mktemp -d)
  trap 'rm -rf "$TMP_DIR"' EXIT

  # Fetch the release checksums (optional — verification is best-effort).
  SHA256SUMS=""
  if curl -fsSL "https://github.com/${REPO}/releases/download/${VERSION}/SHA256SUMS" \
       -o "${TMP_DIR}/SHA256SUMS" 2>/dev/null; then
    SHA256SUMS="${TMP_DIR}/SHA256SUMS"
  fi

  mkdir -p "$INSTALL_DIR"

  for bin in $BINARIES; do
    asset="${bin}-${TARGET}.tar.gz"
    url="https://github.com/${REPO}/releases/download/${VERSION}/${asset}"

    echo "Downloading ${asset}..."
    if ! curl -fsSL "$url" -o "${TMP_DIR}/${asset}"; then
      echo "Error: failed to download ${asset}"
      echo "Release assets may not be available for ${TARGET}."
      exit 1
    fi

    verify_checksum "${TMP_DIR}/${asset}" "$asset"

    tar -xzf "${TMP_DIR}/${asset}" -C "$TMP_DIR"
    chmod +x "${TMP_DIR}/${bin}"

    if [ -w "$INSTALL_DIR" ]; then
      mv "${TMP_DIR}/${bin}" "${INSTALL_DIR}/${bin}"
    else
      echo "Installing to ${INSTALL_DIR} requires sudo..."
      sudo mv "${TMP_DIR}/${bin}" "${INSTALL_DIR}/${bin}"
    fi
    echo "  installed ${INSTALL_DIR}/${bin}"
  done

  # PATH hint.
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      echo ""
      echo "Note: ${INSTALL_DIR} is not in your PATH. Add it to your shell profile:"
      echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
      ;;
  esac

  echo ""
  echo "Done. Run 'aspens-cli --help' or 'aspens-repl' to get started."
}

main() {
  echo "Aspens SDK installer"
  echo "===================="
  echo ""
  install
}

main "$@"
