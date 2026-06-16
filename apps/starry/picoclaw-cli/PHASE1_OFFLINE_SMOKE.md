# Phase 1: Offline Smoke Record

This is a short factual record for later expansion.

## Result

Phase 1 offline smoke passed on StarryOS x86_64 QEMU.

Command:

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-alpine.img
```

Guest commands covered:

- `picoclaw version`
- `picoclaw --help`
- `picoclaw onboard`
- `picoclaw status`
- checked `/root/.picoclaw/config.json`
- checked `/root/.picoclaw/workspace`

Success marker:

```text
STARRY_PICOCLAW_OFFLINE_PASSED
```

## Problems Found

1. QEMU config schema mismatch.

   `success_regex` and `fail_regex` must be arrays in this repository's ostool
   QEMU config format. A single string failed TOML parsing before QEMU started.

2. Rootfs injection wrote the debugfs command file into the guest image.

   The temporary `debugfs.cmds` file was originally placed inside the overlay
   directory, so the generic overlay walk injected it as `/debugfs.cmds`. It was
   moved outside the overlay temp directory.

3. PicoClaw binary was injected as a FIFO.

   The debugfs mode command used `010%s`, which produced `010755` for executable
   files. In ext inode mode this became a FIFO with mode `0755`, so the guest
   returned `Permission denied` when running `/usr/local/bin/picoclaw`.

   Fix: use `0100%s`, producing `0100755`, which is a regular executable file.

4. Non-blocking StarryOS warnings appeared while running PicoClaw.

   Observed warnings:

   - `sys_prctl: unsupported option 1398164801`
   - `Unsupported ioctl command: 21505 for fd: 1`

   These did not block `version`, `--help`, `onboard`, or `status`. Keep them as
   watch points for the online agent and gateway phases.

## Files Completed

- `apps/starry/picoclaw-cli/prepare_picoclaw_assets.sh`
  - Downloads or reuses the PicoClaw Linux x86_64 release asset.
  - Verifies SHA-256.
  - Installs `picoclaw` and `picoclaw-launcher` under `target/picoclaw/assets/`.

- `apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh`
  - Builds or reuses the StarryOS x86_64 Alpine rootfs.
  - Injects PicoClaw binaries into `/usr/local/bin/`.
  - Supports optional online config, `.security.yml`, proxy env file, and CA bundle injection.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml`
  - Runs the offline smoke commands.
  - Checks local PicoClaw config/workspace state.
  - Emits `STARRY_PICOCLAW_OFFLINE_PASSED`.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml`
  - Prepared for Phase 2 online agent validation.
  - Requires online rootfs with config and `.security.yml`.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-gateway.toml`
  - Prepared for Phase 3 gateway validation.
  - Starts gateway and checks `/health`.

- `apps/starry/picoclaw-cli/README.md`
  - Documents offline, online agent, and gateway usage.

- `apps/starry/README.md`
  - Adds a short entry pointing to the PicoClaw CLI example.

## Not Done Yet

- Online agent request validation.
- Gateway health validation.
- StarryOS kernel changes. No kernel change was needed for Phase 1.
- Regression tests for syscall or ABI gaps. No blocking gap was confirmed in Phase 1.
