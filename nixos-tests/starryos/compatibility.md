# StarryOS-backed nixosTest P1 compatibility evidence

This ledger records the evidence obtained for the independent P1 path. It does not claim full NixOS compatibility.

## P1 discovery and static checks

| Check | Result |
| --- | --- |
| `cargo xtask starry test nixos --list` | Passed; reported `boot`, `arch=x86_64`, `target=x86_64-unknown-none`; no VM started. |
| Launcher tests | Passed: 7/7 tests in `python3 -m unittest discover -s nixos-tests/starryos -p 'test_*.py' -v`. |
| Axbuild host tests | Passed: 263 tests for `cargo test -p axbuild starry` in `ghcr.io/rcore-os/tgoskits-container:latest`. |
| `cargo xtask clippy --package axbuild` | Passed. |
| Nix test flake evaluation | Passed: `nix flake check --no-build path:nixos-tests/starryos`. |
| App flake evaluation | Passed: `nix flake check --no-build path:apps/starry/nixos`. |
| Rootfs artifact self-test | Passed: `STARRY_NIXOS_ARTIFACT_SELF_TEST_PASSED`. |

The Nix checks retried a missing mirror NAR before successfully evaluating the derivations. Rust formatting was applied with `cargo fmt --all` after the initial check reported formatting differences.

## P1 runtime

Command:

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c boot
```

Observed preparation and launch identity:

- kernel source: `/home/user0/workspace/tgoskits/target/x86_64-unknown-none/release/starryos.bin`
- kernel NAR hash: `sha256-0qiyFvF8EeDJvPr4VB2EyqAQ8MqVdRnko95GDKcVB6A=`
- imported kernel store path: `/nix/store/gq18jjcphdhyd6mcn9wzf1nsqrqmd2zv-starry-nixos-kernel`
- system toplevel: `/nix/store/fmwlbisll4zjspinwkkn6wm2hj6b5hz6-nixos-system-starrynixos-starry-nixos-stage2`
- QEMU: pinned `qemu-system-x86_64`, q35, 8 GiB, 8 vCPU, TCG-compatible configuration

The run reached the ordered markers `pid1 → activation → systemd → marker → STARRY_NIXOS_SYSTEM_PASSED`. The driver then finished `wait_for_shutdown` in 12.85 seconds and completed the test script in 551.08 seconds with no terminal failure pattern and no QEMU exit exception. Guest serial also showed `Import lastlog data into lastlog2 database` occupying about 7.5 minutes before `multi-user.target`; this is recorded as a slow path, not a P1 contract failure.

This TCG success depended on the `reboot(2)` fix that remains in this feature branch: Starry previously called `shutdown_filesystems()` inside `sys_reboot()`, which blocked power-off after the marker service. The same kernel change was also submitted independently as https://github.com/rcore-os/tgoskits/pull/2220 so upstream can review the ABI fix without the larger nixosTest framework. Merge #2220 first when possible; if P1 lands later, drop the duplicate `sys.rs` hunk rather than reviewing it twice.

The test script polls the full console for terminal evidence, validates the ordered success sequence, rejects failure patterns, and bounds shutdown polling by the same 900-second global deadline.

## Retained app and test-suit paths

The retained app command:

```bash
proxychains4 cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
```

built and published the same rootfs identity:

- flake-lock SHA-256: `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4`
- system: `/nix/store/fmwlbisll4zjspinwkkn6wm2hj6b5hz6-nixos-system-starrynixos-starry-nixos-stage2`
- image SHA-256: `11615c02b509893e3a4384c91513c72f4197a290c88f1d15e540555e1ee5cd6f`

QEMU started and the guest printed `STARRY_NIXOS_PHASE=activation`, `systemd`, `marker`, and `STARRY_NIXOS_SYSTEM_PASSED`. A later retry without the local `sys_reboot()` patch reported `QEMU stopped without matching a configured success regex` after `rsext4::ext4::sync` during poweroff. That nonzero result is the known kernel shutdown defect tracked by https://github.com/rcore-os/tgoskits/pull/2220, not a P1 framework regression. The `sys_reboot()` patch is restored on this feature branch.

The retained focused test-suit command:

```bash
cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2
```

reached kernel build and image-registry fetch. Wrapping the same command in `proxychains4` broke grouped `prebuild.sh` because `qemu-x86_64` user-mode then loaded `libproxychains4.so` against musl (`__isoc23_sscanf` missing, exit 127). Without the wrapper, the host request to `https://raw.githubusercontent.com/rcore-os/tgosimages/refs/heads/main/registry/v0.0.12.toml` timed out. No guest marker result is claimed. This is host image-registry/network evidence, not a P1 framework regression.

## CI-like P1 run

`.github/ci/checks/starry-apps.toml` still schedules only the legacy app command inside `docker.io/nixos/nix:2.33.1`. P1 is not added to CI. The native-host TCG P1 command already produced the T021 evidence above; repeating the container app path would duplicate T027 rather than exercise `cargo xtask starry test nixos`.


## Scope

P1 does not replace either retained path or claim guest service assertions, broad upstream NixOS test compatibility, multiple machines, networking, graphics, installer/initrd support, or other architectures. CI scheduling remains unchanged.
