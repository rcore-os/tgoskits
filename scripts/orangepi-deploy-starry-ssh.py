#!/usr/bin/env python3
"""Deploy StarryOS boot.scr + starryos.bin + dtb over SSH (password auth)."""
from __future__ import annotations

import os
import sys
from pathlib import Path

import pexpect

ROOT = Path(__file__).resolve().parents[1]
BOARD = os.environ.get("BOARD", "192.168.6.133")
USER = os.environ.get("BOARD_USER", "orangepi")
PASS = os.environ.get("BOARD_PASS", "orangepi")
BIN = ROOT / "target/aarch64-unknown-linux-musl/release/starryos.bin"
DTB = ROOT / "os/StarryOS/configs/board/orangepi-5-plus.dtb"
BOOT_SCR = ROOT / "tmp/starry-boot-deploy/boot-starry.scr"

REMOTE = rf"""set -e
PASS={PASS!r}
MNT=/boot
if [ ! -f "$MNT/boot.scr.linux.bak" ] && [ -f "$MNT/boot.scr" ]; then
  printf '%s\n' "$PASS" | sudo -S cp "$MNT/boot.scr" "$MNT/boot.scr.linux.bak"
fi
printf '%s\n' "$PASS" | sudo -S cp /tmp/starryos.bin "$MNT/starryos.bin"
printf '%s\n' "$PASS" | sudo -S cp /tmp/starryos.dtb "$MNT/starryos.dtb"
printf '%s\n' "$PASS" | sudo -S cp /tmp/boot-starry.scr "$MNT/boot.scr"
printf '%s\n' "$PASS" | sudo -S sync
ls -lh "$MNT/boot.scr" "$MNT/starryos.bin" "$MNT/starryos.dtb" "$MNT/boot.scr.linux.bak"
"""


def run(cmd: str, stdin: str | None = None, timeout: int = 120) -> None:
    print(f"$ {cmd}", flush=True)
    child = pexpect.spawn(cmd, timeout=timeout, encoding="utf-8", codec_errors="replace")
    child.logfile = sys.stdout
    while True:
        idx = child.expect(
            [
                pexpect.EOF,
                "(?i)password:",
                "Are you sure you want to continue connecting",
            ]
        )
        if idx == 1:
            child.sendline(PASS)
            if stdin is not None:
                child.send(stdin)
                child.sendeof()
        elif idx == 2:
            child.sendline("yes")
        else:
            break
    if child.isalive():
        child.close()
    if child.exitstatus not in (0, None):
        raise SystemExit(f"command failed (exit {child.exitstatus})")


def main() -> int:
    for path in (BIN, DTB, BOOT_SCR):
        if not path.is_file():
            print(f"Missing {path}", file=sys.stderr)
            return 1

    run(
        f"scp -o StrictHostKeyChecking=no {BIN} {DTB} {BOOT_SCR} "
        f"{USER}@{BOARD}:/tmp/"
    )
    run(
        f"ssh -o StrictHostKeyChecking=no {USER}@{BOARD} "
        f"cp /tmp/orangepi-5-plus.dtb /tmp/starryos.dtb",
    )
    run(f"ssh -o StrictHostKeyChecking=no {USER}@{BOARD} bash -s", stdin=REMOTE)
    print("Rebooting board into StarryOS ...", flush=True)
    run(
        f"ssh -o StrictHostKeyChecking=no {USER}@{BOARD} "
        f"echo {PASS} | sudo -S reboot",
        timeout=30,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
