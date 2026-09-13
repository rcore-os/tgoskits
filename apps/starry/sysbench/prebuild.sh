#!/bin/bash
set -eu
install -Dm755 "$STARRY_APP_DIR/../sysbench-board/harness/run.sh" "$STARRY_OVERLAY_DIR/usr/bin/sysbench-run"
