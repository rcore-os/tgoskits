//! Runtime registration of virtual interrupt-controller capabilities.

mod registration;
mod registry;

pub use registration::ControllerRegistration;
pub(crate) use registration::{
    EndpointRegistration, MessageEndpointRegistration, PlannedBundleResources,
    WiredEndpointRegistration,
};
pub use registry::InterruptRegistrationError;
pub(crate) use registry::InterruptRegistry;
