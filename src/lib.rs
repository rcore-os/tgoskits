//! `axvisor_api` is a library that provides:
//! - a set of Rust API for all components of the `axvisor` Hypervisor, including:
//!
//!   - memory management,
//!   - time and timer management,
//!   - interrupt management,
//!   - ...
//!
//!   these APIs are defined here, should be implemented by the axvisor Hypervisor, and can be use by all components.
//!
//! - a standard way to define and implement APIs, including the [`api_mod!`] macro and the [`api_mod_impl`] attribute,
//!   which the components can utilize to define and implement their own APIs.
//!
//! # How to define and implement APIs
//!
//! ## Define APIs
//!
//! To define APIs, you can use the `api_mod!` macro, which accepts one or more modules containing API functions. An API
//! function is defined with the `extern fn` syntax. Note that Vanilla Rust does not support defining extern functions
//! in such a way, so the definition of the API functions can easily be distinguished from the regular functions.
//!
//! ```rust
//! api_mod! {
//!     /// Memory-related API
//!     pub mod memory {
//!         pub use memory_addr::PhysAddr;
//!
//!         /// Allocate a frame
//!         extern fn alloc_frame() -> Option<PhysAddr>;
//!         /// Deallocate a frame
//!         extern fn dealloc_frame(addr: PhysAddr);
//!     }
//! }
//! ```
//!
//! Defined APIs can be invoked by other components:
//!
//! ```rust, no_run
//! struct SomeComponent;
//!
//! impl SomeComponent {
//!     fn some_method() {
//!         let frame = axvisor_api::memory::alloc_frame().unwrap();
//!         // Do something with the frame
//!         axvisor_api::memory::dealloc_frame(frame);
//!     }
//! }
//! ```
//!
//! ## Implement APIs
//!
//! Defined APIs should be implemented by the Hypervisor, or other components that are able and responsible to do so. To
//! implement APIs, you can use the `api_mod_impl` attribute, with the path of the module defining the APIs as the
//! argument, on a module containing the implementation of the API functions. The implementations of the API functions
//! are also defined with the `extern fn` syntax.
//!
//! ```rust, no_run
//! #[api_mod_impl(axvisor::memory)]
//! mod memory_impl {
//!     use axvisor_api::memory::PhysAddr;
//!
//!     extern fn alloc_frame() -> Option<PhysAddr> {
//!         // Implementation of the `alloc_frame` API
//!         todo!()
//!     }
//!
//!     extern fn dealloc_frame(addr: PhysAddr) {
//!         // Implementation of the `dealloc_frame` API
//!         todo!()
//!     }
//! }
//! ```
//!
//! ## Tricks behind the macros
//!
//! [`api_mod!`] and [`api_mod_impl`] are wrappers around the great [`crate_interface`] crate, with some macro tricks to
//! make the usage more convenient.
//!

#![no_std]

pub use axvisor_api_proc::{api_mod, api_mod_impl};

api_mod! {
    /// Memory-related API
    pub mod memory {
        pub use memory_addr::{PhysAddr, VirtAddr};

        /// Allocate a frame
        extern fn alloc_frame() -> Option<PhysAddr>;
        /// Deallocate a frame
        extern fn dealloc_frame(addr: PhysAddr);
        /// Convert a physical address to a virtual address
        extern fn phys_to_virt(addr: PhysAddr) -> VirtAddr;
        /// Convert a virtual address to a physical address
        extern fn virt_to_phys(addr: VirtAddr) -> PhysAddr;
    }

    /// Time-and-timer-related API
    pub mod time {
        extern crate alloc;
        use core::time::Duration;
        use alloc::boxed::Box;

        /// Time value
        pub type TimeValue = Duration;
        /// Cancel token， used to cancel a scheduled timer event
        pub type CancelToken = usize;

        /// Get the current time
        extern fn current_time() -> TimeValue;
        /// Register a timer
        extern fn register_timer(deadline: TimeValue, callback: Box<dyn FnOnce(TimeValue) + Send + 'static>) -> CancelToken;
        /// Cancel a timer
        extern fn cancel_timer(token: CancelToken);
        /// Convert cycles to time
        extern fn ticks_to_time(cycles: u64) -> TimeValue;
        /// Convert time to cycles
        extern fn time_to_ticks(time: TimeValue) -> u64;
    }
}

#[doc(hidden)]
pub mod __priv {
    pub mod crate_interface {
        pub use crate_interface::{call_interface, def_interface, impl_interface};
    }
}

#[cfg(test)]
mod test;
