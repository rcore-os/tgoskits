use alloc::{boxed::Box, vec::Vec};
use core::{ffi::c_void, fmt, mem, ptr};

use uefi::{Guid, boot};
use uefi_raw::{
    Boolean,
    protocol::{
        device_path::{DevicePathProtocol, DeviceSubType, DeviceType},
        media::LoadFile2Protocol,
    },
};

const LINUX_EFI_INITRD_MEDIA_GUID: Guid = uefi::guid!("5568e427-68fc-4f3d-ac74-ca555231cc68");

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct LinuxInitrdVendorPath {
    header: DevicePathProtocol,
    vendor_guid: Guid,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct LinuxInitrdDevicePath {
    vendor: LinuxInitrdVendorPath,
    end: DevicePathProtocol,
}

impl LinuxInitrdDevicePath {
    const fn new() -> Self {
        Self {
            vendor: LinuxInitrdVendorPath {
                header: DevicePathProtocol {
                    major_type: DeviceType::MEDIA,
                    sub_type: DeviceSubType::MEDIA_VENDOR,
                    length: (mem::size_of::<LinuxInitrdVendorPath>() as u16).to_le_bytes(),
                },
                vendor_guid: LINUX_EFI_INITRD_MEDIA_GUID,
            },
            end: DevicePathProtocol {
                major_type: DeviceType::END,
                sub_type: DeviceSubType::END_ENTIRE,
                length: (mem::size_of::<DevicePathProtocol>() as u16).to_le_bytes(),
            },
        }
    }
}

#[repr(C)]
struct InitrdProvider {
    protocol: LoadFile2Protocol,
    device_path: LinuxInitrdDevicePath,
    bytes: Box<[u8]>,
    handle: Option<uefi::Handle>,
}

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
}

impl Drop for PublishedPayload {
    fn drop(&mut self) {
        let Some(provider) = self.provider.as_mut() else {
            return;
        };
        let Some(handle) = provider.handle.take() else {
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
            crate::logln!("linux_initrd_provider_error: failed to revoke LoadFile2");
            revoked = false;
        }
        // SAFETY: the device-path interface was installed on the same handle.
        if unsafe {
            boot::uninstall_protocol_interface(handle, &DevicePathProtocol::GUID, device_path)
        }
        .is_err()
        {
            crate::logln!("linux_initrd_provider_error: failed to revoke device path");
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
            return Ok(PublishedPayload { provider: None });
        };
        let mut provider = Box::new(InitrdProvider {
            protocol: LoadFile2Protocol {
                load_file: load_file2,
            },
            device_path: LinuxInitrdDevicePath::new(),
            bytes,
            handle: None,
        });
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
        provider.handle = Some(handle);
        crate::logln!(
            "linux_initrd_provider_ready: archive_bytes={}",
            provider.bytes.len()
        );
        Ok(PublishedPayload {
            provider: Some(provider),
        })
    }
}

