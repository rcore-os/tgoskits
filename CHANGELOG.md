# Changelog

## 0.2.0

- No longer collect intermediate tables into a `Vec`, walk the page table and
deallocate them on drop.
- Replace the `update` method of `PageTable64` with `remap` and `protect`, also add `protect_region` and `copy_from`.

## 0.1.0

Initial release.
