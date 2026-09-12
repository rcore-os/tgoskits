# Redcola StarryOS AI Control Demo

This case runs a small deterministic AI-control workload as a Linux-user-mode
program inside the StarryOS QEMU guest. It is intended as bonus evidence for the
Quancheng Laboratory 2026 AxVisor contest, where StarryOS is preferred over a
standard Linux non-RT guest.

The guest program compares a fixed manual PWM baseline against a fixed-point
neural-network control policy over eight samples. The network is intentionally
tiny and deterministic for reproducibility: a 4-input, 4-hidden-unit ReLU MLP
computes a PWM command from demand, load, vibration, and bias features before
the simulated plant reports the tracking error. A successful run prints:

```text
REDCOLA_STARRY_AI_CONTROL_PASS samples=8 manual_abs_error=1013 ai_abs_error=0
```

## Build and Run

From the repository root:

```sh
cargo xtask starry app qemu -t qemu/redcola-ai-control --arch aarch64
```

This case is a Rust Starry app case. Its `prebuild.sh` is intentionally small:
it only writes `rust/src/prebuild_marker.txt`, which `build.rs` embeds so the
guest log can prove that the prebuild hook ran. The actual Rust cross-build and
rootfs installation are handled by the normal Starry app runner invoked by
`cargo xtask starry app qemu`. That runner builds the static AArch64 musl
program from `rust/` and injects the generated overlay so the guest sees the
program as `/usr/bin/redcola-ai-control`.

The QEMU config then runs `/usr/bin/redcola-ai-control` as the shell init
command and treats the final DONE line as the success marker, after the full
PASS metrics line and `prebuild_marker=prebuild-ok` have already been printed.

## Environment Note

The StarryOS kernel build depends on the repository's normal AArch64 bare-metal
toolchain. On minimal Kali images without `aarch64-linux-musl-gcc`, the local
validation used a temporary clang-based freestanding wrapper outside the git
tree under `/tmp/redcola-toolchain/bin`, plus a tiny temporary sysroot under
`/tmp/redcola-freestanding-sysroot`. Those files are not part of this case.

Minimal Kali validation command after preparing that wrapper:

```sh
SYS=/tmp/redcola-freestanding-sysroot
RES=$(clang -print-resource-dir)
export PATH=/tmp/redcola-toolchain/bin:$PATH
export BINDGEN_EXTRA_CLANG_ARGS="-nostdinc -isystem $SYS/include -isystem $RES/include"
cargo xtask starry app qemu -t qemu/redcola-ai-control --arch aarch64
```

The wrapper must provide `aarch64-linux-musl-gcc -print-sysroot` and compile
freestanding AArch64 C sources with clang. A normal distro-provided
`aarch64-linux-musl-gcc` toolchain can be used instead.

Validation evidence should record the full QEMU log, its SHA-256 digest, the
full PASS metrics line, and the final DONE marker used by the QEMU runner. A
passing MLP run contains lines like:

```text
REDCOLA_STARRY_AI_BEGIN guest=StarryOS role=non_rt_guest model=fixed_point_mlp_policy hidden=4
REDCOLA_STARRY_CONTROL_SUMMARY manual_abs_error=1013 ai_abs_error=0 max_ai_error=0
REDCOLA_STARRY_AI_CONTROL_PASS samples=8 manual_abs_error=1013 ai_abs_error=0
REDCOLA_STARRY_AI_DONE
```
