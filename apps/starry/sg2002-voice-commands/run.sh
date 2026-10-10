#!/bin/sh
set -eu
app_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
unset VOICE_CUSTOM_OP_LIBRARY VOICE_FRONTEND_WORKER VOICE_FRONTEND_MODEL
exec "$app_dir/lib/ld-linux-riscv64-lp64d.so.1" \
    --library-path "$app_dir/lib" "$app_dir/bin/voice-commands" "$app_dir/model" "$@"
