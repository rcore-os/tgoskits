# StarryOS-backed nixosTest

This directory owns the project-local NixOS test framework for StarryOS. It is independent of the Starry application cases under `test-suit/starryos/` and of the legacy `starry app qemu` acceptance path. It currently contains four x86_64 cases: `boot`, `service`, `service-fail`, and `unsupported`.

## Supported boundary

- Host: x86_64 Linux with Nix or Lix, flakes, and `nix-command`.
- Guest: one x86_64 StarryOS VM using the existing NixOS stage-2 system.
- Runtime: QEMU and OVMF supplied by the pinned flake input; TCG is the correctness baseline and `/dev/kvm` is not required.
- Scope: serial console, lifecycle assertions, and declared systemd-oneshot command records. There is no `machine.succeed` / `/dev/hvc0` backdoor, SSH, network test, multi-machine test, graphical test, installer/initrd test, or additional architecture.

Host-installed QEMU and OVMF are not prerequisites. The workflow does not change the active NixOS system, user profile, or tracked repository files.

## Run the cases

From the repository root:

```bash
cargo xtask starry test nixos --list
cargo xtask starry test nixos --arch x86_64 -c boot
cargo xtask starry test nixos --arch x86_64 -c service
cargo xtask starry test nixos --arch x86_64 -c service-fail
cargo xtask starry test nixos --arch x86_64 -c unsupported
```

`--list` is discovery-only: it does not build, evaluate Nix, or start QEMU. The run command builds the current-checkout Starry UEFI image through the existing axbuild path, verifies its NAR hash, imports that exact content into the independent test flake, constructs the shared app-owned stage-2 system, and starts the pinned test driver.

Do not manually stage a kernel or rootfs, and do not wrap the run in an external timeout shorter than 15 minutes. A cold closure build can add host time before QEMU starts.

## Passing evidence and bounds

The serial log must contain this ordered sequence:

```text
STARRY_NIXOS_PHASE=pid1
STARRY_NIXOS_PHASE=activation
STARRY_NIXOS_PHASE=systemd
STARRY_NIXOS_PHASE=marker
STARRY_NIXOS_SYSTEM_PASSED
```

The driver waits at most 600 seconds for terminal evidence and has a 900-second global timeout. A pass additionally requires no panic, fatal record, marker-unit failure, explicit `STARRY_NIXOS_SYSTEM_FAILED:` line, premature QEMU exit, or nonzero QEMU/test status. The marker is not sufficient by itself: the guest must power off cleanly.

The xtask output reports the prepared kernel identity and streams the Nix test-driver output. On failure, inspect the last phase, the first terminal failure pattern, the QEMU exit/shutdown result, and the retained driver log. Artifact preparation, Nix evaluation, machine startup, activation, terminal evidence, shutdown, and timeout failures are intentionally nonzero and fail closed.

Each driver run owns a fresh temporary rootfs overlay, OVMF variables copy, and ESP. The immutable Nix-store rootfs and firmware inputs are never reused as writable state.

## Retained compatibility paths

P1 adds a new entry point; it does not replace these existing checks:

```bash
apps/starry/nixos/build-rootfs.sh --self-test
cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2
```
## Diagnostics and limits

A failure before QEMU indicates kernel preparation, NAR import, flake evaluation, rootfs, firmware, or launch-adapter setup. Look for `STARRY_NIXOS_PHASE_FAILED=` in driver/xtask output: `artifact-preparation`, `machine-startup`, `stage2-activation`, `guest-assertion`, `unexpected-guest-exit`, `shutdown`, or `timeout`.

`service` adds a structured `STARRY_NIXOS_ASSERT_*` serial block after the P1 markers. `service-fail` must return nonzero with phase `guest-assertion`. `unsupported` must return nonzero immediately with `unsupported Starry nixosTest operation: succeed` and must not wait for `/dev/hvc0`. Guest commands are declared systemd oneshots, not `machine.succeed`. There is still no claim that unmodified upstream NixOS tests run on StarryOS, and CI scheduling is unchanged.
A failure before QEMU indicates kernel preparation, NAR import, flake evaluation, rootfs, firmware, or launch-adapter setup. During boot, the last phase and full serial log identify the earliest observable boundary. Missing terminal evidence is a bounded timeout; a terminal marker without shutdown indicates lifecycle failure.

P1 deliberately does not claim broad upstream NixOS test compatibility. Test-specific services and in-guest assertions are deferred to P2 because StarryOS does not yet expose the required guest command channel. Repeatability and deeper diagnostic reporting are P3 work. Unsupported operations must remain explicit failures rather than skipped success.

## 当前验证记录

- `apps/starry/nixos/build-rootfs.sh --self-test` passed with `STARRY_NIXOS_ARTIFACT_SELF_TEST_PASSED`.
- `cargo xtask starry test nixos --list` passed and reported `boot`, `arch=x86_64`, `target=x86_64-unknown-none` without launching a VM.
- `python3 -m unittest discover -s nixos-tests/starryos -p 'test_*.py' -v` passed all 7 launcher contract tests.
- The focused axbuild fallback `cargo test -p axbuild starry` passed 263 tests in the project container. `cargo xtask clippy --package axbuild` passed; repository formatting was applied with `cargo fmt --all`.
- `nix flake check --no-build path:nixos-tests/starryos` and `path:apps/starry/nixos` both evaluated successfully. The Nix client retried a missing mirror NAR before evaluating the derivations.
- Real TCG `proxychains4 cargo xtask starry test nixos --arch x86_64 -c boot` reached the ordered marker sequence, finished `wait_for_shutdown` in 12.85 seconds, and completed the test script in 551.08 seconds. Kernel NAR hash `sha256-0qiyFvF8EeDJvPr4VB2EyqAQ8MqVdRnko95GDKcVB6A=`, kernel store `/nix/store/gq18jjcphdhyd6mcn9wzf1nsqrqmd2zv-starry-nixos-kernel`, system `/nix/store/fmwlbisll4zjspinwkkn6wm2hj6b5hz6-nixos-system-starrynixos-starry-nixos-stage2`. Clean shutdown uses the `sys_reboot()` fix that remains in this branch and is also submitted independently as https://github.com/rcore-os/tgoskits/pull/2220.
- The retained `proxychains4 cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix` path published the same rootfs identity and printed `activation`/`systemd`/`marker`/`STARRY_NIXOS_SYSTEM_PASSED`. A retry taken while the local reboot patch was temporarily removed reported `QEMU stopped without matching a configured success regex` during poweroff sync. Marker semantics are unchanged; the patch is restored on this branch.
- The retained `cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2` path built the Starry test kernel. `proxychains4` must not wrap that command because grouped `prebuild.sh` runs under musl `qemu-x86_64`; the unwrapped run timed out fetching `tgosimages` registry `v0.0.12.toml`. No guest result is claimed.

## 网络受限主机

如果 GitHub image registry 或 flake 内容请求超时，而本机 SOCKS5 代理监听在 `127.0.0.1:7890`，可用 `proxychains4` 包裹需要联网的命令：

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c boot
proxychains4 cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
```

`proxychains4` 只解决宿主机下载路径；不会改变 guest 的 QEMU、TCG、串口或关机语义。不要把 `proxychains4` 注入 guest 的 `qemu-x86_64` 用户态预构建命令：该命令运行在 Starry 测试 rootfs 的动态链接环境中，代理库可能因 glibc 符号不兼容而导致预构建失败。
