#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/env.sh
source "$ROOT/scripts/env.sh"

toolchain_archive_override=""
sdk_archive_override=""

usage() {
  cat <<'USAGE'
Usage: scripts/setup.sh [--toolchain-archive PATH] [--sdk-archive PATH]

Prepare the SG2002 validator build environment:
  - install the Rust riscv64gc-unknown-linux-musl target
  - download, verify, and extract the Xuantie V3.4.0 musl toolchain
  - download, verify, and extract the Milk-V/Cvitek SG200x TPU SDK

Options:
  --toolchain-archive PATH  use a local toolchain archive instead of downloading
  --sdk-archive PATH        use a local TPU SDK archive instead of downloading
  --archive PATH            compatibility alias for --toolchain-archive
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive|--toolchain-archive)
      toolchain_archive_override="${2:-}"
      [[ -n "$toolchain_archive_override" ]] || { echo "error: $1 needs a path" >&2; exit 2; }
      shift
      ;;
    --sdk-archive)
      sdk_archive_override="${2:-}"
      [[ -n "$sdk_archive_override" ]] || { echo "error: --sdk-archive needs a path" >&2; exit 2; }
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

mkdir -p "$AKARS_TENNIS_TOOLCHAINS_DIR/.cache"

prepare_archive() {
  local url="$1"
  local sha256="$2"
  local archive_name="$3"
  local override="$4"
  local archive="$AKARS_TENNIS_TOOLCHAINS_DIR/.cache/$archive_name"

  if [[ -n "$override" ]]; then
    cp "$override" "$archive"
  elif [[ ! -f "$archive" ]]; then
    curl -fL --retry 3 --output "$archive.download" "$url"
    mv "$archive.download" "$archive"
  fi

  printf '%s  %s\n' "$sha256" "$archive" | sha256sum -c - >&2
  printf '%s\n' "$archive"
}

if akars_tennis_toolchain_ready; then
  echo "toolchain already installed: $AKARS_TENNIS_TOOLCHAIN_DIR"
else
  archive="$(
    prepare_archive \
      "$AKARS_TENNIS_TOOLCHAIN_URL" \
      "$AKARS_TENNIS_TOOLCHAIN_SHA256" \
      "$AKARS_TENNIS_TOOLCHAIN_ARCHIVE" \
      "$toolchain_archive_override"
  )"

  tmp_dir="$AKARS_TENNIS_TOOLCHAINS_DIR/.extract-toolchain.$$"
  rm -rf "$tmp_dir" "$AKARS_TENNIS_TOOLCHAIN_DIR"
  mkdir -p "$tmp_dir"
  tar -xzf "$archive" -C "$tmp_dir"
  mv "$tmp_dir/$AKARS_TENNIS_TOOLCHAIN_EXTRACTED" "$AKARS_TENNIS_TOOLCHAIN_DIR"
  rm -rf "$tmp_dir"
fi

if akars_tennis_tpu_sdk_ready; then
  echo "TPU SDK already installed: $AKARS_TPU_SDK_DIR"
else
  archive="$(
    prepare_archive \
      "$AKARS_TPU_SDK_URL" \
      "$AKARS_TPU_SDK_SHA256" \
      "$AKARS_TPU_SDK_ARCHIVE" \
      "$sdk_archive_override"
  )"

  tmp_dir="$AKARS_TENNIS_TOOLCHAINS_DIR/.extract-tpu-sdk.$$"
  rm -rf "$tmp_dir" "$AKARS_TPU_SDK_DIR"
  mkdir -p "$tmp_dir" "$(dirname "$AKARS_TPU_SDK_DIR")"
  tar -xzf "$archive" -C "$tmp_dir"
  mv "$tmp_dir/$AKARS_TPU_SDK_EXTRACTED" "$AKARS_TPU_SDK_DIR"
  rm -rf "$tmp_dir"
fi

akars_tennis_toolchain_ready || {
  echo "error: installed toolchain is incomplete: $AKARS_TENNIS_TOOLCHAIN_DIR" >&2
  exit 1
}

akars_tennis_tpu_sdk_ready || {
  echo "error: installed TPU SDK is incomplete: $AKARS_TPU_SDK_DIR" >&2
  exit 1
}

"$AKARS_TENNIS_CC" --version
echo "setup complete"