unsafe extern "efiapi" fn load_file2(
    this: *mut LoadFile2Protocol,
    file_path: *const DevicePathProtocol,
    boot_policy: Boolean,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> uefi::Status {
    if this.is_null() || file_path.is_null() || buffer_size.is_null() {
        return uefi::Status::INVALID_PARAMETER;
    }
    if boot_policy != Boolean::FALSE {
        return uefi::Status::UNSUPPORTED;
    }
    // SAFETY: Linux passes the remaining END_ENTIRE node returned by
    // LocateDevicePath. The provider owns the protocol and starts with it.
    let provider = unsafe { &*this.cast::<InitrdProvider>() };
    let path = unsafe { &*file_path };
    if path.major_type != DeviceType::END
        || path.sub_type != DeviceSubType::END_ENTIRE
        || path.length != (mem::size_of::<DevicePathProtocol>() as u16).to_le_bytes()
    {
        return uefi::Status::INVALID_PARAMETER;
    }
    let size = unsafe { &mut *buffer_size };
    if buffer.is_null() || *size < provider.bytes.len() {
        *size = provider.bytes.len();
        return uefi::Status::BUFFER_TOO_SMALL;
    }
    // SAFETY: the caller supplied a writable buffer of the requested size.
    unsafe {
        ptr::copy_nonoverlapping(
            provider.bytes.as_ptr(),
            buffer.cast::<u8>(),
            provider.bytes.len(),
        );
    }
    *size = provider.bytes.len();
    uefi::Status::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn end_path() -> DevicePathProtocol {
        DevicePathProtocol {
            major_type: DeviceType::END,
            sub_type: DeviceSubType::END_ENTIRE,
            length: (mem::size_of::<DevicePathProtocol>() as u16).to_le_bytes(),
        }
    }

    fn provider(bytes: &[u8]) -> Box<InitrdProvider> {
        Box::new(InitrdProvider {
            protocol: LoadFile2Protocol {
                load_file: load_file2,
            },
            device_path: LinuxInitrdDevicePath::new(),
            bytes: bytes.to_vec().into_boxed_slice(),
            handle: None,
        })
    }

    #[test]
    fn linux_initrd_device_path_uses_media_vendor_guid() {
        let path = LinuxInitrdDevicePath::new();
        let guid = unsafe { ptr::read_unaligned(ptr::addr_of!(path.vendor.vendor_guid)) };
        assert_eq!(mem::size_of::<LinuxInitrdDevicePath>(), 24);
        assert_eq!(guid, LINUX_EFI_INITRD_MEDIA_GUID);
    }

    #[test]
    fn load_file2_reports_size_then_copies_exact_bytes() {
        let mut provider = provider(b"initrd");
        let path = end_path();
        let mut size = 0;
        let status = unsafe {
            load_file2(
                ptr::from_mut(&mut provider.protocol),
                ptr::from_ref(&path),
                Boolean::FALSE,
                ptr::from_mut(&mut size),
                ptr::null_mut(),
            )
        };
        assert_eq!(status, uefi::Status::BUFFER_TOO_SMALL);
        assert_eq!(size, 6);

        let mut short = [0; 3];
        size = short.len();
        let status = unsafe {
            load_file2(
                ptr::from_mut(&mut provider.protocol),
                ptr::from_ref(&path),
                Boolean::FALSE,
                ptr::from_mut(&mut size),
                short.as_mut_ptr().cast(),
            )
        };
        assert_eq!(status, uefi::Status::BUFFER_TOO_SMALL);
        assert_eq!(size, 6);

        let mut output = [0; 6];
        let status = unsafe {
            load_file2(
                ptr::from_mut(&mut provider.protocol),
                ptr::from_ref(&path),
                Boolean::FALSE,
                ptr::from_mut(&mut size),
                output.as_mut_ptr().cast(),
            )
        };
        assert_eq!(status, uefi::Status::SUCCESS);
        assert_eq!(&output, b"initrd");
    }

    #[test]
    fn load_file2_rejects_invalid_requests() {
        let mut provider = provider(b"initrd");
        let path = end_path();
        let mut size = 6;
        let status = unsafe {
            load_file2(
                ptr::null_mut(),
                ptr::from_ref(&path),
                Boolean::FALSE,
                ptr::from_mut(&mut size),
                ptr::null_mut(),
            )
        };
        assert_eq!(status, uefi::Status::INVALID_PARAMETER);

        let status = unsafe {
            load_file2(
                ptr::from_mut(&mut provider.protocol),
                ptr::from_ref(&path),
                Boolean::TRUE,
                ptr::from_mut(&mut size),
                ptr::null_mut(),
            )
        };
        assert_eq!(status, uefi::Status::UNSUPPORTED);

        let invalid_path = DevicePathProtocol {
            major_type: DeviceType::MEDIA,
            sub_type: DeviceSubType::MEDIA_VENDOR,
            length: 20_u16.to_le_bytes(),
        };
        let status = unsafe {
            load_file2(
                ptr::from_mut(&mut provider.protocol),
                ptr::from_ref(&invalid_path),
                Boolean::FALSE,
                ptr::from_mut(&mut size),
                ptr::null_mut(),
            )
        };
        assert_eq!(status, uefi::Status::INVALID_PARAMETER);
    }
}
