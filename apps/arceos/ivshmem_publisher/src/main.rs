//! ArceOS peer 0 publishes a Message V1 broadcast to two Linux ivshmem peers.

#[cfg(feature = "arceos")]
use core::{
    ptr::NonNull,
    slice,
    sync::atomic::{AtomicU32, Ordering, fence},
};

#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use ax_std::os::arceos::modules::ax_hal::{self, mem::PhysAddr};
#[cfg(feature = "arceos")]
use axvisor_ivshmem::{
    Peer, READY, SECTION_SIZE, Section,
    message::{FRAGMENT_SIZE, MessageEndpoint},
};

#[cfg(feature = "arceos")]
const ECAM_SIZE: usize = 0x10_0000;
#[cfg(feature = "arceos")]
const BAR_SIZE: usize = 0x1_0000;
#[cfg(feature = "arceos")]
const PEERS: usize = 3;
#[cfg(feature = "arceos")]
const REQUEST: &[u8] = b"hello ivshmem subscribers";
#[cfg(feature = "arceos")]
const REPLY: &[u8] = b"ack";
#[cfg(feature = "arceos")]
const TIMEOUT_NANOS: u64 = 120_000_000_000;

#[cfg(feature = "arceos")]
fn main() {
    println!("IVSHMEM_PUBLISHER_START");
    match run() {
        Ok(()) => println!("IVSHMEM_PUBLISHER_PASSED"),
        Err(error) => println!("IVSHMEM_PUBLISHER_FAILED {error}"),
    }
}
#[cfg(not(feature = "arceos"))]
fn main() {}

#[cfg(feature = "arceos")]
fn run() -> Result<(), String> {
    let host = pci_host()?;
    let ecam = map_device(host.ecam, ECAM_SIZE)?;
    let mut function = None;
    for device in 0..32 {
        let offset = device << 15;
        if read32(ecam, offset) == 0x1110_1af4 && function.replace(offset).is_some() {
            return Err("multiple ivshmem PCI functions".into());
        }
    }
    let function = function.ok_or("ivshmem PCI function not found")?;
    let bar0 = read32(ecam, function + 0x10) & !0xf;
    let bar2 = read32(ecam, function + 0x18) & !0xf;
    let bar0 = map_device(host.bar_cpu(bar0, 0x1000)?, 0x1000)?;
    let bar2_address = host.bar_cpu(bar2, BAR_SIZE)?;
    let command = read16(ecam, function + 4);
    write16(ecam, function + 4, command | 2);
    if read32(bar0, 0) != 0 || read32(bar0, 4) != PEERS as u32 {
        return Err("unexpected ivshmem peer identity or profile".into());
    }
    let shared = ax_mm::iomap_cached(PhysAddr::from_usize(bar2_address), BAR_SIZE)
        .map_err(|error| format!("map coherent BAR2: {error}"))?;
    let shared = shared.as_ptr();
    // SAFETY: BAR2 remains mapped until shutdown. The profile places the
    // read-only state table in page 0 and three contiguous 4 KiB output
    // sections in pages 1..4; all atomic fields are aligned and zero-filled.
    let states = unsafe { slice::from_raw_parts(shared.cast::<AtomicU32>(), PEERS) };
    // SAFETY: Same mapping, page-aligned output range, exact profile size.
    let sections =
        unsafe { slice::from_raw_parts(shared.add(SECTION_SIZE).cast::<Section>(), PEERS) };
    // SAFETY: Only this VM owns peer 0's output; the other output pages are
    // stage-2 read-only. Linux peers attach independently once per link.
    let mut peer = unsafe { Peer::attach(0, sections, states) }
        .map_err(|error| format!("attach peer: {error:?}"))?;
    peer.initialize()
        .map_err(|error| format!("initialize peer: {error:?}"))?;
    fence(Ordering::SeqCst);
    write32(bar0, 0x10, READY);
    let deadline = now().saturating_add(TIMEOUT_NANOS);
    while states[1].load(Ordering::Acquire) != READY || states[2].load(Ordering::Acquire) != READY {
        ensure_time(deadline)?;
        core::hint::spin_loop();
    }
    let mut endpoint = MessageEndpoint::new(peer);
    endpoint
        .start_message(axvisor_ivshmem::BROADCAST, REQUEST.len() as u64)
        .map_err(|error| format!("start broadcast: {error:?}"))?;
    let send = endpoint
        .try_write(REQUEST)
        .map_err(|error| format!("send broadcast: {error:?}"))?;
    if !send.complete || send.consumed != REQUEST.len() {
        return Err("broadcast did not fit an empty ring".into());
    }
    ring(bar0, 1);
    ring(bar0, 2);

    let mut acked = [false; PEERS];
    let mut output = [0u8; FRAGMENT_SIZE];
    while !acked[1] || !acked[2] {
        ensure_time(deadline)?;
        let mut invalid = false;
        endpoint
            .poll(&mut output, |frame, bytes| {
                let source = frame.source as usize;
                if source == 0
                    || source >= PEERS
                    || acked[source]
                    || frame.destination != 0
                    || frame.id != 1
                    || frame.message_len != REPLY.len() as u64
                    || !frame.complete
                    || bytes != REPLY
                {
                    invalid = true;
                } else {
                    acked[source] = true;
                }
            })
            .map_err(|error| format!("receive acknowledgements: {error:?}"))?;
        if invalid {
            return Err("invalid or duplicate subscriber acknowledgement".into());
        }
        if !acked[1] || !acked[2] {
            let _ = wait_event(bar0, 1_000_000);
        }
    }
    Ok(())
}

