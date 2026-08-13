//! ArceOS guest test for AxVisor's in-memory VirtIO block backend.

#[cfg(feature = "arceos")]
use ax_driver::virtio::VirtIoHalImpl;
#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use virtio_drivers::{
    Hal,
    device::blk::VirtIOBlk,
    transport::mmio::{MmioTransport, VirtIOHeader},
};

const MMIO_BASE: usize = 0x0a00_0200;
const MMIO_SIZE: usize = 0x200;
const TEST_SECTOR: usize = 7;

#[cfg(feature = "arceos")]
fn main() {
    match run() {
        Ok(()) => println!("ARCEOS_VIRTIO_BLK_PASS"),
        Err(error) => println!("ARCEOS_VIRTIO_BLK_FAIL {error}"),
    }
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
fn run() -> Result<(), String> {
    // SAFETY: AxVisor reserves this complete MMIO aperture for the configured
    // virtio-blk device, and the mapping remains live for the process lifetime.
    let mmio = unsafe { <VirtIoHalImpl as Hal>::mmio_phys_to_virt(MMIO_BASE as u64, MMIO_SIZE) };
    let header = mmio.cast::<VirtIOHeader>();
    // SAFETY: `header` points at the mapped VirtIO MMIO register aperture.
    let transport = unsafe { MmioTransport::new(header, MMIO_SIZE) }
        .map_err(|error| format!("create MMIO transport: {error:?}"))?;
    let mut block = VirtIOBlk::<VirtIoHalImpl, _>::new(transport)
        .map_err(|error| format!("initialize block device: {error:?}"))?;

    let mut expected = [0u8; 512];
    for (index, byte) in expected.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
    }
    block
        .write_blocks(TEST_SECTOR, &expected)
        .map_err(|error| format!("write sector: {error:?}"))?;
    block
        .flush()
        .map_err(|error| format!("flush device: {error:?}"))?;

    let mut actual = [0u8; 512];
    block
        .read_blocks(TEST_SECTOR, &mut actual)
        .map_err(|error| format!("read sector: {error:?}"))?;
    if actual != expected {
        return Err("readback differs from written sector".into());
    }
    Ok(())
}
