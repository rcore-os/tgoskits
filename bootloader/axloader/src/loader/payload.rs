use core::{ffi::c_void, fmt, ptr, ptr::NonNull};

use host_boot_abi::{BOOT_PAYLOAD_GUID, BootPayload};
use uefi::boot::{self, AllocateType, MemoryType};

#[derive(Debug)]
pub enum PayloadError {
    Allocation(uefi::Status),
    Install(uefi::Status),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation(status) => write!(f, "host payload allocation failed: {status:?}"),
            Self::Install(status) => write!(f, "host payload handoff failed: {status:?}"),
        }
    }
}

pub struct PreparedPayload {
    table: Option<BootPayload>,
    archive: Option<(NonNull<u8>, usize)>,
}

impl Drop for PreparedPayload {
    fn drop(&mut self) {
        if let Some((address, pages)) = self.archive.take() {
            // SAFETY: these loader pages have not been handed to a running kernel.
            unsafe { boot::free_pages(address, pages) }.expect("failed to free host archive");
        }
    }
}

pub struct PublishedPayload {
    prepared: PreparedPayload,
    table_ptr: Option<NonNull<u8>>,
}

impl Drop for PublishedPayload {
    fn drop(&mut self) {
        let Some(table_ptr) = self.table_ptr.take() else {
            return;
        };
        // SAFETY: removing the table revokes firmware's reference before its
        // backing allocation and archive pages are released.
        if unsafe { boot::install_configuration_table(&BOOT_PAYLOAD_GUID, ptr::null()) }.is_err() {
            self.prepared.archive.take();
            crate::logln!("host_payload_error: failed to revoke UEFI configuration table");
            return;
        }
        // SAFETY: the configuration table no longer references this pool allocation.
        unsafe { boot::free_pool(table_ptr) }.expect("failed to free host boot table");
    }
}

pub fn prepare_uploaded(initramfs: Option<&[u8]>) -> Result<PreparedPayload, PayloadError> {
    if initramfs.is_none() {
        return Ok(PreparedPayload {
            table: None,
            archive: None,
        });
    }
    let mut table = BootPayload::empty();
    let archive = if let Some(bytes) = initramfs {
        let pages = bytes.len().div_ceil(4096);
        let address = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
            .map_err(|error| PayloadError::Allocation(error.status()))?;
        // SAFETY: The allocated page range is writable and has at least bytes.len() bytes.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), address.as_ptr(), bytes.len()) };
        table.archive_start = address.as_ptr() as u64;
        table.archive_len = bytes.len() as u64;
        Some((address, pages))
    } else {
        None
    };
    Ok(PreparedPayload {
        table: Some(table),
        archive,
    })
}

impl PreparedPayload {
    pub fn publish(self) -> Result<PublishedPayload, PayloadError> {
        let Some(table) = self.table.as_ref() else {
            return Ok(PublishedPayload {
                prepared: self,
                table_ptr: None,
            });
        };
        let table_ptr =
            boot::allocate_pool(MemoryType::RUNTIME_SERVICES_DATA, size_of::<BootPayload>())
                .map_err(|error| PayloadError::Allocation(error.status()))?;
        // SAFETY: table_ptr is a unique runtime-services allocation large enough
        // for BootPayload. Its contents remain immutable while installed.
        unsafe { ptr::copy_nonoverlapping(table, table_ptr.as_ptr().cast::<BootPayload>(), 1) };
        // SAFETY: UEFI retains this runtime-services allocation until removal.
        if let Err(error) = unsafe {
            boot::install_configuration_table(
                &BOOT_PAYLOAD_GUID,
                table_ptr.as_ptr().cast::<c_void>(),
            )
        } {
            // SAFETY: publication failed and firmware has no reference to the table.
            unsafe { boot::free_pool(table_ptr) }.expect("failed to free host boot table");
            return Err(PayloadError::Install(error.status()));
        }
        crate::logln!("host_payload_ready: archive_bytes={}", table.archive_len);
        Ok(PublishedPayload {
            prepared: self,
            table_ptr: Some(table_ptr),
        })
    }
}