#[cfg(feature = "arceos")]
struct PciHost {
    ecam: usize,
    bus: u64,
    cpu: u64,
    size: u64,
}
#[cfg(feature = "arceos")]
impl PciHost {
    fn bar_cpu(&self, bar: u32, size: usize) -> Result<usize, String> {
        let offset = u64::from(bar)
            .checked_sub(self.bus)
            .ok_or("BAR before PCI memory window")?;
        if bar == 0
            || offset
                .checked_add(size as u64)
                .is_none_or(|end| end > self.size)
        {
            return Err("BAR outside PCI memory window".into());
        }
        usize::try_from(self.cpu.checked_add(offset).ok_or("PCI address overflow")?)
            .map_err(|_| "BAR address does not fit usize".into())
    }
}
#[cfg(feature = "arceos")]
fn pci_host() -> Result<PciHost, String> {
    let fdt = ax_hal::dtb::get_fdt().ok_or("missing FDT")?;
    let mut hosts = fdt.find_compatible(&["pci-host-ecam-generic"]);
    let node = hosts.next().ok_or("missing PCI ECAM host")?;
    if hosts.next().is_some() {
        return Err("ambiguous PCI ECAM host".into());
    }
    let mut regs = node.reg().ok_or("ECAM host missing reg")?;
    let ecam = regs.next().ok_or("ECAM host missing aperture")?;
    if regs.next().is_some()
        || ecam.size != Some(ECAM_SIZE)
        || !ecam.address.is_multiple_of(ECAM_SIZE as u64)
    {
        return Err("invalid ECAM window".into());
    }
    let pci = node.into_pci().ok_or("ECAM node is not a PCI bridge")?;
    if pci.bus_range() != Some(0..0) {
        return Err("unexpected PCI bus range".into());
    }
    let mut ranges = pci
        .ranges()
        .map_err(|error| format!("invalid PCI ranges: {error:?}"))?;
    let memory = ranges.next().ok_or("missing PCI memory range")?;
    if ranges.next().is_some() || memory.space != fdt_parser::PciSpace::Memory32 || memory.size == 0
    {
        return Err("invalid PCI memory range".into());
    }
    Ok(PciHost {
        ecam: usize::try_from(ecam.address).map_err(|_| "ECAM address overflow")?,
        bus: memory.bus_address,
        cpu: memory.cpu_address,
        size: memory.size,
    })
}
#[cfg(feature = "arceos")]
fn map_device(base: usize, size: usize) -> Result<NonNull<u8>, String> {
    let address = ax_mm::iomap(PhysAddr::from_usize(base), size)
        .map_err(|error| format!("map device {base:#x}: {error}"))?;
    NonNull::new(address.as_mut_ptr()).ok_or_else(|| "null device mapping".into())
}
#[cfg(feature = "arceos")]
fn read32(base: NonNull<u8>, offset: usize) -> u32 {
    // SAFETY: Offset is aligned and within the mapped ECAM function or BAR0.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast()) }
}
#[cfg(feature = "arceos")]
fn write32(base: NonNull<u8>, offset: usize, value: u32) {
    // SAFETY: Offset is aligned and within the mapped BAR0.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast(), value) }
}
#[cfg(feature = "arceos")]
fn read16(base: NonNull<u8>, offset: usize) -> u16 {
    // SAFETY: Offset is aligned and within the mapped ECAM function.
    unsafe { core::ptr::read_volatile(base.as_ptr().add(offset).cast()) }
}
#[cfg(feature = "arceos")]
fn write16(base: NonNull<u8>, offset: usize, value: u16) {
    // SAFETY: Offset is aligned and within the mapped ECAM function.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast(), value) }
}
#[cfg(feature = "arceos")]
fn ring(regs: NonNull<u8>, peer: u32) {
    fence(Ordering::SeqCst);
    write32(regs, 0x0c, peer << 16);
}
#[cfg(feature = "arceos")]
fn wait_event(regs: NonNull<u8>, nanos: u64) -> bool {
    let deadline = now().saturating_add(nanos);
    while now() < deadline {
        if read32(regs, 0x14) & 1 != 0 {
            write32(regs, 0x14, 1);
            return true;
        }
        core::hint::spin_loop();
    }
    false
}
#[cfg(feature = "arceos")]
fn now() -> u64 {
    ax_hal::time::monotonic_time_nanos()
}
#[cfg(feature = "arceos")]
fn ensure_time(deadline: u64) -> Result<(), String> {
    if now() >= deadline {
        Err("ivshmem message exchange timed out".into())
    } else {
        Ok(())
    }
}
