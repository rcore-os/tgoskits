#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DISK_IMG="$SCRIPT_DIR/disk.img"

truncate -s 64M "$DISK_IMG"
mkfs.fat -F 32 "$DISK_IMG" >/dev/null
