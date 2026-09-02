//! ArceOS guest smoke test for AxVisor's initial ivshmem PCI endpoint.

#[cfg(feature = "arceos")]
use core::ptr::NonNull;

#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use ax_std::os::arceos::modules::ax_hal::mem::PhysAddr;

const ECAM_BASE: usize = 0x0b00_0000;
const ECAM_SIZE: usize = 0x10_0000;
const PCI_ID_OFFSET: usize = 0x00;
const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_BAR2_OFFSET: usize = 0x18;
const PCI_COMMAND_MEMORY_ENABLE: u16 = 1 << 1;
const IVSHMEM_PCI_ID: u32 = 0x1110_1af4;
const IVSHMEM_BAR_SIZE: usize = 0x1_0000;
const TEST_OFFSET: usize = 0x120;
const TEST_VALUE: u64 = 0x4956_5348_4d45_4d31;

#[cfg(feature = "arceos")]
fn main() {
    println!("ARCEOS_IVSHMEM_PCI_START");
    match run() {
        Ok(()) => println!("ARCEOS_IVSHMEM_PCI_PASS"),
        Err(error) => println!("ARCEOS_IVSHMEM_PCI_FAIL {error}"),
    }
}

#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
fn run() -> Result<(), String> {
    let ecam = map_device_range(ECAM_BASE, ECAM_SIZE, "ECAM")?;
    let identity = read_u32(ecam, PCI_ID_OFFSET);
    if identity != IVSHMEM_PCI_ID {
        return Err(format!(
            "unexpected PCI identity {identity:#010x}, expected {IVSHMEM_PCI_ID:#010x}"
        ));
    }

    let bar2 = usize::try_from(read_u32(ecam, PCI_BAR2_OFFSET) & 0xffff_fff0)
        .map_err(|_| "BAR2 address does not fit usize".to_string())?;
    if bar2 == 0 {
        return Err("BAR2 was not assigned".into());
    }
    let command = read_u16(ecam, PCI_COMMAND_OFFSET);
    write_u16(
        ecam,
        PCI_COMMAND_OFFSET,
        command | PCI_COMMAND_MEMORY_ENABLE,
    );

    let shared_memory = map_device_range(bar2, IVSHMEM_BAR_SIZE, "ivshmem BAR2")?;
    write_u64(shared_memory, TEST_OFFSET, TEST_VALUE);
    let actual = read_u64(shared_memory, TEST_OFFSET);
    if actual != TEST_VALUE {
        return Err(format!(
            "BAR2 readback mismatch: expected {TEST_VALUE:#018x}, got {actual:#018x}"
        ));
    }

    println!("ivshmem-pci identity={identity:#010x} bar2={bar2:#x}");
    Ok(())
}

#[cfg(feature = "arceos")]
fn map_device_range(base: usize, size: usize, name: &str) -> Result<NonNull<u8>, String> {
    let address = ax_mm::iomap(PhysAddr::from_usize(base), size)
        .map_err(|error| format!("map {name} at {base:#x}: {error}"))?;
    NonNull::new(address.as_mut_ptr()).ok_or_else(|| format!("{name} mapping returned null"))
}

#[cfg(feature = "arceos")]
fn read_u16(base: NonNull<u8>, offset: usize) -> u16 {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast::<u16>()) }
}

#[cfg(feature = "arceos")]
fn write_u16(base: NonNull<u8>, offset: usize, value: u16) {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast::<u16>(), value) }
}

#[cfg(feature = "arceos")]
fn read_u32(base: NonNull<u8>, offset: usize) -> u32 {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast::<u32>()) }
}

#[cfg(feature = "arceos")]
fn read_u64(base: NonNull<u8>, offset: usize) -> u64 {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast::<u64>()) }
}

#[cfg(feature = "arceos")]
fn write_u64(base: NonNull<u8>, offset: usize, value: u64) {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast::<u64>(), value) }
}
