# StarryOS-backed nixosTest compatibility evidence

This ledger records the evidence obtained for the independent P1–P3 path. It does not claim full NixOS compatibility.

## P1 discovery and static checks

| Check | Result |
| --- | --- |
| `cargo xtask starry test nixos --list` | Passed; reported `boot`, `service`, `service-fail`, `unsupported` for `arch=x86_64`, `target=x86_64-unknown-none`; no VM started. |
| Launcher plus evaluator tests | Passed: 19 tests in `python3 -m unittest discover -s nixos-tests/starryos -p 'test_*.py' -v`. |
| Axbuild host tests | Passed: `cargo test -p axbuild starry` in `ghcr.io/rcore-os/tgoskits-container:latest`. |
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

P1 does not replace either retained path. P2 adds declared-oneshot serial assertions and fail-closed unsupported command-channel APIs; it still does not claim broad upstream NixOS test compatibility, multiple machines, networking, graphics, installer/initrd support, or other architectures. CI scheduling remains unchanged.

## P2 runtime

Discovery after the four-case CLI:

```text
boot	arch=x86_64	target=x86_64-unknown-none
service	arch=x86_64	target=x86_64-unknown-none
service-fail	arch=x86_64	target=x86_64-unknown-none
unsupported	arch=x86_64	target=x86_64-unknown-none
```

### service

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c service
```

- kernel NAR hash: `sha256-sfwTL35oQdxyM0uyS2Yhvkr3Dv05/TWre62/HXzA3pg=`
- imported kernel store path: `/nix/store/3lm42jvn1267r1kxfyyy3kg2sixcn9k0-starry-nixos-kernel`
- system toplevel: `/nix/store/13k7r15yk0rirxihlv1g1liqg4d5j0wk-nixos-system-starrynixos-starry-nixos-stage2`
- ordered P1 markers, then `STARRY_NIXOS_ASSERT_BEGIN` / `CMD=hello` / `STATUS=0` / `Hello, world!` / `STARRY_NIXOS_ASSERT_PASSED`
- `wait_for_shutdown` finished in 3.89 seconds; test script 416.43 seconds; xtask exit 0

The script waits for the P1 terminal markers first, then for the assertion block inside the 900-second global bound. Journal-prefixed serial lines such as `starry-nixos-service-assert-start[2]: STARRY_NIXOS_ASSERT_CMD=hello` are accepted.

### service-fail

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c service-fail
```

First TCG run (T054):

- kernel NAR hash: `sha256-NZYfApU75v7pV4C+wnoJ36RNq9wrKTVAC1eStjHBKvQ=`
- imported kernel store path: `/nix/store/0f90n1kg46hfkbfhk7kqlybilpz766mf-starry-nixos-kernel`
- system toplevel: `/nix/store/rf643ni0z3vibvvfr6djb4wadi0904kh-nixos-system-starrynixos-starry-nixos-stage2`
- ordered P1 markers, then `STARRY_NIXOS_ASSERT_FAILED:declared command false exited 1`
- `wait_for_shutdown` finished in 3.99 seconds; xtask exit 1 with `STARRY_NIXOS_PHASE_FAILED=guest-assertion` and `failed expectation: declared command false exited 1`

A passing Nix/QEMU status would be a framework bug. The guest still force-poweroffs after the named failure.

### unsupported

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c unsupported
```

Returned nonzero in 30.03 seconds with `STARRY_NIXOS_PHASE_FAILED=guest-assertion` and `unsupported Starry nixosTest operation: succeed`. The wrapped machine never called `connect()` and did not wait on `/dev/hvc0`.

### boot after P2

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c boot
```

- kernel NAR hash: `sha256-jRxVPH4i+pibB9gOC3ay/gAzbKagHqWCjkxu0myzFGI=`
- imported kernel store path: `/nix/store/2833mv9ym1ynqza37f9948l4wjwg4bcp-starry-nixos-kernel`
- system toplevel: `/nix/store/b9h09mm8rlcw8z0iyhsqfwnq1jblvfz9-nixos-system-starrynixos-starry-nixos-stage2`
- ordered markers `pid1 → activation → systemd → marker → STARRY_NIXOS_SYSTEM_PASSED`
- `wait_for_shutdown` finished in 4.80 seconds; test script 431.43 seconds; xtask exit 0

The keep-running extra module is not attached to `boot`. Marker poweroff semantics match P1.

## P3 isolation

`service-fail` is the repeatable named-failure reference. Consecutive TCG runs with no manual overlay cleanup:

| Run | Exit | Phase | Failed expectation | Overlay |
| --- | --- | --- | --- | --- |
| 1 | 1 | `guest-assertion` | `declared command false exited 1` | fresh `/build/vm-state-starry-nixos-service-fail/` overlay |
| 2 | 1 | `guest-assertion` | `declared command false exited 1` | fresh overlay; same backing image `/nix/store/cpnfgwjgnardcm04p3flm90aw6wgqviq-ext4-fs.img`; no manual cleanup |
| 3 | 1 | `guest-assertion` | `declared command false exited 1` | fresh overlay; same backing image; no manual cleanup |

Launcher overlay rejection remains fail-closed: `test_launch_vm.py` still rejects an existing overlay before QEMU starts.

Rootfs artifact self-test after P2/P3:

```bash
apps/starry/nixos/build-rootfs.sh --self-test
```

Passed with `STARRY_NIXOS_ARTIFACT_SELF_TEST_PASSED`. The keep-running extra module is test-owned and is not part of the app package output.

Retained app QEMU after P2/P3:

```bash
proxychains4 cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
```

- flake-lock SHA-256: `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4`
- system: `/nix/store/b9h09mm8rlcw8z0iyhsqfwnq1jblvfz9-nixos-system-starrynixos-starry-nixos-stage2`
- image SHA-256: `37bb262fcd56a7e70b68fa87e984ec2c84700f60ff778f88587462e665a49eae`
- guest printed `STARRY_NIXOS_PHASE=pid1`, `activation`, `systemd`, `marker`, and `STARRY_NIXOS_SYSTEM_PASSED`
- `qemu run` finished in 445.94 seconds

The app image still lacks `/etc/starry-nixos/keep-running`. Marker/poweroff semantics are unchanged from P1.
