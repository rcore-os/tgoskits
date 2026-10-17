//! Linux LoadFile2 device path and bounded initramfs reads.
use alloc::boxed::Box;
use core::{ffi::c_void, mem, ptr};

use uefi::Guid;
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
pub(in crate::handoff) struct LinuxInitrdDevicePath {
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
pub(in crate::handoff) struct InitrdProvider {
    pub(in crate::handoff) protocol: LoadFile2Protocol,
    pub(in crate::handoff) device_path: LinuxInitrdDevicePath,
    bytes: Box<[u8]>,
}

impl InitrdProvider {
    pub(in crate::handoff) fn new(bytes: Box<[u8]>) -> Self {
        Self {
            protocol: LoadFile2Protocol {
                load_file: load_file2,
            },
            device_path: LinuxInitrdDevicePath::new(),
            bytes,
        }
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
        Box::new(InitrdProvider::new(bytes.to_vec().into_boxed_slice()))
    }

    #[test]
    fn linux_initrd_device_path_uses_media_vendor_guid() {
        let provider = provider(b"initrd");
        let path = &provider.device_path;
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
