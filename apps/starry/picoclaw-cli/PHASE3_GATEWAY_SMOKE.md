# Phase 3: Gateway Smoke Record

This is a short factual record for later expansion.

## Goal

Phase 3 checks that PicoClaw can run as a long-lived HTTP gateway service inside
StarryOS x86_64 QEMU and answer a minimal local health request.

## Result

Phase 3 gateway smoke passed on StarryOS x86_64 QEMU.

Command:

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-gateway.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img
```

Guest flow:

```bash
picoclaw gateway --allow-empty --host 127.0.0.1
curl -fsS http://127.0.0.1:18790/health
```

Success marker:

```text
STARRY_PICOCLAW_GATEWAY_PASSED
```

Health response observed in the guest:

```text
{"status":"ok","uptime":"55.827136ms","pid":15}
```

## Problems Found

1. QEMU netdev options were duplicated when the gateway config added host port
   forwarding.

   The rootfs-backed StarryOS QEMU path already ensures a default
   `-netdev user,id=net0`. The gateway config needs
   `hostfwd=tcp::18790-:18790`, so the generated command had two `net0`
   definitions and QEMU failed before StarryOS booted.

   The QEMU rootfs patcher now treats any `-netdev` argument containing
   `id=net0` as the existing netdev, preserving extra options such as
   `hostfwd`.

## Files Changed

- `scripts/axbuild/src/rootfs/qemu.rs`
  - Preserves existing `net0` netdev options while still patching rootfs drive
    paths.
  - Adds a regression test for preserving `hostfwd` in rootfs-backed QEMU runs.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-gateway.toml`
  - Starts `picoclaw gateway` in the guest and validates `/health`.
  - Keeps host forwarding on port `18790` for manual inspection.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-interactive.toml`
  - Provides an interactive StarryOS shell with PicoClaw environment variables
    and common commands printed on entry.

- `apps/starry/picoclaw-cli/run_picoclaw_interactive.sh`
  - Creates or reuses a persistent user rootfs with online PicoClaw config and
    starts the interactive QEMU shell.

- `apps/starry/picoclaw-cli/README.md`
  - Documents Gateway smoke and interactive usage.

- `apps/starry/README.md`
  - Points the top-level Starry examples index to the Phase 3 and interactive
    flows.

## Verification

- `cargo fmt`
- `cargo xtask clippy --package axbuild`
  - 4 checks passed.
- Gateway QEMU smoke passed and printed
  `STARRY_PICOCLAW_GATEWAY_PASSED`.
