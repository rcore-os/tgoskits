# crate_interface_lite

[![Crates.io](https://img.shields.io/crates/v/crate_interface)](https://crates.io/crates/crate_interface_lite)
[![Docs.rs](https://docs.rs/crate_interface/badge.svg)](https://docs.rs/crate_interface_lite)
[![CI](https://github.com/arceos-org/crate_interface/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/crate_interface/actions/workflows/ci.yml)

A lightweight version of [crate_interface](https://crates.io/crates/crate_interface)
written with declarative macros.

## Example

```rust
// Define the interface
crate_interface_lite::def_interface!(
    pub trait HelloIf {
        fn hello(name: &str, id: usize) -> String;
    }
);

// Implement the interface in any crate
struct HelloIfImpl;
crate_interface_lite::impl_interface!(
    impl HelloIf for HelloIfImpl {
        fn hello(name: &str, id: usize) -> String {
            format!("Hello, {} {}!", name, id)
        }
    }
);

// Call `HelloIfImpl::hello` in any crate
use crate_interface_lite::call_interface;
assert_eq!(
    call_interface!(HelloIf::hello("world", 123)),
    "Hello, world 123!"
);
assert_eq!(
    call_interface!(HelloIf::hello, "rust", 456), // another calling style
    "Hello, rust 456!"
);
```
