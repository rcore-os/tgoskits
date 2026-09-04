#!/usr/bin/env bash
# Deploy StarryOS to Orange Pi 5 Plus /boot over SSH.
#   ./starryos.bash        booti (starryos.bin + dtb)
#   ./starryos.bash fit    official FIT image.fit + bootm (when U-Boot TFTP has no ethernet)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

MODE="${1:-booti}"
BOARD="${BOARD:-192.168.6.133}"
BOARD_USER="${BOARD_USER:-orangepi}"
BOARD_PASS="${BOARD_PASS:-orangepi}"
WORK="${ROOT}/tmp/starry-boot-deploy"

if ! command -v mkimage >/dev/null 2>&1; then
  echo "Install u-boot-tools: sudo apt install u-boot-tools" >&2
  exit 1
fi

mkdir -p "$WORK"

case "$MODE" in
  booti)
    BIN="${BIN:-target/aarch64-unknown-linux-musl/release/starryos.bin}"
    DTB="${DTB:-os/StarryOS/configs/board/orangepi-5-plus.dtb}"
    BOOT_SCR="${WORK}/boot-starry.scr"
    [[ -f "$BIN" ]] || { echo "Missing $BIN — run: cargo xtask starry defconfig orangepi-5-plus && cargo xtask starry build" >&2; exit 1; }
    [[ -f "$DTB" ]] || { echo "Missing $DTB" >&2; exit 1; }
    cat >"${WORK}/starry-boot.cmd" <<'CMD'
echo Loading StarryOS from eMMC...
load mmc 1:1 0x400000 starryos.bin
load mmc 1:1 0x0a100000 starryos.dtb
echo Booting StarryOS...
booti 0x400000 - 0x0a100000
CMD
    mkimage -C none -A arm64 -T script -d "${WORK}/starry-boot.cmd" "$BOOT_SCR"
    echo "Copying starryos.bin, starryos.dtb, boot.scr to ${BOARD_USER}@${BOARD}:/tmp/ ..."
    scp "$BIN" "$DTB" "$BOOT_SCR" "${BOARD_USER}@${BOARD}:/tmp/"
    REMOTE_FILES='
printf "%s\n" "$PASS" | sudo -S cp /tmp/starryos.bin "$MNT/starryos.bin"
printf "%s\n" "$PASS" | sudo -S cp /tmp/starryos.dtb "$MNT/starryos.dtb"
printf "%s\n" "$PASS" | sudo -S cp /tmp/boot-starry.scr "$MNT/boot.scr"
'
    REMOTE_LS='ls -lh "$MNT/boot.scr" "$MNT/starryos.bin" "$MNT/starryos.dtb"'
    ;;
  fit)
    FIT="${FIT:-target/aarch64-unknown-linux-musl/release/image.fit}"
    BOOT_SCR="${WORK}/boot-starry-fit.scr"
    [[ -f "$FIT" ]] || {
      echo "Missing $FIT — run: cargo xtask starry defconfig orangepi-5-plus && cargo xtask starry build" >&2
      exit 1
    }
    cat >"${WORK}/starry-fit.cmd" <<'CMD'
echo Loading StarryOS FIT from eMMC...
setenv fit_addr_r 0x5480000
load mmc 1:1 ${fit_addr_r} image.fit
echo Booting StarryOS...
bootm ${fit_addr_r}#config-ostool
CMD
    mkimage -C none -A arm64 -T script -d "${WORK}/starry-fit.cmd" "$BOOT_SCR"
    echo "Copying image.fit, boot.scr to ${BOARD_USER}@${BOARD}:/tmp/ ..."
    scp "$FIT" "$BOOT_SCR" "${BOARD_USER}@${BOARD}:/tmp/"
    REMOTE_FILES='
printf "%s\n" "$PASS" | sudo -S cp /tmp/image.fit "$MNT/image.fit"
printf "%s\n" "$PASS" | sudo -S cp /tmp/boot-starry-fit.scr "$MNT/boot.scr"
'
    REMOTE_LS='ls -lh "$MNT/boot.scr" "$MNT/image.fit"'
    ;;
  uboot)
    exec cargo xtask starry uboot --uboot-config os/StarryOS/configs/board/orangepi-5-plus-uboot.toml
    ;;
  *)
    echo "Usage: $0 [booti|fit|uboot]" >&2
    exit 1
    ;;
esac

ssh "${BOARD_USER}@${BOARD}" "PASS=${BOARD_PASS} bash -s" <<REMOTE
set -euo pipefail
MNT=/boot
if ! mountpoint -q "\$MNT"; then
  MNT=/mnt/boot
  BOOT_DEV=\$(findmnt -n -o SOURCE /boot 2>/dev/null || echo /dev/mmcblk0p1)
  printf '%s\n' "\$PASS" | sudo -S mkdir -p "\$MNT"
  printf '%s\n' "\$PASS" | sudo -S mount "\$BOOT_DEV" "\$MNT"
fi
if [[ -f "\$MNT/boot.scr" && ! -f "\$MNT/boot.scr.linux.bak" ]]; then
  printf '%s\n' "\$PASS" | sudo -S cp "\$MNT/boot.scr" "\$MNT/boot.scr.linux.bak"
fi
${REMOTE_FILES}
printf '%s\n' "\$PASS" | sudo -S sync
${REMOTE_LS} "\$MNT/boot.scr.linux.bak" 2>/dev/null || true
echo "Ready to reboot into StarryOS."
REMOTE

read -r -p "Reboot board now? [y/N] " ans
if [[ "${ans,,}" == "y" ]]; then
  ssh "${BOARD_USER}@${BOARD}" "printf '%s\n' '${BOARD_PASS}' | sudo -S reboot" || true
  echo "Reboot sent. Watch serial for STARRY_ORANGEPI_BOOT_OK / root@starry:/root #"
fi
