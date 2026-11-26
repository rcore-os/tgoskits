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
    IVC_MAX_RECORD_BYTES, IVC_MAX_TIMEOUT_MS, IvcDevice, IvcDeviceError, IvcDeviceInfo,
    IvcEndpoint, IvcEndpointInfo, IvcError, IvcPeerRole, IvcPendingEndpoint, IvcPendingInfo,
    IvcSession, ivc_close, ivc_open, ivc_recv, ivc_send,
};
