//! SDIO I/O-card protocol over the portable SD/MMC host transaction model.
//!
//! The types in this module own card-level state only. Controller registers,
//! IRQ registration, task creation, sleeping, and retry scheduling stay in the
//! host and runtime layers.

mod function;
mod init;
mod response;
mod transfer;
mod types;

pub use function::{
    SdioBlockSizeRequest, SdioDirectRequest, SdioFunctionEnableRequest, SdioInterruptEnableRequest,
};
pub use init::SdioInitRequest;
pub use transfer::{SdioDmaSubmitError, SdioDmaTransferRequest, SdioTransferRequest};
pub use types::*;

use super::{
    host::{HostProgressWait, SdMmcIrqHost},
    transport::ProtocolHost,
};

/// Sole card-protocol owner for an IO-only SDIO card.
pub struct SdioCard<H: SdMmcIrqHost + 'static> {
    host: ProtocolHost<H>,
    info: Option<SdioCardInfo>,
    functions: [Option<SdioFunctionInfo>; 7],
    next_io_request_id: u64,
    active_io_request_id: Option<u64>,
}

impl<H: SdMmcIrqHost + 'static> SdioCard<H> {
    /// Construct an uninitialized IO-card protocol owner.
    pub fn new(host: H) -> Self {
        Self {
            host: ProtocolHost::new(host),
            info: None,
            functions: [None; 7],
            next_io_request_id: 1,
            active_io_request_id: None,
        }
    }

    /// Return shared access to the physical host capability.
    pub fn host(&self) -> &H {
        self.host.inner()
    }

    /// Return exclusive access to the physical host capability.
    pub fn host_mut(&mut self) -> &mut H {
        self.host.inner_mut()
    }

    /// Return the latest card information after initialization.
    pub const fn info(&self) -> Option<&SdioCardInfo> {
        self.info.as_ref()
    }

    /// Return information for an enumerated I/O function.
    pub fn function(&self, function: FunctionNumber) -> Option<&SdioFunctionInfo> {
        let index = function.get().checked_sub(1)? as usize;
        self.functions.get(index)?.as_ref()
    }

    /// Return the runtime condition required before protocol progress.
    pub const fn progress_wait(&self) -> HostProgressWait {
        self.host.progress_wait()
    }

    fn reserve_io_request(&mut self) -> Result<u64, crate::Error> {
        if self.active_io_request_id.is_some() {
            return Err(crate::Error::Busy);
        }
        let request_id = self.next_io_request_id;
        self.next_io_request_id = self.next_io_request_id.wrapping_add(1).max(1);
        self.active_io_request_id = Some(request_id);
        Ok(request_id)
    }

    fn ensure_io_request(&self, request_id: u64) -> Result<(), crate::Error> {
        if self.active_io_request_id == Some(request_id) {
            Ok(())
        } else {
            Err(crate::Error::InvalidArgument)
        }
    }

    fn finish_io_request(&mut self, request_id: u64) -> Result<(), crate::Error> {
        self.ensure_io_request(request_id)?;
        self.active_io_request_id = None;
        Ok(())
    }
}
