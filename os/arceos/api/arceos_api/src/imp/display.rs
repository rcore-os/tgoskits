pub use ax_display::DisplayInfo as AxDisplayInfo;
pub use ax_display::DisplayError as AxDisplayError;

/// Gets the framebuffer information.
pub fn ax_framebuffer_info() -> Result<AxDisplayInfo, AxDisplayError> {
    ax_display::framebuffer_info()
}

/// Flushes the framebuffer, i.e. show on the screen.
pub fn ax_framebuffer_flush() -> Result<(), AxDisplayError> {
    ax_display::framebuffer_flush()
}

/// Accesses framebuffer bytes only for the lifetime of this callback.
///
/// # Safety
///
/// Exclude concurrent CPU and userspace mmap access and wait for GPU writes.
pub unsafe fn ax_with_framebuffer(
    access: &mut dyn FnMut(&mut [u8], AxDisplayInfo),
) -> Result<(), AxDisplayError> {
    unsafe { ax_display::with_framebuffer(access) }
}
