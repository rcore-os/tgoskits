# Changelog

## 0.3.1

### Bug Fixes

- Use `TPIDRPRW` instead of `TPIDRURO` as the per-CPU pointer register for ARM32 (https://github.com/arceos-org/percpu/pull/24).

## 0.3.0

### New Features & Breaking Changes

- Add custom per-CPU area support (https://github.com/arceos-org/percpu/pull/22)
- API changes:
  + `init()` -> `init(base, count)`: the base address of per-CPU area and number of CPUs have been added as parameters.
  + the old `init()` function is renamed to `init_in_place()`.

## 0.2.2

### Bug Fixes

- Use `percpu_symbol_vma!` to get the VMA of `_percpu_load_start` to avoid incorrect pointer-non-zero optimization (https://github.com/arceos-org/percpu/pull/19).

## 0.2.1

### New Features

- Introduce feature `non-zero-vma` to use a non-zero address for the `.percpu` section to fix a linker issue when testing under linux (https://github.com/arceos-org/percpu/pull/16).
- Add ARMv7A (32-bit) target support (https://github.com/arceos-org/percpu/pull/15).

## 0.2.0

### Breaking Changes

- API changes:
  + `get_local_thread_pointer()` -> `read_percpu_reg()`
  + `set_local_thread_pointer()` -> `write_percpu_reg()`
  + `init(max_cpu_num: usize)` -> `init() -> usize`
  + Add `init_percpu_reg()`.
  + Add `percpu_area_num()`.

### Other Changes

- Make sure the percpu data area is initialized only once.
- Automatically detect number of CPUs in `percpu::init`.
- x86_64:
    + Use `mov` instruction instead of `movabs` to get the per-CPU variable offset.

## 0.1.7

### New Features

- Add LoongArch64 support (https://github.com/arceos-org/percpu/pull/10).

## 0.1.6

### Minor Updates

- Export dummy `percpu_area_base` for the `sp-naive` feature. (https://github.com/arceos-org/percpu/pull/9)

## 0.1.5

### New Features

- Add accessors to remote CPUs. (https://github.com/arceos-org/percpu/pull/7)

## 0.1.4

### New Features

- Add feature `arm-el2` to run in the AArch64 EL2 privilege level. (https://github.com/arceos-org/percpu/pull/2, https://github.com/arceos-org/percpu/pull/3)

## 0.1.0

Initial release.
