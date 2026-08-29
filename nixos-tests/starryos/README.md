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

`service` continues after those markers until a structured `STARRY_NIXOS_ASSERT_*` block appears, then requires status 0, the expected command output, and clean poweroff. `service-fail` must return nonzero with `STARRY_NIXOS_PHASE_FAILED=guest-assertion` and the named failed expectation. `unsupported` must return nonzero immediately with `unsupported Starry nixosTest operation: succeed` and must not wait for `/dev/hvc0`.

The xtask output reports the prepared kernel identity and streams the Nix test-driver output. On failure, inspect `STARRY_NIXOS_PHASE_FAILED=`, the failed expectation, the last serial evidence, and the QEMU exit/shutdown result. Artifact preparation, Nix evaluation, machine startup, activation, guest assertion, unexpected guest exit, shutdown, and timeout failures are intentionally nonzero and fail closed.

Each driver run owns a fresh temporary rootfs overlay, OVMF variables copy, and ESP. The immutable Nix-store rootfs and firmware inputs are never reused as writable state. A retry needs no manual overlay cleanup.

## Retained compatibility paths

The nixosTest entry point does not replace these existing checks:

```bash
apps/starry/nixos/build-rootfs.sh --self-test
cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2
```

## Diagnostics and limits

A failure before QEMU indicates kernel preparation, NAR import, flake evaluation, rootfs, firmware, or launch-adapter setup. Look for `STARRY_NIXOS_PHASE_FAILED=` in driver/xtask output:

- `artifact-preparation`
- `machine-startup`
- `stage2-activation`
- `guest-assertion`
- `unexpected-guest-exit`
- `shutdown`
- `timeout`

Guest commands are declared systemd oneshots observed on serial, not `machine.succeed`. Shared marker poweroff still happens unless the test-owned `/etc/starry-nixos/keep-running` file is present. There is still no claim that unmodified upstream NixOS tests run on StarryOS, and CI scheduling is unchanged.

## 当前验证记录

最新 TCG 身份、断言块、阶段名和三次 `service-fail` 隔离结果记在 `nixos-tests/starryos/compatibility.md`。

- `cargo xtask starry test nixos --list` 报告四个 x86_64 用例且不启动 VM。
- `python3 -m unittest discover -s nixos-tests/starryos -p 'test_*.py' -v` 覆盖 launcher 与 `starry_machine` 合同。
- TCG `service` 在 P1 marker 后看到 `STARRY_NIXOS_ASSERT_PASSED` 并以零退出。
- TCG `service-fail` 以 `STARRY_NIXOS_PHASE_FAILED=guest-assertion` 和 `declared command false exited 1` 非零退出。
- TCG `unsupported` 立即以 `unsupported Starry nixosTest operation: succeed` 非零退出，不等待 `/dev/hvc0`。
- TCG `boot` 仍完成有序 marker 与关机。

## 网络受限主机

如果 GitHub image registry 或 flake 内容请求超时，而本机 SOCKS5 代理监听在 `127.0.0.1:7890`，可用 `proxychains4` 包裹需要联网的命令：

```bash
proxychains4 cargo xtask starry test nixos --arch x86_64 -c boot
proxychains4 cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
```

`proxychains4` 只解决宿主机下载路径；不会改变 guest 的 QEMU、TCG、串口或关机语义。不要把 `proxychains4` 注入 guest 的 `qemu-x86_64` 用户态预构建命令：该命令运行在 Starry 测试 rootfs 的动态链接环境中，代理库可能因 glibc 符号不兼容而导致预构建失败。
