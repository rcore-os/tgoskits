# axvisor\_api (Experimental Next-Generation Axvisor API)

[![CI](https://github.com/arceos-hypervisor/axvisor_api/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/arceos-hypervisor/axvisor_api/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/axvisor_api)](https://crates.io/crates/axvisor_api)
[![Docs.rs](https://docs.rs/axvisor_api/badge.svg)](https://docs.rs/axvisor_api)
[![License](https://img.shields.io/badge/License-GPL--3.0--or--later%20OR%20Apache--2.0%20OR%20MulanPSL--2.0-blue.svg)](LICENSE.Apache2)

\> [中文README](README.zh-cn.md) <

**⚠️ This repository is experimental. The list and syntax of the API may change. ⚠️**

**⚠️ These APIs may eventually be stabilized or may be removed in the future. ⚠️**

**⚠️ The maintainers will do their best to maintain compatibility, but breaking changes may still occur. ⚠️**

## Why a Next-Generation API?

Various components of Axvisor need to access functionalities provided by the ArceOS unikernel. For the crate Axvisor itself, ArceOS is a dependency and can be accessed directly. However, to maintain decoupling, other lower-level components should not use ArceOS as a direct dependency and hence cannot directly access its API. Therefore, a form of "dependency injection" is required to provide ArceOS's API to various parts of Axvisor.

Currently, Axvisor mainly uses traits + generic parameters to implement dependency injection and provide APIs to components. For example:

```rust
// Component defines the API it needs
pub trait ModAHal {
    fn foo() -> u32;
}

pub struct ModA<T: ModAHal> {
    state: u32,
}

impl<T: ModAHal> ModA<T> {
    pub fn new() -> Self {
        Self { state: T::foo() }
    }
}

// Axvisor provides the implementation
pub struct ModAHalImpl;

impl ModAHal for ModAHalImpl {
    fn foo() -> u32 {
        42
    }
}

pub fn main() {
    let mod_a = ModA::<ModAHalImpl>::new();
    println!("ModA state: {}", mod_a.state);
}
```

This method has some obvious advantages:

1. Very elegant, fully conforms to Rust's programming paradigm, no magic, easy to understand.
2. Low coupling. In theory, any low-level component can be ported to any other kernel as long as the required API is provided.

However, there are also some disadvantages:

1. The caller (struct or function) must carry all the generic parameters for the dependencies it uses, making the code verbose and less readable.
2. Different traits inevitably have overlapping methods, causing redundancy.
3. A common solution to the above two issues is to group APIs into some common traits. However, this increases nesting and coupling between traits. For example:

```rust
pub trait MemoryHal {
    // Memory-related API
}

pub trait VCpuHal {
    type Memory: MemoryHal;
    // Virtual CPU-related API
}

pub trait VMHal {
    type VCpu: VCpuHal;
    // Virtual machine-related API
}
```

4. The most serious problem: if a struct or function at the bottom of the dependency graph adds a new API dependency, all upstream type signatures must change to accommodate it, greatly increasing maintenance cost.

## Design of the Next-Generation API

`axvisor_api` aims to solve the above problems. Its design approach is:

1. Use `crate_interface` to define the API interface and wrap the provided API into regular functions.
2. Organize APIs by module, with each module corresponding to a functional area and a `crate_interface` trait.
3. Each module can include API function definitions, type definitions, constants, and helper functions based on API functions.

Example code of `axvisor_api`:

```rust
// Define an API module
#[api_mod]
mod memory {
    pub use memory_addr::{PhysAddr, VirtAddr};

    /// Allocate a frame.
    extern fn alloc_frame() -> Option<PhysAddr>;
    /// Deallocate a frame.
    extern fn dealloc_frame(addr: PhysAddr);
}

// Implement the API module
#[api_mod_impl(axvisor_api::memory)]
mod memory_impl {
    use crate_interface::memory::{alloc_frame, dealloc_frame, PhysAddr};

    extern fn alloc_frame() -> Option<PhysAddr> {
        // Call ArceOS's memory allocation function
        arceos_memory_alloc()
    }

    extern fn dealloc_frame(addr: PhysAddr) {
        // Call ArceOS's memory deallocation function
        arceos_memory_dealloc(addr);
    }
}

// Use the API module
use axvisor_api::memory::{alloc_frame, dealloc_frame, PhysAddr};
pub fn main() {
    let frame = alloc_frame().expect("Failed to allocate frame");
    println!("Allocated frame at address: {:?}", frame);
    dealloc_frame(frame);
}
```

This approach replaces all previous traits with a unified, functionally categorized API collection and eliminates explicit trait dependencies via `crate_interface`. Its advantages are:

1. API function calls are just like regular functions — simpler to use and easier to understand.
2. Callers don't need to worry about what APIs their dependencies require. Changes in dependencies don't affect callers.
3. API modules can include types, constants, and helper functions, providing better organization.

Nonetheless, there are also some downsides:

1. Although `crate_interface` uses traits under the hood for compile-time checks, these checks are weaker than traditional traits. For instance, if an `api_mod` is not implemented at all, the issue is only caught at link time.
2. This design doesn't allow providing two different API implementations for the same component via traits in the same program, slightly reducing flexibility. However, such cases are rare in Axvisor.
3. Reduces the ability to reuse a single component independently. For instance, a component that could previously be reused directly now must import `axvisor_api`. This could be mitigated using feature flags to disable unused parts of `axvisor_api` (not yet implemented), but it's still a downside.

## Current Issues

Besides the above drawbacks, the current implementation of `axvisor_api` has some issues:

1. **Does not support non-inline modules**: The common approach of placing modules in separate files (e.g., `#[api_mod] mod x;` with `x.rs`) is currently unsupported due to limitations of Rust procedural macros.
2. **Slight interference with IDE functionality**: The use of procedural macros may slightly affect IDE features like autocomplete and jump-to-definition. However, rust-analyzer seems to work fine.
3. **Conflict between `extern fn` syntax and rustfmt**: Marking API functions with `extern fn` improves readability and consistency, but rustfmt rewrites it to `extern "C" fn`, causing compile errors. A possible solution is to directly use `extern "C" fn`, though this may clash with actual external C function declarations.
4. **API functions not prominent enough**: Since the only difference between API and normal functions is the `extern` keyword and the lack of a body, API functions may not stand out in large code blocks. Currently, detailed documentation is provided as a workaround. In the future, an `#[api]` attribute might improve readability.

Additionally, there are some platform-related issues that are independent of the `axvisor_api` design or implementation. For example:

* **Platform-specific APIs**: Some APIs are strongly tied to specific platforms or even specific devices but are essential. For example, on ARM, the semi-virtualized GIC implementation relies heavily on the physical GIC driver. Including all such functionality in `axvisor_api` makes the modules bloated, but not doing so could hurt readability and maintainability.

## License

Axvisor_api is licensed under the Apache License, Version 2.0. See the [LICENSE](./LICENSE) file for details.
