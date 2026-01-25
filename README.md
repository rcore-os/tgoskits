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
    fn hello(name: &str, id: usize) -> String;
}

// Implement the interface in any crate
struct HelloIfImpl;

#[crate_interface::impl_interface]
impl HelloIf for HelloIfImpl {
    fn hello(name: &str, id: usize) -> String {
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
    fn hello(name: &str, id: usize) -> String;
}

// a function to call the interface function is generated here like:
// fn hello(name: &str, id: usize) -> String { ... }

// Implement the interface in any crate
struct HelloIfImpl;

#[crate_interface::impl_interface]
impl HelloIf for HelloIfImpl {
    fn hello(name: &str, id: usize) -> String {
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
        fn hello(name: &str, id: usize) -> String;
    }
}

mod b {
    #[crate_interface::def_interface(namespace = Restaurant)]
    pub trait HelloIf {
        fn hello(name: &str, id: usize) -> String;
    }
}

mod c {
    use super::{a, b};

    struct HelloIfImplA;

    #[crate_interface::impl_interface(namespace = ShoppingMall)]
    impl a::HelloIf for HelloIfImplA {
        fn hello(name: &str, id: usize) -> String {
            format!("Welcome to the mall, {} {}!", name, id)
        }
    }

    struct HelloIfImplB;
    #[crate_interface::impl_interface(namespace = Restaurant)]
    impl b::HelloIf for HelloIfImplB {
        fn hello(name: &str, id: usize) -> String {
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

### Default Implementations with Weak Symbols

The `weak_default` feature allows you to define **default implementations** for
interface methods. These defaults are compiled as **weak symbols**, which means:

- Implementors can choose to override only the methods they need
- Methods without explicit implementations will automatically use the defaults
- The linker resolves which implementation to use at link time

This is useful when you want to provide sensible defaults while still allowing
customization. To use this feature, you need to use nightly Rust and enable
`#![feature(linkage)]` in the crate that defines the interface trait.

Due to Rust compiler limitations, it's impossible to implement an interface  
with default implementations in the same crate where it's defined. This should
not be a problem for most cases, because the only sensible scenario where an
interface would be implemented in the same crate where it's defined is for  
testing, and such tests can always be done in a separate crate.  

For example, given the following interface definition with default
implementations:

```rust,ignore
#![feature(linkage)]
use crate_interface::def_interface;

#[def_interface]
pub trait InitIf {
    fn init() {
        // default implementation that calls another method
        Self::setup();
        println!("Default init");
    }
    fn setup() {
        println!("Default setup");
    }
}
```

The macro will expand to:

```rust,ignore
pub trait InitIf {
    fn init() {
        #[allow(non_snake_case)]
        #[linkage = "weak"]
        #[no_mangle]
        extern "Rust" fn __InitIf_init() {
            // A proxy function is generated for Self::setup() calls
            #[allow(non_snake_case)]
            fn __self_proxy_setup() {
                unsafe { __InitIf_mod::__InitIf_setup() }
            }

            // Self::setup() is rewritten to use the proxy function
            __self_proxy_setup();
            println!("Default init");
        }
        __InitIf_init()
    }
    fn setup() {
        #[allow(non_snake_case)]
        #[linkage = "weak"]
        #[no_mangle]
        extern "Rust" fn __InitIf_setup() {
            println!("Default setup");
        }
        __InitIf_setup()
    }
}

#[doc(hidden)]
#[allow(non_snake_case)]
pub mod __InitIf_mod {
    use super::*;
    extern "Rust" {
        pub fn __InitIf_init();
        pub fn __InitIf_setup();
    }
}
```

The default implementation is compiled as a weak symbol (`#[linkage = "weak"]`).
When an implementor provides their own implementation using `impl_interface`, it
generates a strong symbol with the same name, which the linker will prefer over
the weak symbol.

When a default implementation calls another trait method via `Self::method()`,
a proxy function (`__self_proxy_method`) is generated. This proxy calls the
extern function, ensuring that if an implementor overrides that method, the
overridden (strong symbol) version is called at runtime instead of the default.

## Things to Note

A few things to keep in mind when using this crate:

- **Methods with receivers are not supported.** Interface functions must not
  have `self`, `&self`, or `&mut self` parameters. Use associated functions
  (static methods) instead:

  ```rust,compile_fail
  # use crate_interface::*;
  #[def_interface]
  trait MyIf {
      fn foo(&self); // error: methods with receiver (self) are not allowed
  }
  ```

- **Generic parameters are not supported.** Interface functions cannot have
  generic type parameters, lifetime parameters, or const generic parameters:

  ```rust,compile_fail
  # use crate_interface::*;
  #[def_interface]
  trait MyIf {
      fn foo<T>(x: T); // error: generic parameters are not allowed
  }
  ```

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
    fn hello(name: &str, id: usize) -> String;
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
    fn hello(name: &str, id: usize) -> String {
        {
            #[inline]
            #[export_name = "__HelloIf_hello"]
            extern "Rust" fn __HelloIf_hello(name: &str, id: usize) -> String {
                HelloIfImpl::hello(name, id)
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
    fn hello(name: &str, id: usize) -> String;
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
    fn hello(name: &str, id: usize) -> String;
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
