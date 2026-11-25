#![no_std]

//! ArceOS glue for the AxVisor ivshmem PCI endpoint.
//!
//! The device API is compiled only with the `arceos` feature so that other
//! users of this package do not accidentally acquire ArceOS mappings. Guest
//! protocol state machines remain outside this low-level device boundary.

mod record;

pub use record::{MessageRecordEndpoint, RecordProgress};

#[cfg(feature = "arceos")]
mod arceos;
#[cfg(feature = "arceos")]
mod ivc;

#[cfg(feature = "arceos")]
pub use arceos::{
    EVENT_VECTOR_0, IvshmemDevice, IvshmemError, IvshmemIdentity, IvshmemLayout, OutputSection,
};
#[cfg(feature = "arceos")]
pub use ivc::{
    IvcDevice, IvcDeviceError, IvcDeviceInfo, IvcEndpoint, IvcEndpointInfo, IvcPeerRole,
    IvcPendingEndpoint, IvcPendingInfo,
};
