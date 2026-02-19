#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/release-gh.sh <tag> [--targets t1,t2,...] [--notes-file path] [--append] [--skip-build]

Examples:
  scripts/release-gh.sh v0.2.0
  scripts/release-gh.sh v0.2.0 --targets aarch64-apple-darwin,x86_64-apple-darwin
  scripts/release-gh.sh v0.2.0 --append --skip-build

Notes:
  - Requires: cargo, gh, git, tar, and either shasum or sha256sum.
  - By default, builds only the host Rust target.
  - Use --append when uploading additional platform artifacts to an existing release.
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

sha256_file() {
  local file="$1"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  else
    echo "No sha256 tool found (need shasum or sha256sum)" >&2
    exit 1
  fi
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -eq 0 ]]; then
  usage
  exit 0
fi

TAG="$1"
shift

TARGETS=""
NOTES_FILE=""
APPEND="false"
SKIP_BUILD="false"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --targets)
      TARGETS="${2:-}"
      shift 2
      ;;
    --notes-file)
      NOTES_FILE="${2:-}"
      shift 2
      ;;
    --append)
      APPEND="true"
      shift
      ;;
    --skip-build)
      SKIP_BUILD="true"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-].+)?$ ]]; then
  echo "Tag must look like vX.Y.Z (example: v0.2.0). Got: $TAG" >&2
  exit 1
fi

require_cmd cargo
require_cmd gh
require_cmd git
require_cmd tar

if ! git rev-parse --show-toplevel >/dev/null 2>&1; then
  echo "Not inside a git repository." >&2
  exit 1
fi

if ! gh auth status >/dev/null 2>&1; then
  echo "GitHub CLI is not authenticated. Run: gh auth login" >&2
  exit 1
fi

if [[ -n "$NOTES_FILE" && ! -f "$NOTES_FILE" ]]; then
  echo "Notes file not found: $NOTES_FILE" >&2
  exit 1
fi

if [[ -z "$TARGETS" ]]; then
  HOST_TARGET="$(rustc -vV | awk '/^host: / {print $2}')"
  TARGETS="$HOST_TARGET"
fi

IFS=',' read -r -a TARGET_ARRAY <<< "$TARGETS"
if [[ ${#TARGET_ARRAY[@]} -eq 0 ]]; then
  echo "No targets to build/upload." >&2
  exit 1
fi

VERSION_NO_V="${TAG#v}"
DIST_DIR="dist/${TAG}"
mkdir -p "$DIST_DIR"

ARTIFACTS=()

for target in "${TARGET_ARRAY[@]}"; do
  target="$(echo "$target" | xargs)"
  if [[ -z "$target" ]]; then
    continue
  fi

  if [[ "$SKIP_BUILD" == "false" ]]; then
    if ! rustup target list --installed | grep -q "^${target}$"; then
      echo "Installing missing rust target: $target"
      rustup target add "$target"
    fi

    echo "Building ratmail for $target"
    cargo build --release --locked -p ratmail --target "$target"
  fi

  BIN_PATH="target/${target}/release/ratmail"
  if [[ ! -f "$BIN_PATH" ]]; then
    echo "Binary not found: $BIN_PATH" >&2
    echo "If this target is built elsewhere, copy artifacts first or remove --skip-build." >&2
    exit 1
  fi

  PKG_DIR="ratmail-${VERSION_NO_V}-${target}"
  PKG_ROOT="${DIST_DIR}/${PKG_DIR}"
  rm -rf "$PKG_ROOT"
  mkdir -p "$PKG_ROOT"
  cp "$BIN_PATH" "${PKG_ROOT}/ratmail"
  chmod +x "${PKG_ROOT}/ratmail"

  TAR_NAME="${PKG_DIR}.tar.gz"
  TAR_PATH="${DIST_DIR}/${TAR_NAME}"
  tar -C "$DIST_DIR" -czf "$TAR_PATH" "$PKG_DIR"
  ARTIFACTS+=("$TAR_PATH")
done

if [[ ${#ARTIFACTS[@]} -eq 0 ]]; then
  echo "No artifacts were produced." >&2
  exit 1
fi

CHECKSUMS_PATH="${DIST_DIR}/checksums.txt"
{
  for artifact in "${ARTIFACTS[@]}"; do
    digest="$(sha256_file "$artifact")"
    file_name="$(basename "$artifact")"
    echo "${digest}  ${file_name}"
  done
} > "$CHECKSUMS_PATH"
ARTIFACTS+=("$CHECKSUMS_PATH")

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "Release $TAG exists. Uploading artifacts."
  gh release upload "$TAG" "${ARTIFACTS[@]}" --clobber
else
  echo "Creating release $TAG and uploading artifacts."
  if [[ -n "$NOTES_FILE" ]]; then
    gh release create "$TAG" "${ARTIFACTS[@]}" --title "Ratmail $TAG" --notes-file "$NOTES_FILE"
  else
    gh release create "$TAG" "${ARTIFACTS[@]}" --title "Ratmail $TAG" --generate-notes
  fi
fi

if [[ "$APPEND" == "true" ]]; then
  echo "--append was provided. Existing release flow is already append-safe via upload --clobber."
fi

echo
echo "Published assets:"
for artifact in "${ARTIFACTS[@]}"; do
  echo "  - $(basename "$artifact")"
done
echo "Local output dir: $DIST_DIR"
