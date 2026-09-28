use core::{ffi::c_void, fmt, ptr};

use host_boot_abi::{BOOT_PAYLOAD_GUID, BootPayload};
use httpboot_protocol::BootFile;
use uefi::{
    Handle,
    boot::{self, AllocateType, MemoryType},
};

use super::{
    elf_loader::EntryHandoff,
    http::{self, KernelLoadError},
};

#[derive(Debug)]
pub enum PayloadError {
    UnsupportedHandoff,
    InvalidArchive,
    InvalidCmdline(&'static str),
    Download(KernelLoadError),
    HashMismatch,
    Allocation(uefi::Status),
    Install(uefi::Status),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHandoff => write!(f, "host payload requires UEFI handoff"),
            Self::InvalidArchive => write!(f, "invalid host archive offer"),
            Self::InvalidCmdline(error) => write!(f, "{error}"),
            Self::Download(error) => write!(f, "host archive download failed: {error:?}"),
            Self::HashMismatch => write!(f, "host archive checksum mismatch"),
            Self::Allocation(status) => write!(f, "host payload allocation failed: {status:?}"),
            Self::Install(status) => write!(f, "host payload handoff failed: {status:?}"),
        }
    }
}

pub fn install(
    nic: Handle,
    initramfs: &Option<BootFile>,
    cmdline: Option<&str>,
    handoff: EntryHandoff,
) -> Result<(), PayloadError> {
    if initramfs.is_none() && cmdline.is_none() {
        return Ok(());
    }
    if handoff != EntryHandoff::Uefi {
        return Err(PayloadError::UnsupportedHandoff);
    }

    let mut table = BootPayload::empty();
    if let Some(cmdline) = cmdline {
        table
            .set_cmdline(cmdline)
            .map_err(PayloadError::InvalidCmdline)?;
    }
    let archive = if let Some(file) = initramfs {
        if file.size == 0 || !file.path.starts_with("http://") {
            return Err(PayloadError::InvalidArchive);
        }
        let bytes = http::download_sized_body(nic, &file.path, file.size)
            .map_err(PayloadError::Download)?;
        if !axloader::integrity::sha256_matches(&bytes, &file.sha256) {
            return Err(PayloadError::HashMismatch);
        }
        let pages = bytes.len().div_ceil(4096);
        let address = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, pages)
            .map_err(|error| PayloadError::Allocation(error.status()))?;
        // SAFETY: the UEFI allocation contains at least bytes.len() writable bytes.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), address.as_ptr(), bytes.len()) };
        table.archive_start = address.as_ptr() as u64;
        table.archive_len = bytes.len() as u64;
        Some((address, pages))
    } else {
        None
    };
    let table_ptr =
        match boot::allocate_pool(MemoryType::RUNTIME_SERVICES_DATA, size_of::<BootPayload>()) {
            Ok(ptr) => ptr,
            Err(error) => {
                if let Some((address, pages)) = archive {
                    // SAFETY: the allocation has not been published.
                    unsafe { boot::free_pages(address, pages) }
                        .expect("failed to free unpublished host archive");
                }
                return Err(PayloadError::Allocation(error.status()));
            }
        };
    // SAFETY: table_ptr is a uniquely owned runtime-services pool allocation
    // large enough for BootPayload; after publication it is never mutated.
    unsafe { table_ptr.as_ptr().cast::<BootPayload>().write(table) };
    // SAFETY: UEFI owns this runtime-services pool allocation after install.
    if let Err(error) = unsafe {
        boot::install_configuration_table(&BOOT_PAYLOAD_GUID, table_ptr.as_ptr().cast::<c_void>())
    } {
        // SAFETY: publication failed, so both allocations remain loader-owned.
        unsafe { boot::free_pool(table_ptr) }.expect("failed to free host boot table");
        if let Some((address, pages)) = archive {
            unsafe { boot::free_pages(address, pages) }.expect("failed to free host archive");
        }
        return Err(PayloadError::Install(error.status()));
    }
    crate::logln!(
        "host_payload_ready: archive_bytes={} cmdline_bytes={}",
        initramfs.as_ref().map_or(0, |file| file.size),
        cmdline.map_or(0, str::len)
    );
    Ok(())
}
