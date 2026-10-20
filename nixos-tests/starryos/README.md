# StarryOS-backed nixosTest

This directory owns the project-local NixOS test framework for StarryOS. It is independent of the Starry application cases under `test-suit/starryos/` and of the legacy `starry app qemu` acceptance path. Cases live in `cases/*.nix`. The in-tree catalog currently includes `boot`, `function-allowed`, `function-forbidden`, `hello-tmpfiles`, `service`, `service-fail`, `sysctl-forbidden`, and `unsupported`.

## 1. Supported boundary

`run_nixos_app` validates the target architecture and the application's `requires` declaration before execution. The framework currently supports:

- Host: x86_64 Linux with Nix or Lix, flakes, and `nix-command`.
- Guest: one x86_64 StarryOS VM using the existing NixOS stage-2 system.
- Runtime: QEMU and OVMF supplied by the pinned flake input; TCG is the correctness baseline and `/dev/kvm` is not required.
- Scope: serial console, lifecycle assertions, and declared systemd-oneshot command records. There is no `machine.succeed` / `/dev/hvc0` backdoor, SSH, network test, multi-machine test, graphical test, installer/initrd test, or additional architecture.

Host-installed QEMU and OVMF are not prerequisites. The workflow does not change the active NixOS system, user profile, or tracked repository files.

## 2. Run the cases

`run_nixos_app` separates read-only case discovery from case execution. Execution reuses the `missing_caps` check and delegates kernel building and NixOS testing to the single-case runner.

### 2.1 Case selection

Run these commands from the repository root. Case execution requires `--cap nix`; listing does not build the kernel or invoke Nix, so it does not require that capability declaration. When `--arch` is omitted, the architecture defaults to `x86_64`. Explicitly selecting another architecture fails before building.

```bash
cargo xtask starry app qemu -t nixos --cap nix
cargo xtask starry app qemu -t nixos --list-cases
cargo xtask starry app qemu -t nixos --cap nix --case service
cargo xtask starry app qemu -t nixos --cap nix --all-cases
```

The default command runs the `boot` case. `--list-cases` only discovers cases; `--case` runs one case; `--all-cases` runs the discovered catalog.

### 2.2 Execution results

`--all-cases` runs all discovered cases in name order, continues after individual failures, and reports each result and its error context through `QemuTestSummary`. The batch exits with a nonzero status if any case fails. The `function-forbidden`, `service-fail`, `sysctl-forbidden`, and `unsupported` cases deliberately retain nonzero exit contracts, so a full batch containing them also exits with a nonzero status. The summary does not automatically treat a build failure as an expected success. A single `--case` invocation returns that case's result directly.

`--list` is discovery-only: it reads `cases/*.nix` stems, does not build, evaluate Nix, or start QEMU, and does not require the runner to name those stems in source. The run command builds the current-checkout Starry UEFI image through the existing axbuild path, verifies its NAR hash, imports that exact content into the independent test flake, constructs the shared app-owned stage-2 system, and starts the pinned test driver.

Do not manually stage a kernel or rootfs, and do not wrap the run in an external timeout shorter than 15 minutes. A cold closure build can add host time before QEMU starts.

## 3. Passing evidence and bounds

The serial log must contain this ordered sequence:

```text
STARRY_NIXOS_PHASE=pid1
STARRY_NIXOS_PHASE=activation
STARRY_NIXOS_PHASE=systemd
STARRY_NIXOS_PHASE=marker
STARRY_NIXOS_SYSTEM_PASSED
```

The driver waits at most 300 seconds for terminal evidence and uses the same 300-second global timeout. A pass additionally requires no panic, fatal record, marker-unit failure, explicit `STARRY_NIXOS_SYSTEM_FAILED:` line, premature QEMU exit, or nonzero QEMU/test status.

`service` continues after those markers until a structured `STARRY_NIXOS_ASSERT_*` block appears, then requires status 0, the expected command output, and clean poweroff. `service-fail` must return nonzero with `STARRY_NIXOS_PHASE_FAILED=guest-assertion` and the named failed expectation. `unsupported` must return nonzero immediately with `unsupported Starry nixosTest operation: succeed` and must not wait for `/dev/hvc0`.

`hello-tmpfiles` is the authoring example: a test-owned extra module writes `/etc/starry-nixos/hello-tmpfiles`, and the generated oneshot asserts that file over serial.

The xtask output reports the prepared kernel identity and streams the Nix test-driver output. On failure, inspect `STARRY_NIXOS_PHASE_FAILED=`, the failed expectation, the last serial evidence, and the QEMU exit/shutdown result. Artifact preparation, Nix evaluation, machine startup, activation, guest assertion, unexpected guest exit, shutdown, and timeout failures are intentionally nonzero and fail closed.

Each driver run owns a fresh temporary rootfs overlay, OVMF variables copy, and ESP. The immutable Nix-store rootfs and firmware inputs are never reused as writable state. A retry needs no manual overlay cleanup.

