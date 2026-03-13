# Changelog

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
