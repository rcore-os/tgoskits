#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/env.sh
source "$ROOT/scripts/env.sh"

archive_override=""

usage() {
  cat <<'USAGE'
Usage: scripts/setup.sh [--archive PATH]

Prepare the SG2002 validator build environment:
  - install the Rust riscv64gc-unknown-linux-musl target
  - download, verify, and extract the Xuantie V3.4.0 musl toolchain

Options:
  --archive PATH  use a local toolchain archive instead of downloading
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive)
      archive_override="${2:-}"
      [[ -n "$archive_override" ]] || { echo "error: --archive needs a path" >&2; exit 2; }
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if ! rustup target list --installed | grep -qx "$AKARS_TENNIS_TARGET"; then
  rustup target add "$AKARS_TENNIS_TARGET"
fi

if akars_tennis_toolchain_ready; then
  echo "toolchain already installed: $AKARS_TENNIS_TOOLCHAIN_DIR"
  exit 0
fi

mkdir -p "$AKARS_TENNIS_TOOLCHAINS_DIR/.cache"
archive="$AKARS_TENNIS_TOOLCHAINS_DIR/.cache/$AKARS_TENNIS_TOOLCHAIN_ARCHIVE"

if [[ -n "$archive_override" ]]; then
  cp "$archive_override" "$archive"
elif [[ ! -f "$archive" ]]; then
  curl -fL --retry 3 --output "$archive.download" "$AKARS_TENNIS_TOOLCHAIN_URL"
  mv "$archive.download" "$archive"
fi

printf '%s  %s\n' "$AKARS_TENNIS_TOOLCHAIN_SHA256" "$archive" | sha256sum -c -

tmp_dir="$AKARS_TENNIS_TOOLCHAINS_DIR/.extract.$$"
rm -rf "$tmp_dir" "$AKARS_TENNIS_TOOLCHAIN_DIR"
mkdir -p "$tmp_dir"
tar -xzf "$archive" -C "$tmp_dir"
mv "$tmp_dir/$AKARS_TENNIS_TOOLCHAIN_EXTRACTED" "$AKARS_TENNIS_TOOLCHAIN_DIR"
rm -rf "$tmp_dir"

akars_tennis_toolchain_ready || {
  echo "error: installed toolchain is incomplete: $AKARS_TENNIS_TOOLCHAIN_DIR" >&2
  exit 1
}

"$AKARS_TENNIS_CC" --version
echo "setup complete"
