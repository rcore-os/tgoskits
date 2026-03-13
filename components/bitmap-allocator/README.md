# BitmapAllocator

[![Crates.io](https://img.shields.io/crates/v/bitmap-allocator)](https://crates.io/crates/bitmap-allocator)
[![Docs.rs](https://docs.rs/bitmap-allocator/badge.svg)](https://docs.rs/bitmap-allocator)
[![CI](https://github.com/rcore-os/bitmap-allocator/actions/workflows/main.yml/badge.svg?branch=main)](https://github.com/rcore-os/bitmap-allocator/actions/workflows/main.yml)

Bit allocator based on segment tree algorithm.

## Example

```rust
use bitmap_allocator::{BitAlloc, BitAlloc1M};

let mut ba = BitAlloc1M::default();
ba.insert(0..16);
for i in 0..16 {
    assert!(ba.test(i));
}
ba.remove(2..8);
assert_eq!(ba.alloc(), Some(0));
assert_eq!(ba.alloc(), Some(1));
assert_eq!(ba.alloc(), Some(8));
ba.dealloc(0);
ba.dealloc(1);
ba.dealloc(8);
```