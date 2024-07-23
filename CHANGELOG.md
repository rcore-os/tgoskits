# Changelog

## Unreleased

### New features

- Added methods for match register and interrupts.
- Added optional dependency on `chrono`.

## 0.2.0

### Breaking changes

- Changed `get_unix_timestamp` and `set_unix_timestamp` to use u32 rather than u64, to match the
  size of the device registers.
- Made `Rtc::new` unsafe, as it must be passed a valid pointer.
- Made `set_unix_timestamp` take `&mut self` rather than `&self` because it writes to device memory.

### Other changes

- Implemented `Send` and `Sync` for `Rtc`.

## 0.1.0

Initial release.
