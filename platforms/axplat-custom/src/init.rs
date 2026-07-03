use ax_plat::init::InitIf;

struct InitIfImpl;

#[impl_plat_interface]
impl InitIf for InitIfImpl {
    fn init_early(_cpu_id: usize, _arg: usize) {}

    #[cfg(feature = "smp")]
    fn init_early_secondary(_cpu_id: usize) {}

    fn init_later(_cpu_id: usize, _arg: usize) {
        let _ = rdrive::init(rdrive::Platform::Static);
    }

    #[cfg(feature = "smp")]
    fn init_later_secondary(_cpu_id: usize) {}
}

struct PlatformInfoImpl;

#[impl_plat_interface]
impl ax_plat::platform::PlatformInfoIf for PlatformInfoImpl {
    fn platform_name() -> &'static str {
        crate::config::PLATFORM_NAME
    }
}
