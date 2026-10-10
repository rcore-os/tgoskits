#!/bin/sh
set -eu
app_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
export VOICE_CUSTOM_OP_LIBRARY="$app_dir/lib/libvoice_frontend.so"
export VOICE_FRONTEND_WORKER="$app_dir/tpu/frontend-worker"
export VOICE_FRONTEND_MODEL="$app_dir/model/frontend.cvimodel"
exec "$app_dir/lib/ld-linux-riscv64-lp64d.so.1" \
    --library-path "$app_dir/lib" "$app_dir/bin/voice-commands" "$app_dir/model" "$@"
