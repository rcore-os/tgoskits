//! Guest boot image and platform planning.

#[cfg(any(feature = "fs", feature = "host-fs"))]
use ax_errno::ax_err_type;

pub mod fdt;
pub mod guest_platform;
pub mod images;
mod policy;
mod prepared;

pub use images::*;
pub use policy::{
    GuestAcpiTables, GuestBootDescription, GuestDeviceTree, GuestFdtBuilder,
    boot_firmware_load_gpa, guest_boot_policy,
};
pub use prepared::{PreparedGuestBoot, prepare_guest_boot};

/// Initializes architecture-owned guest firmware resources.
pub fn init_guest_boot_resources() {
    crate::arch::init_guest_boot_resources();
}

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
