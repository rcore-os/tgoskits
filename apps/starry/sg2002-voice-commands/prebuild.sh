#!/usr/bin/env bash
set -euo pipefail
bundle="${VOICE_BUNDLE_DIR:?Set VOICE_BUNDLE_DIR to the RISC-V deployment directory}"
[[ "$STARRY_ARCH" == riscv64 ]] || { echo "voice bundle requires riscv64" >&2; exit 1; }
for file in run.sh bin/voice-commands model/encoder.onnx validation/commands.wav; do
    [[ -f "$bundle/$file" ]] || { echo "missing bundle file: $file" >&2; exit 1; }
done
mkdir -p "$STARRY_OVERLAY_DIR/opt/voice-commands"
cp -a "$bundle/." "$STARRY_OVERLAY_DIR/opt/voice-commands/"
