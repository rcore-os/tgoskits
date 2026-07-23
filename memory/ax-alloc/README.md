# ax-alloc

TGOSKits runtime allocator. Provides [`GlobalAllocator`] implementing [`core::alloc::GlobalAlloc`] for use with `#[global_allocator]`.

Uses `buddy-slab-allocator` as the fixed runtime backend for the kernel heap and page allocator.

## Features

- `global-allocator` – installs the crate allocator through `#[global_allocator]`
- `smp` – enables allocator and per-CPU slab synchronization for multi-core builds
- `tracking` – allocation backtrace tracking

`embedded-default`, `starry`, and `hypervisor` are system configuration
combinations, not allocator features. Their build configurations select only the
concrete allocator capabilities they use.

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0
