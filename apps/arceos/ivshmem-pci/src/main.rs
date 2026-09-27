//! ArceOS guest smoke test for AxVisor's initial ivshmem PCI endpoint.

#[cfg(feature = "arceos")]
use core::ptr::NonNull;

#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use ax_std::os::arceos::modules::ax_hal::{self, mem::PhysAddr};

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
    let host = configured_pci_host()?;
    let ecam = map_device_range(host.ecam_base, ECAM_SIZE, "ECAM")?;
    let mut function_offset = None;
    for device in 0..32 {
        let offset = device << 15;
        if read_u32(ecam, offset + PCI_ID_OFFSET) == IVSHMEM_PCI_ID
            && function_offset.replace(offset).is_some()
        {
            return Err("multiple ivshmem endpoints found".into());
        }
    }
    let function_offset = function_offset.ok_or("ivshmem endpoint not found on bus 0")?;
    let bar2 = u64::from(read_u32(ecam, function_offset + PCI_BAR2_OFFSET) & 0xffff_fff0);
    if bar2 == 0 {
        return Err("BAR2 was not assigned".into());
    }
    let offset = bar2
        .checked_sub(host.memory_bus)
        .ok_or("BAR2 precedes PCI memory window")?;
    if offset
        .checked_add(IVSHMEM_BAR_SIZE as u64)
        .is_none_or(|end| end > host.memory_size)
    {
        return Err("BAR2 exceeds PCI memory window".into());
    }
    let address = host
        .memory_cpu
        .checked_add(offset)
        .ok_or("BAR2 CPU address overflows")?;
    let address = usize::try_from(address).map_err(|_| "BAR2 CPU address does not fit usize")?;
    let command = read_u16(ecam, function_offset + PCI_COMMAND_OFFSET);
    write_u16(
        ecam,
        function_offset + PCI_COMMAND_OFFSET,
        command | PCI_COMMAND_MEMORY_ENABLE,
    );

    let shared_memory = map_device_range(address, IVSHMEM_BAR_SIZE, "ivshmem BAR2")?;
    write_u64(shared_memory, TEST_OFFSET, TEST_VALUE);
    let actual = read_u64(shared_memory, TEST_OFFSET);
    if actual != TEST_VALUE {
        return Err(format!(
            "BAR2 readback mismatch: expected {TEST_VALUE:#018x}, got {actual:#018x}"
        ));
    }

    println!(
        "ivshmem-pci ecam={:#x} identity={IVSHMEM_PCI_ID:#010x} bar2={bar2:#x}",
        host.ecam_base
    );
    Ok(())
}

#[cfg(feature = "arceos")]
struct PciHost {
    ecam_base: usize,
    memory_bus: u64,
    memory_cpu: u64,
    memory_size: u64,
}

#[cfg(feature = "arceos")]
fn configured_pci_host() -> Result<PciHost, String> {
    let fdt = ax_hal::dtb::get_fdt().ok_or("boot FDT is unavailable")?;
    let mut hosts = fdt.find_compatible(&["pci-host-ecam-generic"]);
    let node = hosts.next().ok_or("boot FDT has no generic ECAM host")?;
    if hosts.next().is_some() {
        return Err("boot FDT describes multiple PCI hosts".into());
    }
    let mut registers = node.reg().ok_or("PCI host has no reg property")?;
    let ecam = registers.next().ok_or("PCI host has no ECAM window")?;
    if registers.next().is_some()
        || ecam.size != Some(ECAM_SIZE)
        || !ecam.address.is_multiple_of(ECAM_SIZE as u64)
    {
        return Err("PCI host must expose one aligned bus-zero ECAM window".into());
    }
    let pci = node.into_pci().ok_or("ECAM node is not a PCI bridge")?;
    if pci.bus_range() != Some(0..0) {
        return Err("PCI host must expose only bus 0".into());
    }
    let mut ranges = pci
        .ranges()
        .map_err(|error| format!("invalid PCI ranges: {error:?}"))?;
    let memory = ranges.next().ok_or("PCI host has no memory window")?;
    if ranges.next().is_some()
        || memory.space != fdt_parser::PciSpace::Memory32
        || memory.prefetchable
        || memory.size == 0
    {
        return Err("PCI host must expose one non-prefetchable memory32 window".into());
    }
    memory
        .bus_address
        .checked_add(memory.size)
        .filter(|end| *end <= 1 << 32)
        .ok_or("PCI memory window exceeds 4 GiB")?;
    memory
        .cpu_address
        .checked_add(memory.size)
        .ok_or("PCI CPU memory window overflows")?;
    Ok(PciHost {
        ecam_base: usize::try_from(ecam.address).map_err(|_| "ECAM address does not fit usize")?,
        memory_bus: memory.bus_address,
        memory_cpu: memory.cpu_address,
        memory_size: memory.size,
    })
}

#[cfg(feature = "arceos")]
fn map_device_range(base: usize, size: usize, name: &str) -> Result<NonNull<u8>, String> {
    let address = ax_mm::iomap(PhysAddr::from_usize(base), size)
        .map_err(|error| format!("map {name} at {base:#x}: {error}"))?;
    NonNull::new(address.as_mut_ptr()).ok_or_else(|| format!("{name} mapping returned null"))
}

#[cfg(feature = "arceos")]
fn read_u16(base: NonNull<u8>, offset: usize) -> u16 {
    // SAFETY: call sites use the mapped 1 MiB ECAM window and a device index
    // below 32. The command offset is word-aligned and within its 4 KiB function.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast::<u16>()) }
}

#[cfg(feature = "arceos")]
fn write_u16(base: NonNull<u8>, offset: usize, value: u16) {
    // SAFETY: call sites use the mapped 1 MiB ECAM window and a device index
    // below 32. The command offset is word-aligned and within its 4 KiB function.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast::<u16>(), value) }
}

#[cfg(feature = "arceos")]
fn read_u32(base: NonNull<u8>, offset: usize) -> u32 {
    // SAFETY: call sites use the mapped 1 MiB ECAM window and a device index
    // below 32. ID and BAR offsets are dword-aligned and within a 4 KiB function.
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