The shared `apps/starry/nixos/qemu-x86_64.toml` sets `cmdline = "init=/init"`
to select the generated NixOS initializer on the root disk. The application
path uses `ostool` to write `EFI/BOOT/cmdline.txt`; the independent framework's
`prepare_launch_plan()` reads the same field and writes that file to its own
ESP. `someboot::efi_stub::load_efi_payload()` passes the command line to
StarryOS's `init_options()`, which selects `/init` for the block root.

## 4. Author a case

Add a file under `cases/` whose stem matches `^[a-z][a-z0-9-]{0,62}$`. Copy `cases/hello-tmpfiles.nix` as the template. A record is:

```nix
{
  kind = "assert"; # boot | assert | unsupported
  extraModules = [ ../modules/hello-tmpfiles.nix ];
  command = "cat /etc/starry-nixos/hello-tmpfiles";
  expectedStatus = 0;
  expectedOutput = "tmpfiles-ok";
  expectPass = true;
}
```

Do not write a `testScript` and do not call `machine.succeed`. Extra modules must not re-enable udev, dbus, nscd, logind, getty, DHCP, or the Nix daemon. Cases are in-tree only in this slice. After adding the file:
Function-form NixOS modules are supported and are evaluated by `mkNixosSystem` before the final baseline assertions inspect `config`. A function module that enables a forbidden option still fails during system evaluation; a function module that only declares allowed state is accepted. Extra modules also cannot clear the Stage-2 `kernel.pid_max` and `vm.max_map_count` assertions: `function-forbidden` still fails after `assertions = lib.mkForce [ ]`, and `sysctl-forbidden` fails when an extra module writes `kernel.pid_max`. The `nixos-tests/starryos/flake.nix` interface check covers these cases with `function-allowed`, `function-forbidden`, and `sysctl-forbidden`.

```bash
cargo xtask starry app qemu -t nixos --list-cases
cargo xtask starry app qemu -t nixos --cap nix --case <stem>
```

A failing case sets `expectPass = false`. Zero exit on that case is a framework bug. Illegal stems fail listing. Unknown `--case` names fail with the discovered catalog.

## 5. Retained compatibility paths

The nixosTest entry point does not replace these existing checks:

```bash
apps/starry/nixos/build-rootfs.sh --self-test
cargo xtask starry app qemu -t nixos --arch x86_64 --cap nix
cargo xtask starry test qemu --arch x86_64 -c qemu/system/starrynixos-stage2
```

## 6. Diagnostics and limits

A failure before QEMU indicates kernel preparation, NAR import, flake evaluation, rootfs, firmware, or launch-adapter setup. Look for `STARRY_NIXOS_PHASE_FAILED=` in driver/xtask output:

- `artifact-preparation`
- `machine-startup`
- `stage2-activation`
- `guest-assertion`
- `unexpected-guest-exit`
- `shutdown`
- `timeout`

Guest commands are declared systemd oneshots observed on serial, not `machine.succeed`. Shared marker poweroff still happens unless the test-owned `/etc/starry-nixos/keep-running` file is present. There is still no claim that unmodified upstream NixOS tests run on StarryOS, and CI scheduling is unchanged.

## 7. Validation records

The latest TCG identities, assertion blocks, phase names, and results from three isolated `service-fail` runs are recorded in `nixos-tests/starryos/compatibility.md`.

- `cargo xtask starry app qemu -t nixos --list-cases` lists the x86_64 cases in `cases/*.nix` without starting a VM.
- `python3 -m unittest discover -s nixos-tests/starryos -p 'test_*.py' -v` covers the launcher and `starry_machine` contracts.
- TCG `hello-tmpfiles` reports `STARRY_NIXOS_ASSERT_PASSED` after the P1 marker and exits with status 0.
- TCG `service` reports `STARRY_NIXOS_ASSERT_PASSED` after the P1 marker and exits with status 0.
- TCG `service-fail` exits with a nonzero status, reporting `STARRY_NIXOS_PHASE_FAILED=guest-assertion` and `declared command false exited 1`.
- TCG `unsupported` immediately exits with a nonzero status and `unsupported Starry nixosTest operation: succeed`, without waiting for `/dev/hvc0`.
- TCG `boot` completes the ordered markers and shutdown.

## 8. Network-restricted hosts

If requests to the GitHub image registry or for flake content time out, and a local SOCKS5 proxy listens on `127.0.0.1:7890`, wrap commands that need network access with `proxychains4`:

```bash
proxychains4 cargo xtask starry app qemu -t nixos --cap nix --case boot
proxychains4 cargo xtask starry app qemu -t nixos --cap nix --case boot --arch x86_64
```

`proxychains4` applies to host downloads; it does not change the guest's QEMU, TCG, serial-console, or shutdown behavior. Do not inject `proxychains4` into the guest's `qemu-x86_64` userspace prebuild command: that command uses the Starry test rootfs's dynamic-linking environment, where incompatible glibc symbols in the proxy library can cause the prebuild to fail.
