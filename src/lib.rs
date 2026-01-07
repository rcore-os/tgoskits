//! `axvisor_api` is the bottom-most crate of the AxVisor Hypervisor project. It
//! provides a standardized set of APIs for the components of the Hypervisor and
//! grants them access to OS-level and Hypervisor-level functionalities, like
//! memory allocation, address conversion, time and timer management, cross-vcpu
//! operations, and so on.
//!
//! `axvisor_api` is designed for two main purposes:
//! - **Replace generic-based API-injection mechanism.**
//!   
//!   Generic-based API-injection mechanism, for example:
//!
//!   ```rust
//!   pub trait VCpuHal {
//!       /* ... */
//!   }
//!   pub struct VCpu<H: VCpuHal> {
//!       /* ... */
//!   #   _marker: core::marker::PhantomData<H>,
//!   }
//!   ```
//!   
//!   has been widely used previously here and in other related projects. It's
//!   definitely great, with zero overhead, good readability, and no link-time
//!   magic. However, it turns out that passing generic parameters down through
//!   multiple layers of modules and components is quite inconvenient, and it's
//!   always a pain to decide how to categorize the APIs into different traits
//!   properly.
//!
//!   Finally, the author decided to just use a big monolithic trait for every
//!   component, and use `crate_interface` to eliminate the generic parameter.
//!   (Although there are multiple traits in this crate, technically in the
//!   current implementation the link-time symbol space is used as a big
//!   monolithic trait.) Theoretically, this may slightly increase the
//!   difficulty of re-using a single component in a new software project, but
//!   the author believes that it will reduce the overall complexity when not
//!   one but a few related components are re-used together.
//!
//! - **Enable portability of the Hypervisor across different unikernels.**
//!
//!   Technically, the whole AxVisor Hypervisor can be ported to different
//!   unikernels by implementing the APIs defined in this crate (although not
//!   tested yet). If such porting is successful, this crate can also be used as
//!   a tested hardware-and-unikernel abstraction layer for other hypervisor
//!   projects.
//!
//! This crate also provides a standard way to define and implement APIs, with
//! the [`api_def`] and [`api_impl`] procedural macros. They are built on top of
//! the `crate_interface` crate, which provides the low-level functionalities
//! of defining and implementing crate-level interfaces.
//!
//! # How to define and implement APIs
//!
//! ## Define APIs
//!
//! To define APIs, you can use the `api_def` attribute on a trait defining the
//! API, with each API function defined as a regular function in the trait. It's
//! recommended to pack the trait definition and related definitions (like type
//! aliases) into a module for better organization.
//!
//! ```rust, standalone_crate
//! # // some inconvenience brought by proc-macro-name and doctest
//! # use axvisor_api::__priv;
//! # fn main() {}
//! mod example {
//!     # // some inconvenience brought by proc-macro-name and doctest
//!     # use axvisor_api::api_def;
//!     /// Example API definition
//!     #[api_def]
//!     pub trait ExampleIf {
//!         /// An example API function
//!         fn example_func(arg: usize) -> usize;
//!         /// Another example API function
//!         fn another_func();
//!     }
//! }
//!
//! fn use_example_api() {
//!     let result = example::example_func(42);
//!     example::another_func();
//! }
//! ```
//!
//! `api_def` will generate a caller function for each API function defined in
//! the trait, at the same level as the trait definition. The generated callers
//! can be used to invoke the API functions, as demonstrated above.
//!
//! ## Implement APIs
//!
//! Defined APIs should be implemented somewhere, unless they are not used
//! anywhere. To implement APIs, the implementer should define an empty struct
//! and implement the API trait for the struct, with the `api_impl` attribute on
//! the `impl` block. For example,
//!
//! ```rust, standalone_crate
//! # // some inconvenience brought by proc-macro-name and doctest
//! # use axvisor_api::{api_impl, __priv};
//! mod example {
//!     # // some inconvenience brought by proc-macro-name and doctest
//!     # use axvisor_api::{api_def, __priv};
//!     /// Example API definition
//!     #[api_def]
//!     pub trait ExampleIf {
//!         /// An example API function
//!         fn example_func(arg: usize) -> usize;
//!         /// Another example API function
//!         fn another_func();
//!     }
//! }
//!
//! struct ExampleImpl;
//!
//! #[api_impl]
//! impl example::ExampleIf for ExampleImpl {
//!     fn example_func(arg: usize) -> usize {
//!         arg + 1
//!     }
//!
//!     fn another_func() {
//!         println!("Another function called");
//!     }
//! }
//!
//! fn main() {
//!     let result = example::example_func(42);
//!     assert_eq!(result, 43);
//!     example::another_func(); // prints "Another function called"
//! }
//! ```
//!

#![no_std]

pub use axvisor_api_proc::{api_def, api_impl};

pub mod arch;
pub mod host;
pub mod memory;
pub mod time;
pub mod vmm;

#[doc(hidden)]
pub mod __priv {
    pub mod crate_interface {
        pub use crate_interface::{call_interface, def_interface, impl_interface};
    }
}

#[cfg(test)]
mod test;
