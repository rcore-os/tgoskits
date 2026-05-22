# Phase 2: Online Agent Record

This is a short factual record for later expansion.

## Result

Phase 2 online agent smoke passed on StarryOS x86_64 QEMU.

Command:

```bash
cargo xtask starry qemu \
  --arch x86_64 \
  --qemu-config apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml \
  --rootfs tmp/axbuild/rootfs/rootfs-x86_64-picoclaw-online.img
```

Guest command:

```bash
picoclaw agent -m 'Reply with exactly: STARRY_PICOCLAW_AGENT_OK'
```

Success markers:

```text
STARRY_PICOCLAW_AGENT_OK
STARRY_PICOCLAW_AGENT_PASSED
```

The passing run used an OpenAI-compatible endpoint:

- provider: `openai`
- model: `mimo-v2.5`
- api_base: `https://token-plan-cn.xiaomimimo.com/v1`

No API key or generated rootfs image is tracked.

## Problems Found

1. QEMU success and failure markers were unsafe because the guest shell echoes
   commands before executing them.

   `echo STARRY_PICOCLAW_AGENT_PASSED` appeared in the echoed script and could
   satisfy `success_regex` before the agent request completed. The marker is now
   printed as `printf 'STARRY_%s\n' PICOCLAW_AGENT_PASSED`, so the echoed command
   does not contain the full success token.

2. `tee` masked the `picoclaw agent` exit status.

   The agent command originally piped through `tee`, so the pipeline could
   return success even when `picoclaw agent` failed. The QEMU script now writes
   agent output to a file, checks the command directly, then prints the file.

3. Go DNS failed on UDP `setsockopt`.

   First real online run failed with:

   ```text
   dial udp 10.0.2.3:53: setsockopt: protocol not available
   ```

   Root cause: Go enables `SO_BROADCAST` on UDP sockets during DNS setup.
   StarryOS returned `ENOPROTOOPT`. The StarryOS syscall layer now accepts
   `SOL_SOCKET/SO_BROADCAST` as a validated no-op.

4. The provided Anthropic-style endpoint was not suitable for the first config.

   With `provider = "anthropic"`, PicoClaw used the OpenAI-compatible
   `/chat/completions` path and received 404 from the `/anthropic` base.

   With `provider = "anthropic-messages"`, the request reached the service but
   the default Claude model was not supported.

   Host-side probing showed the usable path for this key is the
   OpenAI-compatible root `/v1` with model `mimo-v2.5`.

## Files Changed

- `os/StarryOS/kernel/src/syscall/net/opt.rs`
  - Accepts `SO_BROADCAST` in `setsockopt` as a no-op after validating the int
    option value.

- `apps/starry/picoclaw-cli/prepare_picoclaw_rootfs.sh`
  - Accepts `OPENAI_API_KEY` and `ANTHROPIC_AUTH_TOKEN` as fallback key sources.
  - Supports Anthropic Messages defaults when `ANTHROPIC_AUTH_TOKEN` is used.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-agent.toml`
  - Avoids success marker false positives from shell echo.
  - Preserves the agent command exit status.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-offline.toml`
  - Uses the safer split success marker.

- `apps/starry/picoclaw-cli/qemu-x86_64-picoclaw-gateway.toml`
  - Uses the safer split success marker.

- `apps/starry/picoclaw-cli/README.md`
  - Documents explicit OpenAI-compatible online rootfs preparation.

## Verification

- `cargo fmt`
- `cargo xtask clippy --package starry-kernel`
  - 10 checks passed.
- Phase 2 QEMU agent smoke passed and printed
  `STARRY_PICOCLAW_AGENT_PASSED`.

## Remaining Watch Points

- `sys_prctl: unsupported option 1398164801` still appears as a non-blocking warning.
- `Unsupported ioctl command: 21505` still appears as a non-blocking terminal ioctl warning.
- Gateway validation is not done yet.
