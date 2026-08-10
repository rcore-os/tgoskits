#!/usr/bin/env bash
# Restore Orange Pi Linux boot.scr from boot.scr.linux.bak over SSH.
set -euo pipefail

BOARD="${BOARD:-192.168.6.133}"
BOARD_USER="${BOARD_USER:-orangepi}"
BOARD_PASS="${BOARD_PASS:-orangepi}"

ssh "${BOARD_USER}@${BOARD}" "PASS=${BOARD_PASS} bash -s" <<'REMOTE'
set -euo pipefail
MNT=/boot
if [[ ! -f "$MNT/boot.scr.linux.bak" ]]; then
  echo "Missing $MNT/boot.scr.linux.bak — restore from U-Boot or reflash eMMC." >&2
  exit 1
fi
printf '%s\n' "$PASS" | sudo -S cp "$MNT/boot.scr.linux.bak" "$MNT/boot.scr"
printf '%s\n' "$PASS" | sudo -S sync
ls -lh "$MNT/boot.scr" "$MNT/boot.scr.linux.bak"
echo "Linux boot.scr restored."
REMOTE

read -r -p "Reboot into Orange Pi Linux? [y/N] " ans
if [[ "${ans,,}" == "y" ]]; then
  ssh "${BOARD_USER}@${BOARD}" "printf '%s\n' '${BOARD_PASS}' | sudo -S reboot" || true
fi
