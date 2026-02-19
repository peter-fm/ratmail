#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -lt 1 ]]; then
  cat <<'EOF'
Usage:
  scripts/macos-release.sh <tag> [extra release-gh args...]

Example:
  scripts/macos-release.sh v0.2.0

This builds/uploads macOS artifacts for:
  - aarch64-apple-darwin
  - x86_64-apple-darwin
EOF
  exit 0
fi

TAG="$1"
shift

scripts/release-gh.sh "$TAG" --targets aarch64-apple-darwin,x86_64-apple-darwin "$@"
