---
name: board-linux-starry-debug
description: Debug physical-board workflows that need Linux-side deployment or inspection before running StarryOS or ArceOS through the local board service. Use this skill when a board workload depends on files in the board Linux rootfs, when the user says to use `board connect`, when SSH/rsync deployment must happen while holding a board lease, when StarryOS reports a deployed binary as `not found`, or when diagnosing mismatches between Linux-visible files and StarryOS-visible files on OrangePi-5-Plus or similar boards.
---

# Board Linux Starry Debug

## Purpose

Use this workflow when one physical board must be kept leased while you:

1. Boot its normal Linux image.
2. Discover the board IP from serial or Linux commands.
3. SSH/rsync files into the Linux rootfs.
4. Flush the filesystem.
5. Release the board lease.
6. Reacquire the board for a StarryOS or ArceOS board run.

This avoids the common mistake of testing a newly copied binary on Linux, then rebooting into StarryOS before the ext4 state is safely visible to the next boot.

## Local Board Service

Prefer the repository's local board service unless the user explicitly gives a shared service endpoint.

```bash
cargo xtask board ls
cargo xtask board connect -b OrangePi-5-Plus
```

If the requested board type is missing, list first and use the exact local type. For example, a shared service may use `OrangePi-5-Plus-robot`, while the local service may expose `OrangePi-5-Plus`.

`board connect` holds the lease until the outer process exits. Logging out of the Linux shell inside serial does not necessarily release the board.

## Linux Deployment Flow

1. Keep `board connect` open until Linux reaches a login or shell prompt.
2. Get the IP from the boot banner or run:

```bash
ip -brief addr
```

3. In a separate host command, verify SSH:

```bash
ssh -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=8 orangepi@<ip> \
  'hostname; id; ip -brief addr'
```

4. Copy payloads to `/tmp`, then use sudo to replace the final path and flush:

```bash
rsync -az --delete <local-dir>/ orangepi@<ip>:/tmp/<name>/
ssh orangepi@<ip> '
  set -e
  printf "%s\n" orangepi | sudo -S rm -rf /target/path
  printf "%s\n" orangepi | sudo -S mv /tmp/<name> /target/path
  printf "%s\n" orangepi | sudo -S chown -R root:root /target/path
  printf "%s\n" orangepi | sudo -S sync
  ls -l /target/path
  sync
'
```

Use a Linux-side smoke test after deployment when the workload can run on Linux. Capture its exact success markers or final result line.

## Releasing The Lease

Before starting the StarryOS or ArceOS board run, release the `board connect` process:

```bash
ps -ef | rg 'target/debug/tg-xtask board connect|cargo xtask board connect'
kill <pid>
sleep 2
cargo xtask board ls
```

Proceed only when the board is available again.

## StarryOS Board Run

Run the board workload through `cargo xtask`, using the local board type unless a remote endpoint is required:

```bash
cargo xtask starry app board -t <case> --board-config <config> -b OrangePi-5-Plus
```

If an app board config has its own `shell_init_cmd`, verify the runner honors it. If the observed command comes from the app's `init.sh` instead, inspect `scripts/axbuild/src/starry/mod.rs` and the app runner path before assuming the config is wrong.

## Diagnosing StarryOS `not found`

When StarryOS prints `<binary>: not found` for a path that exists on Linux, do not assume the binary build is broken. Check the StarryOS-visible rootfs.

Prefer a temporary board config outside the repository, or remove it before committing:

```toml
board_type = "OrangePi-5-Plus"
shell_prefix = "root@starry:/root #"
shell_init_cmd = '''
echo BOARD_DIAG_BEGIN
cd /target/path
pwd
ls -ld /target/path
ls -l /target/path
ls -l /lib/ld-linux-aarch64.so.1 /lib/aarch64-linux-gnu/ld-linux-aarch64.so.1 2>&1 || true
od -An -tx1 -N64 /target/path/<binary> 2>&1 || true
readelf -l /target/path/<binary> 2>&1 || true
echo BOARD_DIAG_DONE
'''
success_regex = ["(?m)^BOARD_DIAG_DONE$"]
fail_regex = ["(?i)\\bpanic(?:ked)?\\b"]
timeout = 120
```

Interpretation:

- Directory exists but binary is absent: redeploy from Linux and run `sync` before releasing the lease.
- Binary exists but PT_INTERP loader is absent: install or copy the dynamic loader and needed shared libraries, or rebuild for an available runtime.
- `readelf` is absent in the guest: use `od` and `ls` for minimal proof, and inspect ELF details from Linux or host-side `readelf`.

## Evidence To Keep

Keep the shortest useful evidence:

- Board type and whether local or shared service was used.
- Linux IP and deployment destination.
- Linux smoke result line or success marker.
- StarryOS final result line or success marker.
- Any root cause found, especially stale rootfs contents, missing loader, or runner command override behavior.

Delete temporary diagnostic configs before committing unless they are meant to become a maintained project test entry.
