# axklib

[![Crates.io](https://img.shields.io/crates/v/axklib.svg)](https://crates.io/crates/axklib)
[![Docs](https://img.shields.io/badge/docs-latest-blue.svg)](https://numpy1314.github.io/axklib)
[![License](https://img.shields.io/crates/l/axklib.svg)](https://github.com/numpy1314/axklib/blob/main/LICENSE)

**axklib** — Small kernel-helper abstractions used across the ArceOS microkernel.

## Overview

This crate exposes a tiny, `no_std`-compatible trait (`Klib`) that the platform/board layer must implement. The trait provides a handful of common kernel helpers such as:

- Memory mapping helpers
- Timing utilities (busy-wait)
- IRQ registration and enabling/disabling

The implementation is typically supplied by the platform layer (e.g., `modules/axklib-impl`) and consumed by drivers and other modules.

The crate also provides small convenience modules (`mem`, `time`, `irq`) that re-export the trait methods with shorter names to make call sites more ergonomic.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
axklib = "0.2.0"
```

## Example

```rust
// 1. Map 4K of device MMIO at physical address `paddr`
// Returns axerrno::AxResult<VirtAddr>
let vaddr = axklib::mem::iomap(paddr, 0x1000)?;

// 2. Busy-wait for 100 microseconds
axklib::time::busy_wait(core::time::Duration::from_micros(100));

// 3. Register an IRQ handler
// Returns bool indicating success
axklib::irq::register(32, my_irq_handler);

fn my_irq_handler() {
    // Handle interrupt...
}
```

## License

Axklib is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.