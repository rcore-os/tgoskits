//! LoadedImage pointer installation and rollback.
use alloc::vec::Vec;
use core::{mem, ptr};

use uefi::{boot, proto::loaded_image::LoadedImage};

use crate::handoff::{HandoffError, uefi_protocol::options::PreparedLoadOptions};

pub(super) struct InstalledLoadOptions {
    words: Option<Vec<u16>>,
    previous_ptr: *const u8,
    previous_size: u32,
}

impl InstalledLoadOptions {
    pub(super) fn install(prepared: PreparedLoadOptions) -> Result<Self, HandoffError> {
        let mut image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .map_err(|error| HandoffError::Firmware(error.status().0))?;
        let (previous_ptr, previous_size) = image
            .load_options_as_bytes()
            .map(|bytes| {
                (
                    bytes.as_ptr(),
                    u32::try_from(bytes.len()).expect("UEFI load options size is u32"),
                )
            })
            .unwrap_or((ptr::null(), 0));
        let (options, size) = prepared.words.as_ref().map_or((ptr::null(), 0), |words| {
            (
                words.as_ptr().cast::<u8>(),
                u32::try_from(words.len() * size_of::<u16>())
                    .expect("validated command line fits UEFI LoadOptionsSize"),
            )
        });
        // SAFETY: `prepared.words` moves into the returned guard, so the
        // aligned UCS-2 buffer remains allocated until the EFI entry returns.
        // A missing command line deliberately installs a null, zero-size value.
        unsafe { image.set_load_options(options, size) };
        drop(image);
        Ok(Self {
            words: prepared.words,
            previous_ptr,
            previous_size,
        })
    }

    pub(super) fn restore(mut self) -> Result<(), HandoffError> {
        if !super::boot_services_active() {
            if let Some(words) = self.words.take() {
                mem::forget(words);
            }
            return Ok(());
        }
        let mut image = match boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()) {
            Ok(image) => image,
            Err(error) => {
                // The image handle still points at this buffer. Keep it alive
                // rather than leave firmware with a dangling LoadOptions pointer.
                if let Some(words) = self.words.take() {
                    mem::forget(words);
                }
                return Err(HandoffError::Firmware(error.status().0));
            }
        };
        // SAFETY: the previous pointer and size were copied from this same
        // LoadedImage protocol before replacement. Its original owner remains
        // responsible for that allocation throughout the child invocation.
        unsafe { image.set_load_options(self.previous_ptr, self.previous_size) };
        Ok(())
    }
}
