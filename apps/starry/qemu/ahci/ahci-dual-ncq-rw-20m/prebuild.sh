#!/bin/sh
set -eu

data_dir="$STARRY_WORKSPACE/tmp/axbuild/ahci-dual-ncq-rw-20m-data"
mkdir -p "$data_dir"
find "$data_dir" -mindepth 1 -maxdepth 1 -delete

mkdir -p "$STARRY_OVERLAY_DIR/etc"
printf '%s\n' "AHCI dual-disk test asset" > "$STARRY_OVERLAY_DIR/etc/ahci-dual-test"
