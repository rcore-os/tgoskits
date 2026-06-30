use ax_plat::platform::PlatformInfoIf;

struct PlatformInfoImpl;

#[impl_plat_interface]
impl PlatformInfoIf for PlatformInfoImpl {
    fn platform_name() -> &'static str {
        crate::config::PLATFORM
    }
}
