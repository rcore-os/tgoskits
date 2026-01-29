# range-alloc-arceos

[![Crates.io](https://img.shields.io/crates/v/range-alloc-arceos.svg)](https://crates.io/crates/range-alloc-arceos)
[![Docs](https://img.shields.io/badge/docs-latest-blue.svg)](https://numpy1314.github.io/range-alloc)
[![License](https://img.shields.io/crates/l/range-alloc-arceos.svg)](https://github.com/numpy1314/range-alloc/blob/main/LICENSE)
[![CI](https://github.com/numpy1314/range-alloc/actions/workflows/check.yml/badge.svg)](https://github.com/numpy1314/range-alloc/actions/workflows/check.yml)

**range-alloc-arceos** is a generic range allocator tailored for the ArceOS ecosystem. 

It is a fork of the excellent [gfx-rs/range-alloc](https://github.com/gfx-rs/range-alloc), adapted for use in kernel development and embedded scenarios (`no_std`). It allows you to dynamically allocate and free ranges from a predefined memory block or address space.

## Features

- **`no_std` Support**: Designed for bare-metal and kernel environments.
- **Generic**: Works with any type that satisfies the `Range` requirements (e.g., memory addresses, port numbers).
- **`markdown
# range-alloc-arceos

[![Crates.io](https://img.shields.io/crates/v/range-alloc-arceos.svg)](https://crates.io/crates/range-alloc-arceos)
[![Docs](https://img.shields.io/badge/docs-latest-blue.svg)](https://numpy1314.github.io/range-alloc)
[![License](https://img.shields.io/crates/l/range-alloc-arceos.svg)](https://github.com/numpy1314/range-alloc/blob/main/LICENSE)
[![CI](https://github.com/numpy1314/range-alloc/actions/workflows/check.yml/badge.svg)](https://github.com/numpy1314/range-alloc/actions/workflows/check.yml)

**range-alloc-arceos** is a generic range allocator tailored for the ArceOS ecosystem. 

It is a fork of the excellent [gfx-rs/range-alloc](https://github.com/gfx-rs/range-alloc), adapted for use in kernel development and embedded scenarios (`no_std`). It allows you to dynamically allocate and free ranges from a predefined memory block or address space.

## Features

- **`no_std` Support**: Designed for bare-metal and kernel environments.
- **Generic**: Works with any type that satisfies the `Range` requirements (e.g., memory addresses, port numbers).
- **Efficient**: Merges adjacent free ranges to minimize fragmentation.

## Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
range-alloc-arceos = "0.1.0-alpha.1"
```

## Example
```rust
use range_alloc_arceos::RangeAllocator;

fn main() {
    // Initialize the allocator with a range (e.g., 0..100)
    let mut allocator = RangeAllocator::new(0..100);

}

```

## Tests
Run the tests with:

```bash
cargo test
```

## License
This project is licensed under either of

- Apache License, Version 2.0, (LICENSE-APACHE
 or http://www.apache.org/licenses/LICENSE-2.0
)

- MIT license (LICENSE-MIT
 or http://opensource.org/licenses/MIT
)

at your option.