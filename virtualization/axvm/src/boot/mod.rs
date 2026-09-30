//! Guest boot image and platform planning.

use crate::ax_err_type;

pub mod fdt;
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
    crate::arch::current::init_guest_boot_resources();
}

/// Application-owned access to guest boot files.
///
/// AxVM owns architecture boot planning, while Axvisor or another monitor owns
/// where bytes come from.
pub trait BootImageProvider {
    fn read_file(&self, file_name: &str) -> crate::AxVmResult<std::vec::Vec<u8>>;

    fn read_file_exact(
        &self,
        file_name: &str,
        read_size: usize,
    ) -> crate::AxVmResult<std::vec::Vec<u8>> {
        let buffer = self.read_file(file_name)?;
        if buffer.len() < read_size {
            return Err(ax_err_type!(
                InvalidData,
                "file is shorter than the requested read size"
            ));
        }
        Ok(buffer[..read_size].to_vec())
    }

    fn file_size(&self, file_name: &str) -> crate::AxVmResult<usize> {
        self.read_file(file_name).map(|buffer| buffer.len())
    }
}
#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64", test))]
pub(crate) mod acpi;
