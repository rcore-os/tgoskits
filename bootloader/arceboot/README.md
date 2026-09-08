# ArceBoot

ArceBoot is a cross-platform operating system bootloader that reuses
[tgoskits](https://github.com/rcore-os/tgoskits) (ArceOS) components. It
provides a UEFI runtime over the ArceOS hardware abstraction so that
next-stage UEFI bootloaders and operating systems can be loaded from a
ramdisk or disk.

Originally developed in the [rustsbi](https://github.com/rustsbi/rustsbi)
repository, ArceBoot was migrated to tgoskits in September 2026 with the
permission of both communities (see `LICENSE` for the original RustSBI
copyright).

## Build

```bash
# default features: alloc + paging + ramdisk_cpio
$ scripts/build-arceboot.sh

# with network and display support
$ AX_FEATURES="net,display" scripts/build-arceboot.sh

# debug build / custom log level
$ AX_PROFILE=dev AX_LOG=trace scripts/build-arceboot.sh
```

The script produces `target/riscv64gc-unknown-none-elf/<profile>/arceboot.bin`.

Build-time configuration is passed through environment variables (see
`src/config.rs`):

| Variable | Meaning | Default |
|---|---|---|
| `AX_USE_RAMDISK` | Enable ramdisk support (`1`) | off |
| `AX_RAMDISK_START` | Physical start of a pre-loaded ramdisk (hex) | `0x84000000` |
| `AX_RAMDISK_SIZE` | Pre-loaded ramdisk size in bytes (hex) | `0x40000000` |
| `SOMEBOOT_RISCV64_KERNEL_LOAD_PADDR` | riscv64 load address | `0x80200000` |

## Feature status

| Feature | Status |
|---|---|
| `alloc` | Working |
| `paging` | Working |
| `ramdisk_cpio` | Working |
| `net` | Device probing only; the ax-net queue runtime needs IRQ-driven executors, which a bootloader without interrupt support cannot provide |
| `display` | Compiles; boot-time IRQ handling is not wired yet |
| `fs` / `virtiodisk` | Removed with the migration; the upstream code used the
legacy `axdriver`/`axfs` stack. Porting it back requires a virtio-blk rdif
driver plus an `ax-fs-ng` runtime adapter (the `axruntime/src/fs/block.rs`
glue is the reference). The original code is preserved in the imported
history and in the rustsbi repository. |

## Porting notes (rustsbi -> tgoskits)

The migration adapted the crate to the tgoskits workspace:

- **Crate renames**: the ArceOS crates were renamed in tgoskits
  (`axhal` -> `ax-hal`, `axdriver` -> `ax-driver`, `axmm` -> `ax-mm`,
  `axalloc` -> `ax-alloc`, `axio` -> `ax-io`, `axplat` -> `ax-plat`, ...).
- **Startup sequence**: rewritten to the tgoskits `axruntime` pattern
  (`ax_hal::percpu::init_primary` + `ax_alloc::init_percpu_slab` +
  `ax_hal::init_early` + `ax_mm::init_memory_management` +
  `ax_hal::init_later` + `rdrive::probe_all`).
- **`axconfig` crate**: tgoskits has no `axconfig`; the boot knobs moved to
  `src/config.rs` and are read from environment variables.
- **Runtime bridges**: the `ax-sync` interface (`SpinOps`/`RwLockOps`/
  `MutexOps`/...) and the `axklib::Klib` trait are implemented locally in
  `src/sync.rs` and `src/klib.rs` (ArceBoot is single-core with no IRQ
  handling, so the locks are plain atomic spins).
- **Linker script**: the image links with the tgoskits platform script chain
  `axplat.x` (axplat-dyn -> somehal -> someboot, wired up by `build.rs`);
  `link.ld` is the upstream script kept for reference only.
- **`uefi-raw`**: bumped 0.11 -> 0.14 to unify with `axloader` (the 0.14 ABI
  marks service functions `unsafe extern "efiapi"` and uses `Boolean(u8)`).
- The upstream `configs/` (defconfig + platform tomls), `tests/` and
  `scripts/test/` are kept unmodified for reference and future test-suit
  work.
