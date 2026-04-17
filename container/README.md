# StarryOS Container Image

This folder contains a Docker image definition for StarryOS development.

Included toolchains:

- Ubuntu 24.04 base image (minimal packages with `--no-install-recommends`)
- QEMU `10.2.1` (built from source, with `aarch64/riscv64/loongarch64/x86_64` system and linux-user targets)
- `qemu-user-static` plus source-built linux-user QEMU binaries for guest-user execution helpers
- Rust toolchain from the repository root `rust-toolchain.toml`
- musl cross compilers for `aarch64/riscv64/loongarch64/x86_64`

## Build

```bash
docker build -t starryos-dev:ubuntu-qemu10.2.1 -f container/Dockerfile .
```

## Run

```bash
docker run -it --rm -v "$(pwd)":/workspace -w /workspace starryos-dev:ubuntu-qemu10.2.1
```

Then you can build StarryOS inside the container, for example:

```bash
cd os/StarryOS
make ARCH=riscv64 build
```
