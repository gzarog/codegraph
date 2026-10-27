#!/bin/sh
#
# CodeGraph installer: downloads the release binary for this machine,
# verifies its checksum and puts it on your PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh
#
# Upgrade:   codegraph upgrade   (or re-run this script)
# Uninstall: curl -fsSL .../install.sh | sh -s -- --uninstall
#
# Environment:
#   CODEGRAPH_VERSION  release tag to install (default: latest)
#   CODEGRAPH_BIN_DIR  where the binary goes (default: ~/.local/bin)
set -eu

REPO="gzarog/codegraph"
BIN_DIR="${CODEGRAPH_BIN_DIR:-$HOME/.local/bin}"

if [ "${1:-}" = "--uninstall" ]; then
  rm -f "$BIN_DIR/codegraph"
  echo "CodeGraph uninstalled (removed $BIN_DIR/codegraph)."
  echo "Agent configs are left as they are; run 'codegraph uninstall' first to remove those."
  exit 0
fi

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin) os="macos" ;;
  Linux)  os="linux" ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "codegraph: on Windows, run in PowerShell:" >&2
    echo "  irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex" >&2
    exit 1 ;;
  *) echo "codegraph: unsupported OS '$os'." >&2; exit 1 ;;
esac
case "$arch" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64)  arch="x86_64" ;;
  *) echo "codegraph: unsupported architecture '$arch'." >&2; exit 1 ;;
esac
asset="codegraph-$os-$arch.tar.gz"

# The releases/latest redirect isn't rate-limited like the API; fall back to the API.
version="${CODEGRAPH_VERSION:-}"
if [ -z "$version" ]; then
  version="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" \
    | sed -n 's#.*/releases/tag/##p')"
fi
if [ -z "$version" ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)"
fi
[ -n "$version" ] || { echo "codegraph: could not resolve the latest release; set CODEGRAPH_VERSION (e.g. v0.1.0)." >&2; exit 1; }
case "$version" in v*) ;; *) version="v$version" ;; esac

base="https://github.com/$REPO/releases/download/$version"
echo "Installing CodeGraph $version ($os-$arch)..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$base/$asset" -o "$tmp/$asset" || { echo "codegraph: download failed: $base/$asset" >&2; exit 1; }
curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || { echo "codegraph: could not download SHA256SUMS" >&2; exit 1; }

expected="$(awk -v f="$asset" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] || { echo "codegraph: checksum mismatch for $asset; not installing." >&2; exit 1; }

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$BIN_DIR"
install -m 0755 "$tmp/codegraph" "$BIN_DIR/codegraph.new"
mv -f "$BIN_DIR/codegraph.new" "$BIN_DIR/codegraph"
echo "Installed $BIN_DIR/codegraph"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Add $BIN_DIR to your PATH, e.g.: echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> ~/.profile" ;;
esac
echo "Next: run 'codegraph install' to connect your coding agents."
