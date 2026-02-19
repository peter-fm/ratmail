#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -lt 1 ]]; then
  cat <<'EOF'
Usage:
  scripts/linux-release.sh <tag> [extra release-gh args...]

Example:
  scripts/linux-release.sh v0.2.0 --append

This builds/uploads Linux artifacts for:
  - x86_64-unknown-linux-gnu
EOF
  exit 0
fi

TAG="$1"
shift

scripts/release-gh.sh "$TAG" --targets x86_64-unknown-linux-gnu "$@"
