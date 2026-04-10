pub use ax_display::DisplayInfo as AxDisplayInfo;

/// Gets the framebuffer information.
pub fn ax_framebuffer_info() -> AxDisplayInfo {
    ax_display::framebuffer_info()
}

/// Flushes the framebuffer, i.e. show on the screen.
pub fn ax_framebuffer_flush() -> bool {
    ax_display::framebuffer_flush()
}
