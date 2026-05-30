#![no_std]

extern crate alloc;

mod error;
mod info;
mod interface;
mod irq;
mod planner;
mod request;

pub use dma_api;
pub use error::BlkError;
pub use info::{DeviceInfo, QueueInfo, QueueLimits};
pub use interface::{IQueue, Interface};
pub use irq::{Event, IdList, IrqHandler, IrqSourceInfo, IrqSourceList};
pub use planner::{
    TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps, TransferSegment,
    TransferSegments,
};
pub use rdif_base::{DriverGeneric, KError, io};
pub use request::{
    Buffer, Request, RequestFlags, RequestId, RequestOp, RequestStatus, Segment, validate_request,
    validate_request_shape,
};
