# ax-alloc

ArceOS global memory allocator. Provides [`GlobalAllocator`] implementing [`core::alloc::GlobalAlloc`] for use with `#[global_allocator]`.

The current backends are selected by feature: `tlsf` uses the `rlsf` crate, and
`buddy-slab` uses `buddy-slab-allocator` with per-CPU slab support.

## Features

- `tlsf` – TLSF byte and page allocation backed by `rlsf`
- `buddy-slab` – buddy page allocation plus per-CPU slab allocation
- `global-allocator` – register the selected backend as `#[global_allocator]`
- `tracking` – allocation tracking (requires `ax-percpu`, `axbacktrace`)

## License

GPL-3.0-or-later OR Apache-2.0 OR MulanPSL-2.0
