//! RDIF device directory and stable construction entry.

mod endpoints;
pub(crate) mod queues;
mod shared;

pub use endpoints::{AicRdifDevice, AicRdifOptions};
pub(crate) use queues::QueueOwnerPorts;
pub(crate) use shared::{
    IrqLatch, MacAddressState, OwnerChannels, OwnerReceiver, OwnerSender, WifiChannels,
    WifiProgressReceiver, WifiProgressSender, WifiProgressSignal, WifiRequestReceiver,
    WifiRequestSender, shared_irq_latch,
};
