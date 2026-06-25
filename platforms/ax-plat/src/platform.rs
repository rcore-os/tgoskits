//! Platform identity information.

/// Platform identity interface.
#[def_plat_interface]
pub trait PlatformInfoIf {
    /// Returns the platform name.
    fn platform_name() -> &'static str;
}
