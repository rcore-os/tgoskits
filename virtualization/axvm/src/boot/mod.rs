//! Guest boot image and platform planning.

#[cfg(any(feature = "fs", feature = "host-fs"))]
use ax_errno::ax_err_type;

pub mod images;
mod policy;

#[cfg(any(
    test,
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "riscv64"
))]
pub mod fdt;
#[cfg(target_arch = "loongarch64")]
pub mod guest_platform;

#[cfg(target_arch = "loongarch64")]
pub use fdt::handle_fdt_operations;
#[cfg(target_arch = "loongarch64")]
pub use fdt::init_guest_boot_resources;
#[cfg(any(target_arch = "aarch64", target_arch = "riscv64"))]
pub use fdt::{GuestDtbImage, handle_fdt_operations};
pub use images::{ImageLoader, get_image_header};
#[cfg(target_arch = "x86_64")]
pub use images::{is_x86_linux_image_config, x86_qemu_passthrough_block_intx};
pub use policy::{GuestAcpiTables, GuestBootDescription, GuestDeviceTree, GuestFdtBuilder};

/// Build-time image bytes supplied by the hypervisor application.
#[derive(Clone, Copy, Debug)]
pub struct StaticVmImage {
    pub id: usize,
    pub kernel: &'static [u8],
    pub bios: Option<&'static [u8]>,
    pub ramdisk: Option<&'static [u8]>,
    pub dtb: Option<&'static [u8]>,
}

/// Application-owned source for guest image bytes and host files.
///
/// AxVM owns architecture boot planning, while Axvisor or another monitor owns
/// where bytes come from.
pub trait BootImageProvider {
    fn static_vm_images(&self) -> &'static [StaticVmImage];

    #[cfg(target_arch = "loongarch64")]
    fn static_firmware_images(&self) -> &'static [StaticVmImage] {
        &[]
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn read_file(&self, file_name: &str) -> ax_errno::AxResult<alloc::vec::Vec<u8>>;

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn read_file_exact(
        &self,
        file_name: &str,
        read_size: usize,
    ) -> ax_errno::AxResult<alloc::vec::Vec<u8>> {
        let buffer = self.read_file(file_name)?;
        if buffer.len() < read_size {
            return Err(ax_err_type!(
                InvalidData,
                "file is shorter than the requested read size"
            ));
        }
        Ok(buffer[..read_size].to_vec())
    }

    #[cfg(any(feature = "fs", feature = "host-fs"))]
    fn file_size(&self, file_name: &str) -> ax_errno::AxResult<usize> {
        self.read_file(file_name).map(|buffer| buffer.len())
    }
}
