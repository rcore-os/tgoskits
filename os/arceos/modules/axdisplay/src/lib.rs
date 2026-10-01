//! Screen output access through the GPU runtime's single device owner.

#![no_std]

extern crate alloc;

use alloc::sync::Arc;

pub use ax_gpu::{rdif_display::DisplayError, rdif_gpu::PixelFormat};
use ax_gpu::{
    rdif_display::{DisplayState, Framebuffer, ScanoutBuffer},
    rdif_gpu::Backing,
};

pub type DisplayResult<T = ()> = Result<T, DisplayError>;

/// Geometry of the boot framebuffer. No borrow or address escapes with this
/// metadata; use [`with_framebuffer`] for CPU access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayInfo {
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub format: PixelFormat,
    pub fb_size: usize,
}

impl DisplayInfo {
    pub const fn line_length(&self) -> usize {
        self.stride
    }
}

fn default_framebuffer() -> DisplayResult<(DisplayState, Framebuffer, Arc<ax_gpu::MappableBacking>)>
{
    let (state, mapping) = ax_gpu::default_framebuffer_mapping()?;
    let framebuffer = state
        .framebuffer
        .clone()
        .ok_or(DisplayError::NotAvailable)?;
    Ok((state, framebuffer, mapping))
}

/// Whether the registered device has a CPU-mappable boot framebuffer.
pub fn has_display() -> bool {
    framebuffer_info().is_ok()
}

/// Returns the boot framebuffer geometry and size.
pub fn framebuffer_info() -> DisplayResult<DisplayInfo> {
    let (_, framebuffer, mapping) = default_framebuffer()?;
    let fb_size = (framebuffer.stride as usize)
        .checked_mul(framebuffer.height as usize)
        .ok_or(DisplayError::InvalidState)?;
    if fb_size > mapping.backing().len() {
        return Err(DisplayError::InvalidState);
    }
    Ok(DisplayInfo {
        width: framebuffer.width,
        height: framebuffer.height,
        stride: framebuffer.stride as usize,
        format: framebuffer.format,
        fb_size,
    })
}

/// Accesses framebuffer bytes while holding the GPU control lock.
///
/// # Safety
///
/// The caller must exclude every other CPU alias, including userspace mmap,
/// for the callback's duration, and wait for any GPU write completion first.
/// The callback must not retain the slice or re-enter GPU/display APIs.
pub unsafe fn with_framebuffer(access: &mut dyn FnMut(&mut [u8], DisplayInfo)) -> DisplayResult {
    let (_, _, mapping) = default_framebuffer()?;
    let info = framebuffer_info()?;
    ax_gpu::with_display(|_| {
        // SAFETY: the caller promises no CPU or user mapping aliases, while
        // the GPU owner lock prevents new device writes. Completion of prior
        // GPU writes remains the caller's explicit prerequisite.
        unsafe {
            mapping
                .backing()
                .with_cpu_bytes(&mut |bytes| access(&mut bytes[..info.fb_size], info))?
        };
        Ok(())
    })?
}

/// Retains boot framebuffer memory for mappings and long-lived framebuffer
/// users. CPU writes must still be serialized through [`with_framebuffer`].
pub fn framebuffer_backing() -> DisplayResult<Arc<dyn Backing>> {
    Ok(default_framebuffer()?.2.backing())
}

/// Retains the boot framebuffer together with its CPU physical mapping.
pub fn framebuffer_mapping() -> DisplayResult<Arc<ax_gpu::MappableBacking>> {
    Ok(default_framebuffer()?.2)
}

fn same_scanout(active: &ScanoutBuffer, default: &ScanoutBuffer) -> bool {
    match (active, default) {
        (ScanoutBuffer::Gpu(active), ScanoutBuffer::Gpu(default)) => active == default,
        (ScanoutBuffer::Backing(active), ScanoutBuffer::Backing(default)) => {
            Arc::ptr_eq(active, default)
        }
        _ => false,
    }
}

/// Submits full-frame damage for the current output.
pub fn framebuffer_flush() -> DisplayResult {
    let (mut state, framebuffer, _) = default_framebuffer()?;
    ax_gpu::with_display(|device| {
        let Some(active) = device.current_state(state.output)? else {
            return Ok(());
        };
        let still_active = active
            .framebuffer
            .as_ref()
            .zip(state.framebuffer.as_ref())
            .is_some_and(|(active, default)| same_scanout(&active.buffer, &default.buffer));
        if !still_active {
            return Ok(());
        }
        state.damage = alloc::vec![ax_gpu::rdif_display::Rect {
            x: 0,
            y: 0,
            width: framebuffer.width,
            height: framebuffer.height,
        }];
        // The present completion needs no observation: this only re-commits
        // the boot framebuffer, whose mapping is retained by the GPU runtime
        // for its lifetime, so no backing is released that the host could
        // still DMA.
        let _ = device.commit(&state)?;
        Ok(())
    })?
}

/// Restores the boot framebuffer after a separate modeset.
pub fn framebuffer_restore_scanout() -> DisplayResult {
    ax_gpu::restore_default_scanout()
}

#[cfg(all(test, feature = "host-test"))]
mod tests {
    use alloc::sync::Arc;
    use core::ops::Range;

    use ax_gpu::rdif_gpu::{Backing, DmaDomainId, DmaSegment, GpuError};

    use super::{ScanoutBuffer, same_scanout};

    struct TestBacking;

    // SAFETY: This zero-length backing has no DMA segments or CPU storage.
    // The test only compares Arc identities and never submits it to a device.
    unsafe impl Backing for TestBacking {
        fn len(&self) -> usize {
            0
        }

        fn domain_id(&self) -> DmaDomainId {
            DmaDomainId::Direct
        }

        fn segments(&self) -> &[DmaSegment] {
            &[]
        }

        fn sync_for_device(&self, _range: Range<usize>) -> Result<(), GpuError> {
            Ok(())
        }

        fn sync_for_cpu(&self, _range: Range<usize>) -> Result<(), GpuError> {
            Ok(())
        }
    }

    #[test]
    fn direct_backing_remains_the_active_boot_scanout() {
        let backing: Arc<dyn Backing> = Arc::new(TestBacking);
        let active = ScanoutBuffer::Backing(backing.clone());
        let default = ScanoutBuffer::Backing(backing);
        assert!(same_scanout(&active, &default));
        assert!(!same_scanout(
            &active,
            &ScanoutBuffer::Backing(Arc::new(TestBacking))
        ));
    }
}
