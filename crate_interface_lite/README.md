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

## Comparison with [crate_interface](https://crates.io/crates/crate_interface)

### Similar: APIs

The public APIs are almost the same as crate_interface. One major difference is
that you cannot use the exported macros as attributes.

```rust,ignore
// With crate_interface...
#[crate_interface::def_interface]
pub trait HelloIf {
    fn hello(name: &str, id: usize) -> String;
}
// With crate_interface_lite...
crate_interface_lite::def_interface!(
    pub trait HelloIf {
        fn hello(name: &str, id: usize) -> String;
    }
);
```

### Different: No proc-macro related dependencies

This is the major reason to use this crate, as it would result in a tidier
dependency tree of your project and slightly speed up the compilation. However,
if you already have proc-macro related dependencies in your crate’s dependency
graph, there is almost no benefit from using this crate.

### Different: No support for method receivers

Unlike `crate_interface::def_interface`, the macro in this crate does not support
method receivers, namely `self`, `&self`, `&mut self`, etc. But in most cases, you
don't need them, since the `impl_interface` is often applied to an unit struct.

```rust,compile_fail
crate_interface_lite::def_interface!(
    pub trait HelloIf {
        fn hello(self, name: &str, id: usize) -> String;
        //       ^^^^ Not supported!
    }
);
```

### Different: No support for default implementations

The `def_interface` in this crate does not support default implementations of
trait functions. In the future, we may support using default implementations as
fallbacks when no other implementations are provided.

```rust,compile_fail
crate_interface_lite::def_interface!(
    pub trait HelloIf {
        fn hello(self, name: &str, id: usize) -> String { todo!() }
        //                                              ^^^^^^^^^^^ Not supported!
    }
);
```
