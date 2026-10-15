use alloc::vec::Vec;
use core::{mem, ptr};

use uefi::{Status, boot, proto::loaded_image::LoadedImage};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum JumpError {
    LoadOptionsAllocateFailed,
    InvalidLoadOptions,
    LoadedImageProtocol(Status),
}

#[derive(Debug)]
pub(super) struct PreparedLoadOptions {
    words: Option<Vec<u16>>,
}

impl PreparedLoadOptions {
    pub fn new(cmdline: Option<&str>) -> Result<Self, JumpError> {
        let words = cmdline
            .filter(|cmdline| !cmdline.is_empty())
            .map(|cmdline| {
                if super::super::validate_cmdline(cmdline).is_err() {
                    return Err(JumpError::InvalidLoadOptions);
                }
                let mut words = Vec::new();
                words
                    .try_reserve_exact(cmdline.len() + 1)
                    .map_err(|_| JumpError::LoadOptionsAllocateFailed)?;
                words.extend(cmdline.bytes().map(u16::from));
                words.push(0);
                Ok(words)
            })
            .transpose()?;
        Ok(Self { words })
    }
}

pub(super) struct InstalledLoadOptions {
    words: Option<Vec<u16>>,
    previous_ptr: *const u8,
    previous_size: u32,
}

impl InstalledLoadOptions {
    pub(super) fn install(prepared: PreparedLoadOptions) -> Result<Self, JumpError> {
        let mut image = boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle())
            .map_err(|error| JumpError::LoadedImageProtocol(error.status()))?;
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

    pub(super) fn restore(mut self) -> Result<(), JumpError> {
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
                return Err(JumpError::LoadedImageProtocol(error.status()));
            }
        };
        // SAFETY: the previous pointer and size were copied from this same
        // LoadedImage protocol before replacement. Its original owner remains
        // responsible for that allocation throughout the child invocation.
        unsafe { image.set_load_options(self.previous_ptr, self.previous_size) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{JumpError, PreparedLoadOptions};

    #[test]
    fn prepares_optional_nul_terminated_ucs2_load_options() {
        assert!(PreparedLoadOptions::new(None).unwrap().words.is_none());
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0"))
                .unwrap()
                .words
                .unwrap(),
            "console=ttyS0"
                .bytes()
                .map(u16::from)
                .chain([0])
                .collect::<Vec<_>>()
        );
        assert!(PreparedLoadOptions::new(Some("")).unwrap().words.is_none());
    }

    #[test]
    fn rejects_invalid_load_options_before_installing_protocol_pointer() {
        assert_eq!(
            PreparedLoadOptions::new(Some("console=ttyS0\nreset")).unwrap_err(),
            JumpError::InvalidLoadOptions
        );
        assert_eq!(
            PreparedLoadOptions::new(Some(&"x".repeat(4096))).unwrap_err(),
            JumpError::InvalidLoadOptions
        );
    }
}
