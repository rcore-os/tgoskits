extern crate alloc;

use core::ptr::NonNull;

use ax_errno::AxError;
use ax_plat::mem::PhysAddr;
use heapless::Vec;
use rdrive::probe::OnProbeError;
use spin::Mutex;

mod pci;
#[cfg(feature = "rknpu")]
mod rknpu;
#[cfg(feature = "serial")]
mod serial;
mod soc;
#[cfg(feature = "rtc")]
mod time;
mod virtio;

pub mod blk;
#[cfg(feature = "display")]
pub mod display;
#[cfg(feature = "input")]
pub mod input;
#[cfg(feature = "net")]
pub mod net;
#[cfg(feature = "usb")]
pub mod usb;
#[cfg(feature = "vsock")]
pub mod vsock;

const MAX_BLOCK_DEVICES: usize = 16;
static BLOCK_DEVICES: Mutex<Vec<blk::Block, MAX_BLOCK_DEVICES>> = Mutex::new(Vec::new());

pub fn clear_block_devices() {
    BLOCK_DEVICES.lock().clear();
}

pub fn register_block_device(device: blk::Block) -> Result<(), blk::Block> {
    BLOCK_DEVICES.lock().push(device)
}

pub fn take_block_devices() -> Vec<blk::Block, MAX_BLOCK_DEVICES> {
    let mut devices = BLOCK_DEVICES.lock();
    core::mem::take(&mut *devices)
}

/// maps a mmio physical address to a virtual address.
pub(crate) fn iomap(addr: PhysAddr, size: usize) -> Result<NonNull<u8>, OnProbeError> {
    axklib::mmio::ioremap_raw(addr.as_usize().into(), size)
        .map_err(|e| match e {
            mmio_api::MapError::NoMemory => OnProbeError::KError(rdrive::KError::NoMem),
            _ => OnProbeError::Other(alloc::format!("{e:?}").into()),
        })
        .map(|mmio| mmio.as_nonnull_ptr())
}

pub fn probe_all_devices() -> Result<(), AxError> {
    clear_block_devices();
    rdrive::probe_all(false).map_err(|_| AxError::BadState)?;

    for dev in rdrive::get_list::<blk::PlatformBlockDevice>() {
        let block = blk::Block::try_from(dev)?;
        if register_block_device(block).is_err() {
            return Err(AxError::NoMemory);
        }
    }

    Ok(())
}
