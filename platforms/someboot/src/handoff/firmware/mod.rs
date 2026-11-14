//! Standard firmware LoadOptions and Linux LoadFile2 publication.
use core::ffi::c_void;

use uefi::{Handle, Status, boot};

use crate::{
    boot_resources::{Allocation, PhysicalRange},
    handoff::{
        HandoffError, HandoffSpec, transaction::Transaction,
        uefi_protocol::options::PreparedLoadOptions,
    },
};
mod metadata;
mod options;
mod payload;

pub struct PreparedHandoff {
    image: PhysicalRange,
    options: Option<options::InstalledLoadOptions>,
    _payload: payload::PublishedPayload,
    _metadata: Option<metadata::Publication>,
    _transaction: Transaction,
}
impl PreparedHandoff {
    pub fn prepare(
        spec: HandoffSpec<'_>,
        archive: Option<Allocation>,
    ) -> Result<Self, HandoffError> {
        spec.validate()?;
        // Reserved-arena archives belong to the post-EBS adapter; native EFI
        // publishes the borrowed source through its Linux LoadFile2 provider.
        if archive.is_some() || !boot_services_active() {
            return Err(HandoffError::Unavailable);
        }
        let transaction = Transaction::begin()?;
        let options = PreparedLoadOptions::new(spec.cmdline)?;
        let payload = payload::prepare_uploaded(spec.initramfs)
            .and_then(payload::PreparedPayload::publish)
            .map_err(payload_error)?;
        let metadata = spec
            .metadata
            .map(|_| metadata::Publication::install(spec))
            .transpose()?;
        let options = options::InstalledLoadOptions::install(options)?;
        Ok(Self {
            image: spec.image,
            options: Some(options),
            _payload: payload,
            _metadata: metadata,
            _transaction: transaction,
        })
    }
    /// Invokes a validated EFI entry while retaining all published buffers.
    ///
    /// # Safety
    /// The entry must be executable EFI ABI code in the given image. Its
    /// allocation stays live throughout the call; all services owned by the
    /// caller must be quiesced first. A returning child must retain Boot Services.
    pub unsafe fn enter(&self, entry: u64) -> Result<(), HandoffError> {
        if !self.image.contains(PhysicalRange::new(entry, 1)) {
            return Err(HandoffError::InvalidTarget);
        }
        let system = uefi::table::system_table_raw().ok_or(HandoffError::Unavailable)?;
        let address = usize::try_from(entry).map_err(|_| HandoffError::InvalidTarget)?;
        // SAFETY: the caller guarantees the ABI, executable entry and lifetime.
        let entry: extern "efiapi" fn(Handle, *const c_void) -> Status =
            unsafe { core::mem::transmute(address) };
        let status = entry(boot::image_handle(), system.as_ptr().cast());
        if !boot_services_active() {
            // A child violating the return contract must not reach logging or
            // the caller's resource destructors after ExitBootServices.
            // SAFETY: the entry contract still requires a valid firmware
            // execution environment and the original system/runtime tables.
            if let Some(runtime) = unsafe { system.as_ref().runtime_services.as_ref() } {
                // SAFETY: runtime services remain callable after EBS, and no
                // published buffer or firmware allocation has been released.
                unsafe {
                    (runtime.reset_system)(
                        uefi::runtime::ResetType::COLD,
                        Status::ABORTED,
                        0,
                        core::ptr::null(),
                    );
                }
            }
            // Retain every owner even if firmware offers no reset service.
            loop {
                core::hint::spin_loop();
            }
        }
        log::info!("uefi_entry_returned: {status:?}");
        if status == Status::SUCCESS {
            Ok(())
        } else {
            Err(HandoffError::Firmware(status.0))
        }
    }
}
impl Drop for PreparedHandoff {
    fn drop(&mut self) {
        if let Some(options) = self.options.take() {
            let _ = options.restore();
        }
    }
}
fn payload_error(error: payload::PayloadError) -> HandoffError {
    match error {
        payload::PayloadError::Allocation(status) | payload::PayloadError::Install(status) => {
            HandoffError::Firmware(status.0)
        }
    }
}

fn boot_services_active() -> bool {
    uefi::table::system_table_raw().is_some_and(|table| {
        // SAFETY: EFI retains this table; EBS clears its BootServices pointer.
        !unsafe { table.as_ref() }.boot_services.is_null()
    })
}
