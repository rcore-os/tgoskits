//! EFI registration and rollback for the Linux initramfs provider.
use alloc::{boxed::Box, vec::Vec};
use core::{ffi::c_void, fmt, ptr};

use uefi::boot;
use uefi_raw::protocol::{device_path::DevicePathProtocol, media::LoadFile2Protocol};

use crate::handoff::uefi_protocol::initrd::InitrdProvider;

#[derive(Debug)]
pub enum PayloadError {
    Allocation(uefi::Status),
    Install(uefi::Status),
}

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation(status) => {
                write!(f, "Linux initrd provider allocation failed: {status:?}")
            }
            Self::Install(status) => {
                write!(f, "Linux initrd provider installation failed: {status:?}")
            }
        }
    }
}

pub struct PreparedPayload {
    bytes: Option<Box<[u8]>>,
}

pub struct PublishedPayload {
    provider: Option<Box<InitrdProvider>>,
    handle: Option<uefi::Handle>,
}

impl Drop for PublishedPayload {
    fn drop(&mut self) {
        if !super::boot_services_active() {
            if let Some(provider) = self.provider.take() {
                Box::leak(provider);
            }
            return;
        }
        let Some(provider) = self.provider.as_mut() else {
            return;
        };
        let Some(handle) = self.handle.take() else {
            return;
        };
        let protocol = ptr::from_mut(&mut provider.protocol).cast::<c_void>();
        let device_path = ptr::from_ref(&provider.device_path).cast::<c_void>();
        let mut revoked = true;
        // SAFETY: both interfaces were installed by this provider and no child
        // invocation is active once the execution guard is dropped.
        if unsafe { boot::uninstall_protocol_interface(handle, &LoadFile2Protocol::GUID, protocol) }
            .is_err()
        {
            log::info!("linux_initrd_provider_error: failed to revoke LoadFile2");
            revoked = false;
        }
        // SAFETY: the device-path interface was installed on the same handle.
        if unsafe {
            boot::uninstall_protocol_interface(handle, &DevicePathProtocol::GUID, device_path)
        }
        .is_err()
        {
            log::info!("linux_initrd_provider_error: failed to revoke device path");
            revoked = false;
        }
        if !revoked {
            // Keep the installed interface backing memory alive if firmware
            // refused to revoke one of the protocols.
            let provider = self.provider.take().expect("provider is present");
            core::mem::forget(provider);
        }
    }
}

pub fn prepare_uploaded(initramfs: Option<&[u8]>) -> Result<PreparedPayload, PayloadError> {
    let bytes = initramfs
        .map(|source| {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(source.len())
                .map_err(|_| PayloadError::Allocation(uefi::Status::OUT_OF_RESOURCES))?;
            bytes.extend_from_slice(source);
            Ok(bytes.into_boxed_slice())
        })
        .transpose()?;
    Ok(PreparedPayload { bytes })
}

impl PreparedPayload {
    pub fn publish(self) -> Result<PublishedPayload, PayloadError> {
        let Some(bytes) = self.bytes else {
            return Ok(PublishedPayload {
                provider: None,
                handle: None,
            });
        };
        let archive_bytes = bytes.len();
        let mut provider = Box::new(InitrdProvider::new(bytes));
        let device_path = ptr::from_ref(&provider.device_path).cast::<c_void>();
        let handle = unsafe {
            boot::install_protocol_interface(None, &DevicePathProtocol::GUID, device_path)
        }
        .map_err(|error| PayloadError::Install(error.status()))?;
        let protocol = ptr::from_mut(&mut provider.protocol).cast::<c_void>();
        if let Err(error) = unsafe {
            boot::install_protocol_interface(Some(handle), &LoadFile2Protocol::GUID, protocol)
        } {
            // SAFETY: only the device-path interface was installed on failure.
            unsafe {
                boot::uninstall_protocol_interface(handle, &DevicePathProtocol::GUID, device_path)
            }
            .expect("failed to revoke Linux initrd device path");
            return Err(PayloadError::Install(error.status()));
        }
        log::info!(
            "linux_initrd_provider_ready: archive_bytes={}",
            archive_bytes
        );
        Ok(PublishedPayload {
            provider: Some(provider),
            handle: Some(handle),
        })
    }
}
