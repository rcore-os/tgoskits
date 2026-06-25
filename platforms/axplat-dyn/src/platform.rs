use ax_plat::platform::PlatformInfoIf;

struct PlatformInfoImpl;

#[impl_plat_interface]
impl PlatformInfoIf for PlatformInfoImpl {
    fn platform_name() -> &'static str {
        somehal::platform_name().unwrap_or(default_platform_name())
    }
}

const fn default_platform_name() -> &'static str {
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64-plat-dyn"
    }
    #[cfg(target_arch = "loongarch64")]
    {
        "loongarch64-plat-dyn"
    }
    #[cfg(target_arch = "riscv64")]
    {
        "riscv64-plat-dyn"
    }
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64-plat-dyn"
    }
    #[cfg(not(any(
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "riscv64",
        target_arch = "x86_64"
    )))]
    {
        "dyn"
    }
}
