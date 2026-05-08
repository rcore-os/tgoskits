---
name: board-uboot-fsck-repair
description: Repair a remote physical board's Linux ext4 root filesystem by interrupting U-Boot through `ostool-server`, injecting one-shot `extraboardargs=fsckfix`, booting Linux, and confirming the login/root/user shell prompt. Use this skill when board Linux boot fails in initramfs fsck with ext4 journal/orphan/inode corruption, when StarryOS or ArceOS board tests may have damaged the OrangePi-5-Plus rootfs, when the user asks to recover a board through U-Boot fsck, or when validating that Linux can boot cleanly before or after Starry board filesystem tests.
---

# Board U-Boot Fsck Repair

## Overview

Recover the board rootfs with a one-shot U-Boot environment override, then prove the board can enter Linux before continuing board tests. Prefer the bundled script for repeatability; fall back to manual `ostool board connect` when interactive serial works better.

## Quick Command

From the repository root:

```bash
node .claude/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js \
  --board-type OrangePi-5-Plus
```

The script reads `~/.ostool/config.toml` by default. Override the server when needed:

```bash
node .claude/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js \
  --server 10.3.10.9 --port 2999 --board-type OrangePi-5-Plus
```

Treat success as all of:

- U-Boot prompt was reached.
- `setenv extraboardargs fsckfix` and `boot` were sent.
- Linux reached a login prompt, root shell, or auto-login user shell such as `orangepi@orangepi5plus:~$`.
- The script printed a `RESULT ... linux_login=true ...` line and saved a serial log.

## Manual Workflow

1. Check availability:

```bash
ostool board ls
```

2. Connect and interrupt U-Boot:

```bash
ostool board connect -b OrangePi-5-Plus
```

Press space when the console prints `Hit any key to stop autoboot:`.

3. At the `=>` U-Boot prompt, inject the repair argument without saving it:

```text
setenv extraboardargs fsckfix
boot
```

Do not use `saveenv`; this is a one-shot recovery path. Prefer `extraboardargs=fsckfix` over `extraargs=fsck.repair=yes` on Orange Pi images because `orangepiEnv.txt` may override `extraargs`, while the boot script appends `extraboardargs` later.

4. Confirm initramfs runs the forcing repair path. Useful evidence includes `fsck.ext4 -y -C0 /dev/mmcblk0p2`, `FILE SYSTEM WAS MODIFIED`, fixed/cleared entries, or a later clean check.

5. Continue only after Linux reaches a prompt such as `root@...#`, `<host> login:`, or `orangepi@orangepi5plus:~$`. If fsck still says `UNEXPECTED INCONSISTENCY; RUN fsck MANUALLY`, collect the serial log and do not run Starry board tests on that rootfs yet.

## Board-Test Usage

Use this repair before a destructive board validation and again after StarryOS writes to the Linux rootfs:

1. Repair and prove Linux boots with this skill.
2. Run the Starry board workload, preferably through `cargo xtask starry test board ...`. For a minimal rootfs safety check, create a temporary config outside the repository:

```toml
board_type = "OrangePi-5-Plus"
shell_prefix = "root@starry:/root #"
shell_init_cmd = "echo STARRY_MINIMAL_BOOT_OK"
success_regex = ["(?m)^STARRY_MINIMAL_BOOT_OK\\s*$"]
fail_regex = ["(?i)(kernel panic|panicked at|fatal exception)"]
timeout = 180
```

Then run:

```bash
cargo xtask starry test board -t smoke-orangepi-5-plus \
  --board-test-config /tmp/starry-minimal-orangepi-5-plus.toml
```

3. Boot Linux normally, without `fsckfix`, to check whether initramfs fsck reports corruption:

```bash
cargo xtask board connect -b OrangePi-5-Plus
```

4. Passing evidence: Starry reaches `root@starry:/root #` and prints the success marker; Linux normal boot reaches a login/user shell and does not print `directory corrupted`, `UNEXPECTED INCONSISTENCY`, or `requires a manual fsck`.
5. A Linux normal boot may print `recovering journal`, run `fsck.ext4 -a`, and exit fsck with status code `1`; this means filesystem errors were corrected automatically. Treat it as acceptable only if boot continues to a prompt and no manual-fsck corruption message appears.
6. If Linux fails fsck, save the failing serial log, release the serial session, then run this repair again before returning the board to the pool.
7. A Starry test may reach `root@starry:/root #` but still fail its command or time out; still perform the Linux normal-boot check before concluding the rootfs is safe.

## Failure Handling

- If U-Boot is not interrupted, release the session and retry; reconnecting powers the board on from a clean session.
- If the script cannot connect to WebSocket serial, verify `ostool board ls` and the `~/.ostool/config.toml` `[board] server_ip` / `port` values.
- If the board reaches Linux but login is not detected, rerun with `--login-regex '<pattern>'` matching the actual console prompt.
- If the repair argument does not affect initramfs, inspect `/boot.cmd` and `/boot/orangepiEnv.txt`; on known Orange Pi images, `extraboardargs=fsckfix` is the working hook.
- Always release the board session after collecting evidence. The server powers off the board on WebSocket close/session release.

## Script Reference

`scripts/uboot_fsck_repair.js` uses the same `ostool-server` API as `ostool board connect`:

- `POST /api/v1/sessions`
- serial WebSocket at `ws_url`
- heartbeat while the session is held
- `DELETE /api/v1/sessions/<id>` on exit

Common options:

```bash
node .claude/skills/board-uboot-fsck-repair/scripts/uboot_fsck_repair.js --help
```
