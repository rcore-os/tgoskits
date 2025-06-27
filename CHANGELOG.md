# Changelog

## 0.5.4

## New Features

- [Fix invalid query result](https://github.com/arceos-org/page_table_multiarch/pull/17).
- [Fix incorrect TLB flush VA bits on aarch64](https://github.com/arceos-org/page_table_multiarch/pull/21).
- [Introduce feature `copy-from` and fix page table drop after `copy-from`](https://github.com/arceos-org/page_table_multiarch/pull/20).

## 0.5.3

### New Features

- Add `empty` method to page table entries for all architectures.

## 0.5.2

### Minor Changes

- [Make LoongArch64's page table default to 4 levels](https://github.com/arceos-org/page_table_multiarch/pull/12).
- [Do not link to alloc crate](https://github.com/arceos-org/page_table_multiarch/pull/13).
- [Implement `Clone` and `Copy` for `PagingError`](https://github.com/arceos-org/page_table_multiarch/pull/14).

## 0.5.1

### LoongArch64

- [Add LoongArch64 support](https://github.com/arceos-org/page_table_multiarch/pull/11).

## 0.5.0

### Breaking Changes

- Upgrade to Rust edition 2024, which requires Rust v1.85 or later.

## 0.4.2

- Fix [x86_64](https://crates.io/crates/x86_64) dependency version as v0.15.1.

## 0.4.1

### RISC-V

- Add trait `SvVirtAddr` for custom virtual address types.

## 0.4.0

### Breaking Changes

- Update `memory_addr` to `0.3.0`, which is not backward compatible with `0.2.0`.

## 0.3.3

- Support the use of `page_table_entry` at the ARM EL2 privilege level (via the `arm-el2` feature).

## 0.3.2

- Fix the Rust documentation for `TlbFlush` and `TlbFlushAll`.

## 0.3.1

- Allow generic virtual address types in `PageTable64`.

## 0.3.0

### New Features

- Allow users to control the TLB flush behavior.
    + Return structures `TlbFlush`/`TlbFlushAll` after mapping change (e.g., call `PageTable64::map`).
    + Add a parameter `flush_tlb_by_page` to `map_region`/`unmap_region`/`protect_region` in `PageTable64`.

## 0.2.0

### New Features

- No longer collect intermediate tables into a `Vec`, walk the page table and
deallocate them on drop.
- Replace the `update` method of `PageTable64` with `remap` and `protect`, also add `protect_region` and `copy_from`.

## 0.1.0

Initial release.
