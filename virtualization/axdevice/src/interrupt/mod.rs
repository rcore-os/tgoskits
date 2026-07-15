//! VM-local interrupt-controller topology.
//!
//! Devices connect to controller inputs, controllers optionally connect to a
//! parent controller, and CPU-facing controllers attach to explicit vCPU
//! ports. The topology never exposes raw vector injection.

mod controller;
mod registry;
mod request;
mod topology;
mod vcpu;

pub use controller::*;
pub use request::*;
pub use topology::*;
pub use vcpu::*;
