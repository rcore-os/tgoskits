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
    let (mmio_base, mmio_size) = configured_virtio_mmio_region()?;
    // SAFETY: The guest FDT describes this complete MMIO aperture as a
    // virtio-mmio device, and the mapping remains live for the process lifetime.
    let mmio = unsafe { <VirtIoHalImpl as Hal>::mmio_phys_to_virt(mmio_base as u64, mmio_size) };
    let header = mmio.cast::<VirtIOHeader>();
    // SAFETY: `header` points at the mapped VirtIO MMIO register aperture.
    let transport = unsafe { MmioTransport::new(header, mmio_size) }
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

#[cfg(feature = "arceos")]
fn configured_virtio_mmio_region() -> Result<(usize, usize), String> {
    let fdt = ax_hal::dtb::get_fdt().ok_or("boot FDT is unavailable")?;
    let mut regions = fdt
        .find_compatible(&["virtio,mmio"])
        .filter_map(|node| node.reg().and_then(|mut registers| registers.next()));
    let region = regions
        .next()
        .ok_or("boot FDT has no virtio-mmio register region")?;
    if regions.next().is_some() {
        return Err("boot FDT describes multiple virtio-mmio devices".into());
    }
    let base =
        usize::try_from(region.address).map_err(|_| "virtio-mmio address does not fit usize")?;
    let size = region
        .size
        .filter(|size| *size > 0)
        .ok_or("virtio-mmio register region has no size")?;
    Ok((base, size))
}
