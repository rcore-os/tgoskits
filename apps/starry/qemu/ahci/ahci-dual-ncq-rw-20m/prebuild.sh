#!/bin/sh
set -eu

data_dir="$STARRY_WORKSPACE/tmp/axbuild/ahci-dual-ncq-rw-20m-data"
mkdir -p "$data_dir"
find "$data_dir" -mindepth 1 -maxdepth 1 -delete

mkdir -p "$STARRY_OVERLAY_DIR/etc"
printf '%s\n' "AHCI dual-disk test asset" > "$STARRY_OVERLAY_DIR/etc/ahci-dual-test"

# Stage the workload itself: the qemu config only sends one short line
# ("sh /usr/local/bin/ahci-dual-ncq-rw-20m.sh") to the guest's interactive shell.
install -D -m 0755 "$STARRY_APP_DIR/ahci-dual-ncq-rw-20m.sh" \
    "$STARRY_OVERLAY_DIR/usr/local/bin/ahci-dual-ncq-rw-20m.sh"
