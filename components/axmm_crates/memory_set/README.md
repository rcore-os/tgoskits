# memory_set

[![Crates.io](https://img.shields.io/crates/v/memory_set)](https://crates.io/crates/memory_set)
[![Docs.rs](https://docs.rs/memory_set/badge.svg)](https://docs.rs/memory_set)
[![CI](https://github.com/arceos-org/axmm_crates/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/axmm_crates/actions/workflows/ci.yml)

Data structures and operations for managing memory mappings.

It is useful to implement [`mmap`][1], [`munmap`][1] and [`mprotect`][2].

[1]: https://man7.org/linux/man-pages/man2/mmap.2.html
[2]: https://man7.org/linux/man-pages/man2/mprotect.2.html

## Examples

```rust
use memory_addr::{va, va_range, VirtAddr};
use memory_set::{MappingBackend, MemoryArea, MemorySet};

const MAX_ADDR: usize = 0x10000;

/// A mock memory flags.
type MockFlags = u8;
/// A mock page table, which is a simple array that maps addresses to flags.
type MockPageTable = [MockFlags; MAX_ADDR];

/// A mock mapping backend that manipulates the page table on `map` and `unmap`.
#[derive(Clone)]
struct MockBackend;

let mut pt = [0; MAX_ADDR];
let mut memory_set = MemorySet::<MockBackend>::new();

// Map [0x1000..0x5000).
memory_set.map(
    /* area: */ MemoryArea::new(va!(0x1000), 0x4000, 1, MockBackend),
    /* page_table: */ &mut pt,
    /* unmap_overlap */ false,
).unwrap();
// Unmap [0x2000..0x4000), will split the area into two parts.
memory_set.unmap(va!(0x2000), 0x2000, &mut pt).unwrap();

let areas = memory_set.iter().collect::<Vec<_>>();
assert_eq!(areas.len(), 2);
assert_eq!(areas[0].va_range(), va_range!(0x1000..0x2000));
assert_eq!(areas[1].va_range(), va_range!(0x4000..0x5000));

// Underlying operations to do when manipulating mappings.
impl MappingBackend for MockBackend {
    type Addr = VirtAddr;
    type Flags = MockFlags;
    type PageTable = MockPageTable;

    fn map(&self, start: VirtAddr, size: usize, flags: MockFlags, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return false;
            }
            *entry = flags;
        }
        true
    }

    fn unmap(&self, start: VirtAddr, size: usize, pt: &mut MockPageTable) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = 0;
        }
        true
    }

    fn protect(
        &self,
        start: VirtAddr,
        size: usize,
        new_flags: MockFlags,
        pt: &mut MockPageTable,
    ) -> bool {
        for entry in pt.iter_mut().skip(start.as_usize()).take(size) {
            if *entry == 0 {
                return false;
            }
            *entry = new_flags;
        }
        true
    }
}
```
