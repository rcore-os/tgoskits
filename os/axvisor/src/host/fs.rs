#[cfg(all(feature = "fs", target_arch = "x86_64"))]
pub fn shutdown_filesystems() -> ax_errno::AxResult {
    ax_std::os::arceos::modules::ax_fs::shutdown_filesystems()
}
