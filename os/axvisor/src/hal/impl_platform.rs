#[cfg(feature = "fs")]
use std::os::arceos;

use ax_errno::AxResult;
use axvisor_api::{memory::PhysAddr, platform::PlatformIf};

struct PlatformImpl;

#[axvisor_api::api_impl]
impl PlatformIf for PlatformImpl {
    fn get_host_fdt_ptr() -> Option<PhysAddr> {
        #[cfg(any(
            target_arch = "aarch64",
            target_arch = "loongarch64",
            target_arch = "riscv64"
        ))]
        {
            let bootarg = ax_hal::dtb::get_bootarg();
            return (bootarg != 0).then(|| bootarg.into());
        }

        #[cfg(not(any(
            target_arch = "aarch64",
            target_arch = "loongarch64",
            target_arch = "riscv64"
        )))]
        {
            None
        }
    }

    fn shutdown_host_filesystems() -> AxResult {
        #[cfg(feature = "fs")]
        {
            return arceos::modules::ax_fs::shutdown_filesystems();
        }

        #[cfg(not(feature = "fs"))]
        {
            Ok(())
        }
    }
}
