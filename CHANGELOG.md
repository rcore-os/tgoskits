# Changelog

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
