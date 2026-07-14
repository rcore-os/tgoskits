# StarryOS kmod axbuild migration

This document records the migration plan for building StarryOS loadable kernel
modules in the tgoskits axbuild flow.

## Background

The upstream StarryOS kmod build flow is Makefile-based:

1. Discover module crates under `modules/`.
2. Re-enter the normal kernel build scripts with `KMOD=y APP=<module>`.
3. Compile the module crate with the same target, features, target directory,
   rustflags, and environment as the kernel build.
4. Convert the module crate rlib into an ET_REL `.ko` with:

   ```text
   rust-lld -flavor gnu -r -T modules/kmod-linker.ld \
       -o <module>.ko --whole-archive lib<module>.rlib \
       --strip-debug --build-id=none --gc-sections -no-pie
   ```

The important property is not the Makefile itself. The important property is
that the module build shares the same kernel build context, so Cargo can reuse
the same compiled `starry-kernel`, `ax-runtime`, platform, and ArceOS dependency
artifacts whenever the compilation inputs match.

The existing tgoskits `cargo xtask starry kmod build` implementation bypassed
that context by directly running `cargo build --manifest-path`. That loses the
Starry-specific target JSON, feature normalization, platform
selection, and kallsyms-compatible kernel configuration, so it is not a correct
migration of the upstream flow.

## Target design

Treat kmod building as a Starry build mode, not as a separate Cargo shortcut.

The `starry kmod build` command should:

1. Accept the same build selectors as `starry build`: `--config`, `--arch`,
   `--target`, `--smp`, and `--debug`.
2. Resolve those selectors through the normal Starry request path.
3. Load the normal Starry Cargo configuration with `build::load_cargo_config`.
4. Clone that Cargo configuration for each module package.
5. Change only the package/bin/output-specific fields needed for module
   compilation:
   - set `package` to the module package name,
   - clear the `starryos` binary selection,
   - disable ELF-to-bin conversion,
   - remove Starry kernel-only kallsyms and uimage post-processing hooks.
6. Keep the target, features, Cargo args, and environment intact. In
   particular, preserve `AX_PLATFORM`, `AX_ARCH`, `AX_TARGET`, `SMP`, target
   JSON arguments, and build-std arguments.
7. Locate the produced module rlib from Cargo metadata, then partial-link it
   with `os/StarryOS/scripts/kmod-linker.ld`.
8. Write `.ko` files next to the kernel ELF under `target/<target>/release/`.

Module discovery uses `os/StarryOS/lkm` as the default tree. `--module <path>`
can point either at one module crate or at a directory to scan recursively.

## Runtime contract

The kernel side already provides the kmod loader, syscall glue, shim symbols,
and kallsyms-backed symbol resolution. A valid `.ko` is expected to remain a
relocatable ELF with unresolved symbols that are resolved at load time through
the in-kernel kallsyms table.

The kernel image itself must still be built by the normal Starry axbuild flow so
the `.kallsyms` section is generated before testing positive module loading.

### LTO and exported Rust symbols

StarryOS kmods currently rely on exact kallsyms lookup for undefined symbols in
the module ELF. This includes not only explicit kernel shim symbols such as
`write_char`, but also Rust `core` and `alloc` symbols that the module crate may
reference after normal Rust code generation.

Workspace-level `lto = true` is incompatible with that assumption. During the
final kernel link, LLVM treats the kernel as a closed-world binary. Symbols that
are not needed by the kernel's own final call graph may be inlined,
internalized, renamed, or removed entirely. The kmod is not part of that final
LTO unit, so future runtime references from `.ko` files are invisible to the
optimizer.

A concrete failure mode is a module referencing a Rust formatting symbol such
as:

```text
_RNvMs5_NtNtC..._4core3fmt8buildersNtB5_9DebugList5entry
```

If the kernel ELF no longer contains the same global symbol after LTO, the
symbol is absent from `.kallsyms` and `insmod` fails with `unknown symbol in
module`. Even if an equivalent implementation remains in the kernel as a local
or LTO-renamed symbol, the loader cannot use it because kallsyms resolution is
an exact string match.

When investigating this class of failure, do not change the module
implementation first. Check the symbol contract directly:

