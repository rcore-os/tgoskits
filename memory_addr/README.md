# memory_addr

[![Crates.io](https://img.shields.io/crates/v/memory_addr)](https://crates.io/crates/memory_addr)
[![Docs.rs](https://docs.rs/memory_addr/badge.svg)](https://docs.rs/memory_addr)
[![CI](https://github.com/arceos-org/axmm_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axmm_crates/actions/workflows/ci.yml)

Wrappers and helper functions for physical and virtual memory addresses.

## Examples

```rust
use memory_addr::{pa, va, va_range, PhysAddr, VirtAddr, MemoryAddr};

let phys_addr = PhysAddr::from(0x12345678);
let virt_addr = VirtAddr::from(0x87654321);

assert_eq!(phys_addr.align_down(0x1000usize), pa!(0x12345000));
assert_eq!(phys_addr.align_offset_4k(), 0x678);
assert_eq!(virt_addr.align_up_4k(), va!(0x87655000));
assert!(!virt_addr.is_aligned_4k());
assert!(va!(0xabcedf0).is_aligned(16usize));

let va_range = va_range!(0x87654000..0x87655000);
assert_eq!(va_range.start, va!(0x87654000));
assert_eq!(va_range.size(), 0x1000);
assert!(va_range.contains(virt_addr));
assert!(va_range.contains_range(va_range!(virt_addr..virt_addr + 0x100)));
assert!(!va_range.contains_range(va_range!(virt_addr..virt_addr + 0x1000)));
```
