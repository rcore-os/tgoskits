# Adding StarryOS QEMU Test Cases

This guide explains how to add new test cases to `test-suit/starryos/`.

## Directory Layout

```
test-suit/starryos/
  normal/                     # Standard test cases (run on every push)
    smoke/                    # Case: basic boot check
      qemu-<arch>.toml        # One TOML per architecture
    usb/                      # Case: USB device test (with C source)
      c/
        CMakeLists.txt
        prebuild.sh           # Optional: runs apk add inside rootfs
        src/
          main.c
      qemu-<arch>.toml
    helloworld/               # Case: simple C program (no prebuild needed)
      c/
        CMakeLists.txt
        src/
          main.c
      qemu-<arch>.toml
  stress/                     # Stress test cases (run on PRs to main)
    stress-ng-0/
      qemu-<arch>.toml
```

## Quick Start: Pure Shell Test

If your test only needs the shell (no compiled binaries), create:

```
test-suit/starryos/normal/<case>/qemu-<arch>.toml
```

Example (`smoke/qemu-riscv64.toml`):

```toml
args = [
    "-nographic", "-cpu", "rv64",
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", "id=disk0,if=none,format=raw,file=${workspace}/target/riscv64gc-unknown-none-elf/rootfs-riscv64.img",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
]
uefi = false
to_bin = true
shell_prefix = "root@starry:"
shell_init_cmd = "pwd && echo 'All tests passed!'"
success_regex = ["(?m)^All tests passed!\\s*$"]
fail_regex = ['(?i)\bpanic(?:ked)?\b']
timeout = 15
```

## C Test Cases

### 1. Create the directory structure

```
test-suit/starryos/normal/<case>/
  c/
    CMakeLists.txt     # Required: build definition
    prebuild.sh         # Optional: install packages into rootfs
    src/                # Your source files
  qemu-<arch>.toml     # One per supported architecture
```

### 2. Write `CMakeLists.txt`

The build system uses clang cross-compilation with the rootfs as sysroot.
Your executable will be installed into `/usr/bin/` inside the guest.

```cmake
cmake_minimum_required(VERSION 3.20)
project(mytest C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(mytest src/main.c)
target_compile_options(mytest PRIVATE -Wall -Wextra -Werror)

install(TARGETS mytest RUNTIME DESTINATION usr/bin)
```

### 3. Optional: `prebuild.sh`

If your test needs packages installed in the rootfs (e.g., libraries):

```sh
#!/bin/sh
set -eu

apk add gcc musl-dev libusb-dev   # or whatever you need
```

This runs inside the rootfs via qemu-user, so you can use `apk add` normally.

### 4. Write `qemu-<arch>.toml`

Set `shell_init_cmd` to the installed binary path:

```toml
shell_init_cmd = "/usr/bin/mytest"
```

Copy the QEMU args from an existing case (e.g., `smoke/qemu-<arch>.toml`) and adjust if needed.

### 5. Supported architectures

| Arch       | Target                              | QEMU CPU    |
|------------|-------------------------------------|-------------|
| x86_64     | x86_64-unknown-none                 | (default)   |
| aarch64    | aarch64-unknown-none-softfloat      | cortex-a53  |
| riscv64    | riscv64gc-unknown-none-elf          | rv64        |
| loongarch64| loongarch64-unknown-none-softfloat  | la464       |

Only create `qemu-<arch>.toml` for architectures where the test is verified to pass.

## TOML Reference

| Field           | Type            | Description |
|-----------------|-----------------|-------------|
| `args`          | `[string]`      | QEMU command-line arguments. `${workspace}` is expanded to the repo root. |
| `uefi`          | `bool`          | Use UEFI boot (false for most cases) |
| `to_bin`        | `bool`          | Convert ELF to raw binary with objcopy |
| `shell_prefix`  | `string`        | Prompt pattern to wait for before sending commands |
| `shell_init_cmd`| `string`        | Command sent to the guest shell |
| `success_regex` | `[string]`      | All must match for PASS (multiline regex) |
| `fail_regex`    | `[string]`      | If any matches, test fails immediately |
| `timeout`       | `integer`       | Seconds before the test is marked as failed |

## Running Tests

```bash
# Run all normal tests for an architecture
cargo starry test qemu -t riscv64

# Run a specific test case
cargo starry test qemu -t riscv64 -c helloworld

# Run stress tests
cargo starry test qemu --stress -t riscv64
```

## Tips

- Keep `fail_regex` narrow. Avoid patterns that match benign output like `failed: 0`.
- Use `success_regex` from a stable, unique success line in your output.
- For slow tests, increase `timeout` only after confirming the command still makes progress.
- Binary dependencies installed via `prebuild.sh` are cross-compiled from the staging rootfs, so standard Alpine packages work.
- Do not run multiple `cargo starry test qemu` commands in parallel in one workspace.