```sh
rust-nm -u target/<target>/release/<module>.ko
rust-nm -n target/<target>/release/starryos
```

The undefined symbol name in the `.ko` must exist as the same global symbol in
the kernel ELF. A different Rust crate disambiguator in the mangled name means
the module and kernel were not built against the same exported Rust symbol set.

The required rule is therefore to build both the Starry kernel image and its
modules with release LTO disabled. This repository currently sets the workspace
release profile to:

```toml
[profile.release]
lto = false
```

Do not enable workspace or command-line release LTO for kernel images that are
expected to load `.ko` files. Building only the module with LTO disabled cannot
restore symbols that were already removed from a kernel ELF built with release
LTO.

## Rootfs integration

Module image installation uses the existing axbuild rootfs injection helpers
instead of reintroducing sudo mount/copy logic. Pass `--rootfs <image>` to
`starry kmod build` to install the produced modules into the image. The guest
path is:

```text
/modules/<module>.ko
```

The rootfs injection step may create a short-lived overlay directory in the
system temporary directory, but it is removed automatically after injection and
is not a build artifact.

Example:

```sh
cargo xtask starry kmod build --arch riscv64 \
    -m os/StarryOS/lkm/hello \
    --rootfs tmp/axbuild/rootfs/kmod-test-riscv64.img
```

## Validation

Minimum validation after implementation:

1. Build the kernel with release LTO disabled:

   ```sh
   cargo xtask starry build --arch <arch>
   ```

   This step is required even if modules are built separately. The kernel ELF
   must be generated with release LTO disabled; otherwise Rust `core`/`alloc`
   symbols needed by modules may be absent from `.kallsyms`.

2. Build the module with the same Starry selectors:

   ```sh
   cargo xtask starry kmod build --arch <arch> -m os/StarryOS/lkm/hello
   ```

   The resulting `.ko` is written next to the kernel ELF:

   ```text
   target/<target>/release/starryos
   target/<target>/release/hello.ko
   ```

3. Inspect the module ELF:

   ```sh
   rust-readobj --file-headers target/<target>/release/hello.ko
   rust-nm -u target/<target>/release/hello.ko
   ```

   The file must be an ET_REL relocatable ELF. Undefined symbols are expected,
   because they are resolved by the kernel loader through kallsyms.

4. Compare module undefined symbols against the kernel ELF before changing the
   module implementation:

   ```sh
   rust-nm -u target/<target>/release/hello.ko
   rust-nm -n target/<target>/release/starryos
   ```

   Every required `.ko` symbol that is not provided by the module itself must
   have the same global symbol name in the kernel ELF. In particular, Rust
   mangled symbols must have the same crate disambiguator.

5. Validate rootfs integration when testing `insmod`:

   ```sh
   cargo xtask starry kmod build --arch <arch> \
       -m os/StarryOS/lkm/hello \
       --rootfs <rootfs.img>
   ```

   The module should appear in the guest as:

   ```text
   /modules/hello.ko
   ```

6. Run a positive loader test in the guest:

   ```sh
   insmod /modules/hello.ko
   rmmod hello
   ```

   The expected result is that `insmod` resolves all undefined symbols, the
   module init function runs, and `rmmod` unloads the module successfully.

## Quick start

If `tmp/axbuild/config/starryos/build-<target>.toml` does not exist, the Starry
build and kmod commands generate it from the default qemu board config for the
selected target. This is required for dynamic-platform targets such as riscv64,
because the plain target default does not contain the virtio and driver
features needed by QEMU.

1. Ensure the rootfs image exists at `tmp/axbuild/rootfs/rootfs-<target>-alpine.img`. If not, build it with:

```sh
cargo xtask starry rootfs --arch <arch>
```

2. Build the module with the same Starry selectors and inject it into the rootfs:

```sh
cargo xtask starry kmod build --arch riscv64 \
    -m os/StarryOS/lkm/hello \
    --rootfs tmp/axbuild/rootfs/rootfs-riscv64-alpine.img/rootfs-riscv64-alpine.img
```

3. Build and boot the kernel with QEMU and the injected rootfs:

```sh
cargo xtask starry qemu --arch riscv64
```

4. Run a positive loader test in the guest:

```sh
insmod /modules/hello.ko
rmmod hello
```
