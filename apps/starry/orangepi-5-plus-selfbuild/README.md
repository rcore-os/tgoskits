# StarryOS self-build on OrangePi 5 Plus

This independent board application reuses the AArch64 self-build flow without
changing the existing macOS or x86_64 applications. It runs on a physical
OrangePi 5 Plus and does not use KVM/HVF. Physical-board control uses the direct
1,500,000-baud UART and a Linux-selected, verified one-time U-Boot script; it
does not invoke the OSTool board runner or interrupt U-Boot autoboot.

The correctness milestone is a compile closure: StarryOS builds an AArch64
`starryos` ELF and raw binary inside a reusable Debian 12 arm64 glibc chroot,
then Linux retrieves and verifies both artifacts and their SHA-256 hashes.
Booting the self-built second-generation kernel is intentionally out of scope.

See [README_CN.md](README_CN.md) for the complete provisioning, recovery,
benchmark, and profiling workflow. The end-to-end entry is:

```bash
apps/starry/orangepi-5-plus-selfbuild/run_selfbuild.sh \
  --host <BOARD_IP>
```

For an interactive console, use:

```bash
apps/starry/orangepi-5-plus-selfbuild/connect_serial.sh
```

This starts picocom on `/dev/ttyACM0` at 1,500,000 baud after disabling
bracketed paste in the host terminal. Without that reset, a terminal left in
bracketed-paste mode sends `ESC[200~` and `ESC[201~` to the board shell as part
of each paste. Pass one serial device path to select another adapter. Exit
with Ctrl+A, then Ctrl+X before running an automated board workload.
If the board's `/bin/sh` is dash, run `bash --noprofile --norc -i` at the board
prompt for command-line editing and bracketed-paste handling. A Linux Bash
prompt may enable bracketed paste again before rebooting into StarryOS dash
within the same picocom session.
`tests/script_smoke.sh` includes a regression that sends pasted input through
real picocom and two PTYs; it requires picocom for that check.

The guest command has a 21,600-second userspace timeout. There is no automatic
kernel-deadlock reset: if the kernel stops scheduling, the serial monitor can
report a timeout, but the board requires manual recovery. The Starry shell
restores the verified Linux boot script before starting the workload, so a later
manual reset returns to Linux. The guest uses the system-default CPU affinity and
parallelism: it first builds the debug `tg-xtask` host runner with plain
`cargo build -p tg-xtask`, then invokes that exact binary to build
StarryOS from the application build config. It emits minute-level compile-unit
progress markers for Linux/StarryOS comparison. The seed kernel remains a
separate build using `cargo xtask starry build` with the board configuration.
Profiling is deliberately bounded to the first command: `--profile stat` or
`--profile record` measures at most 300 seconds of `cargo build -p tg-xtask`
and exits without starting the StarryOS build. `record` uses flat 49 Hz cycle
samples because the current StarryOS perf ABI does not support call-chain
samples. The Linux and StarryOS runs keep the same system-default parallelism.
The one-time boot selects the Linux root partition by GPT `PARTUUID`; Linux,
StarryOS, and U-Boot do not share stable MMC device numbers. The end-to-end
entry drives the UART, waits for Linux to return, and fetches and verifies the
output artifacts before reporting success.

## 1. Kernel build timing

`guest-kernel-selfbuild.sh` measures the StarryOS build using the prepared
`/usr/local/bin/tg-xtask`, excluding the task tool's own build time. It requires
an absent source `target` directory and a new output run directory, checks the
installed toolchain's `llvm-objcopy` before timing, and retains both build
products and failure evidence. Cargo registry sources remain installed for
offline compilation; compiled dependencies must be absent.

### 1.1 Cold build preparation

In board Linux, preserve the prepared task executable outside `target` and move
the old source `target` directory into a uniquely named backup before rebooting.
Verify the executable and LLVM tools inside the build chroot. The benchmark
keeps the source archive, Rust toolchain, build configuration and default Cargo
parallelism fixed between kernel versions.

### 1.2 Runtime evidence

After StarryOS boots, run the dedicated entry with a fresh run name:

```sh
sh /opt/starry-orangepi5plus-selfbuild/init-kernel-selfbuild.sh performance-dev-cold
```

`init-kernel-selfbuild.sh` restores the verified Linux boot script, mounts the
build environment's proc/dev/sys directories and enters its Bash. The guest
records tool and artifact hashes, source metadata, CPU affinity, available
frequency settings and wall time under `/output/runs/<run-name>`. The UART
driver's `--kernel-only` option selects this entry. A pass requires the task
command to succeed and both the AArch64 ELF and nonempty raw binary to exist.
The driver sends the command once; returning to the shell without a terminal
marker fails the run instead of restarting it with compiled dependencies.
Linux must subsequently retrieve the artifacts and verify `SHA256SUMS`.
