# StarryOS Loadable Kernel Modules

This directory contains loadable kernel modules (`.ko`) used by StarryOS.

`cargo xtask starry kmod build` discovers modules under this directory by
default. It supports two module forms:

- Rust kmods: a Cargo package with `Cargo.toml`, for example `hello/`.
- Linux C kmods: a Kbuild `Makefile` with `obj-m`, for example `linux-hello/`.

This document focuses on Rust kmods.

## Rust Module Layout

A Rust kmod is a normal workspace package:

```text
os/StarryOS/lkm/<module>/
├── Cargo.toml
└── src/
    └── lib.rs
```

The crate is compiled as a library first. axbuild then partial-links the
produced `rlib` into an ET_REL `.ko` with
`os/StarryOS/scripts/kmod-linker.ld`.

The module source should be `no_std`:

```rust
#![no_std]

use kmod_tools::{exit_fn, init_fn, module};

#[init_fn]
fn init() -> i32 {
    0
}

#[exit_fn]
fn exit() {}

module!(
    name: "my_module",
    license: "GPL",
    description: "My StarryOS module",
    version: "0.1.0",
);
```

Use `extern "C"` declarations for kernel shim symbols that are resolved
through StarryOS kallsyms at load time.

## Required Dependencies

A Rust kmod should use workspace dependencies so the module and kernel are built
from the same crate graph:

```toml
[dependencies]
kmod-tools.workspace = true
starry-kernel = { workspace = true, features = [...] }
ax-runtime = { workspace = true, features = [...] }
ax-std = { workspace = true, features = [...] }
```

Add lower-level ArceOS crates such as `ax-hal`, `ax-driver`, or `axplat-dyn`
only when the module directly uses their APIs. Do not use module features to
select a platform path; the kernel build always provides `axplat-dyn`.

Do not depend on a different version of `starry-kernel`, `ax-std`, `ax-runtime`,
or platform crates. A Rust kmod may contain undefined Rust `core`/`alloc` and
kernel symbols that must match the kernel ELF exactly.

## Feature Rule

The Rust kmod feature set must be compatible with the kernel that will load it.
In practice, the module's effective Starry/ArceOS feature requirements should
be a subset of, or equal to, the kernel build's enabled capabilities; when in
doubt, make the module package expose the same board and device features as
`starryos` and forward them to the same crates.

The dynamic platform path is mandatory and comes from the kernel build context.
Modules should only forward optional capabilities they actually need, for
example SMP:

```toml
[features]
default = []
smp = ["ax-runtime/smp", "ax-hal/smp"]
```

The same applies to board and device features. If a module needs a symbol or
type behind `starry-kernel/input`, `starry-kernel/vsock`, `ax-driver/rknpu`, or
similar feature gates, the kernel must be built with compatible support.

`cargo xtask starry kmod build` starts from the normal Starry build selectors
(`--arch`, `--target`, `--config`, `--smp`, `--debug`) and reuses the resolved
Cargo target, environment, and normalized feature set. The
module's package features are then applied on top of that context.

## LTO Requirement

Release LTO must remain disabled for kernels that load Rust kmods. With LTO,
the final kernel link can inline, internalize, rename, or remove Rust symbols
that are not needed by the kernel's own closed-world call graph. A module that
later references the original symbol name will then fail kallsyms resolution at
`insmod` time.

This repository currently sets:

```toml
[profile.release]
lto = false
```

Do not enable workspace or command-line release LTO for kmod-capable kernel
images.

## Build

Build the kernel first:

```sh
cargo xtask starry build --arch <arch>
```

Then build one module:

```sh
cargo xtask starry kmod build --arch <arch> -m os/StarryOS/lkm/hello
```

Or build all modules under this directory:

```sh
cargo xtask starry kmod build --arch <arch> --all
```

The resulting `.ko` files are written next to the Starry kernel ELF under the
Cargo target directory, for example:

```text
target/<target>/release/hello.ko
```

Use `--rootfs <image>` to inject built modules into `/modules/` in a rootfs
image.

## Symbol Checks

If a module fails to load with an unknown symbol, compare the module's
undefined symbols with the kernel ELF:

```sh
rust-nm -u target/<target>/release/<module>.ko
rust-nm -n target/<target>/release/starryos
```

The unresolved module symbol must exist in the kernel ELF with the exact same
name.


## kmod app migration

| kmod        | x86_64             | riscv64            | aarch64            | loongarch64        |
| ----------- | ------------------ | ------------------ | ------------------ | ------------------ |
| hello       | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| kprobe_test | :white_check_mark: | :white_check_mark: | :white_check_mark: | :white_check_mark: |
| linux-hello |                    | :white_check_mark: | :white_check_mark: | :white_check_mark: |
