#!/bin/sh
set -eu

export RUST_BACKTRACE=1
export LD_LIBRARY_PATH="/usr/local/lib:/usr/lib:/lib:${LD_LIBRARY_PATH:-}"

echo "STARRY_UVC_FPS_BEGIN"
/usr/bin/uvc-fps --device 0 --format mjpeg --interval-sec 1
