# crate_interface

[![Crates.io](https://img.shields.io/crates/v/crate_interface)](https://crates.io/crates/crate_interface)
[![Docs.rs](https://docs.rs/crate_interface/badge.svg)](https://docs.rs/crate_interface)
[![CI](https://github.com/arceos-org/crate_interface/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/crate_interface/actions/workflows/ci.yml)

Provides a way to **define** a static interface (as a Rust trait) in a crate,
**implement** it in another crate, and **call** it from any crate, using
procedural macros. This is useful when you want to solve *circular dependencies*
between crates.

## Example

### Basic Usage

Define an interface using the `def_interface!` attribute macro, implement it
using the `impl_interface!` attribute macro, and call it using the
`call_interface!` macro. These macros can be used in separate crates.

```rust
// Define the interface
#[crate_interface::def_interface]
pub trait HelloIf {
    fn hello(&self, name: &str, id: usize) -> String;
}

// Implement the interface in any crate
struct HelloIfImpl;

#[crate_interface::impl_interface]
impl HelloIf for HelloIfImpl {
    fn hello(&self, name: &str, id: usize) -> String {
        format!("Hello, {} {}!", name, id)
    }
}

// Call `HelloIfImpl::hello` in any crate
use crate_interface::call_interface;
assert_eq!(
    call_interface!(HelloIf::hello("world", 123)),
    "Hello, world 123!"
);
assert_eq!(
    call_interface!(HelloIf::hello, "rust", 456), // another calling style
    "Hello, rust 456!"
);
```

### Generating Calling Helper Functions

It's also possible to generate calling helper functions for each interface
function, so that you can call them directly without using the `call_interface!`
macro.

This is the **RECOMMENDED** way to use this crate whenever possible, as it
provides a much more ergonomic API.

```rust
// Define the interface with caller generation
#[crate_interface::def_interface(gen_caller)]
pub trait HelloIf {
    fn hello(&self, name: &str, id: usize) -> String;
}

// a function to call the interface function is generated here like:
// fn hello(name: &str, id: usize) -> String { ... }

// Implement the interface in any crate
struct HelloIfImpl;

#[crate_interface::impl_interface]
impl HelloIf for HelloIfImpl {
    fn hello(&self, name: &str, id: usize) -> String {
        format!("Hello, {} {}!", name, id)
    }
}

// Call the generated caller function using caller function
assert_eq!(
    hello("world", 123),
    "Hello, world 123!"
);
```

### Avoiding Name Conflicts with Namespaces

You can specify a namespace for the interface to avoid name conflicts when
multiple interfaces with the same name are defined in different crates. It's
done by adding the `namespace` argument to the `def_interface!`,
`impl_interface!` and `call_interface!` macros.

```rust
mod a {
    #[crate_interface::def_interface(namespace = ShoppingMall)]
    pub trait HelloIf {
        fn hello(&self, name: &str, id: usize) -> String;
    }
}

mod b {
    #[crate_interface::def_interface(namespace = Restaurant)]
    pub trait HelloIf {
        fn hello(&self, name: &str, id: usize) -> String;
    }
}

mod c {
    use super::{a, b};

    struct HelloIfImplA;

    #[crate_interface::impl_interface(namespace = ShoppingMall)]
    impl a::HelloIf for HelloIfImplA {
        fn hello(&self, name: &str, id: usize) -> String {
            format!("Welcome to the mall, {} {}!", name, id)
        }
    }

    struct HelloIfImplB;
    #[crate_interface::impl_interface(namespace = Restaurant)]
    impl b::HelloIf for HelloIfImplB {
        fn hello(&self, name: &str, id: usize) -> String {
            format!("Welcome to the restaurant, {} {}!", name, id)
        }
    }
}

fn main() {
    // Call the interface functions using namespaces
    assert_eq!(
        crate_interface::call_interface!(namespace = ShoppingMall, a::HelloIf::hello("Alice", 1)),
        "Welcome to the mall, Alice 1!"
    );
    assert_eq!(
        crate_interface::call_interface!(namespace = Restaurant, b::HelloIf::hello("Bob", 2)),
        "Welcome to the restaurant, Bob 2!"
    );
}

```

## Things to Note

A few things to keep in mind when using this crate:

- Do not implement an interface for multiple types. No matter in the same crate
  or different crates as long as they are linked together, it will cause a
  link-time error due to duplicate symbol definitions.
- Do not define multiple interfaces with the same name, without assigning them
  different namespaces. `crate_interface` does not use crates and modules to
  isolate interfaces, only their names and namespaces are used to identify them.
- Do not alias interface traits with `use path::to::Trait as Alias;`, only use
  the original trait name, or an error will be raised.

## Implementation

The procedural macros in the above example will generate the following code:

```rust
// #[def_interface]
pub trait HelloIf {
    fn hello(&self, name: &str, id: usize) -> String;
}

#[allow(non_snake_case)]
pub mod __HelloIf_mod {
    use super::*;
    extern "Rust" {
        pub fn __HelloIf_hello(name: &str, id: usize) -> String;
    }
}

struct HelloIfImpl;

// #[impl_interface]
impl HelloIf for HelloIfImpl {
    #[inline]
    fn hello(&self, name: &str, id: usize) -> String {
        {
            #[inline]
            #[export_name = "__HelloIf_hello"]
            extern "Rust" fn __HelloIf_hello(name: &str, id: usize) -> String {
                let _impl: HelloIfImpl = HelloIfImpl;
                _impl.hello(name, id)
            }
        }
        {
            format!("Hello, {} {}!", name, id)
        }
    }
}

// call_interface!
assert_eq!(
    unsafe { __HelloIf_mod::__HelloIf_hello("world", 123) },
    "Hello, world 123!"
);
```

If you enable the `gen_caller` option in `def_interface`, calling helper
functions will also be generated. For example, `HelloIf` above will generate:

```rust
pub trait HelloIf {
    fn hello(&self, name: &str, id: usize) -> String;
}
#[doc(hidden)]
#[allow(non_snake_case)]
pub mod __HelloIf_mod {
    use super::*;
    extern "Rust" {
        pub fn __HelloIf_hello(name: &str, id: usize) -> String;
    }
}
#[inline]
pub fn hello(name: &str, id: usize) -> String {
    unsafe { __HelloIf_mod::__HelloIf_hello(name, id) }
}
```

Namespaces are implemented by further mangling the symbol names with the
namespace, for example, if `HelloIf` is defined with the `ShoppingMall`
namespace, the generated code will be:

```rust
pub trait HelloIf {
    fn hello(&self, name: &str, id: usize) -> String;
}
#[doc(hidden)]
#[allow(non_snake_case)]
pub mod __HelloIf_mod {
    use super::*;
    extern "Rust" {
        pub fn __ShoppingMall_HelloIf_hello(name: &str, id: usize) -> String;
    }
}
```
