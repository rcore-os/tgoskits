//! ArceOS guest smoke test for AxVisor's initial ivshmem PCI endpoint.
//!
//! With the bootargs marker `axvisor.pci_case=ivshmem-cross-peer` the smoke
//! additionally runs the dual-peer handshake shared with the Linux smoke
//! (`apps/linux/ivshmem/bar2_smoke/main.c`): the initiator (peer 0)
//! publishes a request mailbox in its own output section and rings the
//! responder, the responder validates the request, publishes its reply and
//! rings back. Both sides observe the remote peer's state through the BAR2
//! state table. Event waiting polls the BAR0 Event Status register with
//! write-1-to-clear, matching the Linux polling backend's semantics.
//!
//! `axvisor.pci_case=ivshmem-suite-arceos` selects the consolidated three-peer
//! protocol. Peer 0 is the primary Linux guest, peer 1 is this ArceOS guest,
//! and peer 2 is a portable role temporarily implemented by Linux until the
//! planned Zephyr demo is available.

#[cfg(feature = "arceos")]
use core::ptr::NonNull;

#[cfg(feature = "arceos")]
use ax_std as _;
#[cfg(feature = "arceos")]
use ax_std::os::arceos::modules::ax_hal::{self, mem::PhysAddr};

const ECAM_SIZE: usize = 0x10_0000;
const PCI_ID_OFFSET: usize = 0x00;
const PCI_COMMAND_OFFSET: usize = 0x04;
const PCI_BAR0_OFFSET: usize = 0x10;
const PCI_BAR2_OFFSET: usize = 0x18;
const PCI_COMMAND_MEMORY_ENABLE: u16 = 1 << 1;
const IVSHMEM_PCI_ID: u32 = 0x1110_1af4;
const IVSHMEM_BAR_SIZE: usize = 0x1_0000;
// F4 reserves the first BAR2 page for the state table; F5/F6 give every peer
// one 28 KiB output section starting at 0x1000 (peer N: 0x1000 +
// N * 0x7000), writable only by its owner. The test payload must stay
// inside this peer's own section.
const SMOKE_OUTPUT_SECTION_BASE: usize = 0x1000;
const SMOKE_OUTPUT_SECTION_STRIDE: usize = 0x7000;
const SUITE_OUTPUT_SECTION_STRIDE: usize = 0x5000;
const SUITE_MAX_PEERS: u32 = 3;
const SUITE_LINUX_PEER: u32 = 0;
const SUITE_ARCEOS_PEER: u32 = 1;
const SUITE_PORTABLE_PEER: u32 = 2;
const IVSHMEM_REG_ID: usize = 0x00;
const IVSHMEM_REG_MAX_PEERS: usize = 0x04;
const IVSHMEM_REG_DOORBELL: usize = 0x0c;
const IVSHMEM_REG_STATE: usize = 0x10;
const IVSHMEM_REG_EVENT_STATUS: usize = 0x14;
const IVSHMEM_BAR0_SIZE: usize = 0x1000;
const TEST_OFFSET_IN_SECTION: usize = 0x100;
const TEST_VALUE: u64 = 0x4956_5348_4d45_4d31;

// Cross-peer mailbox magic ("IVCP") and the BAR0 state values each side
// publishes for the handshake; identical to the Linux smoke's contract.
const MAILBOX_MAGIC: u32 = 0x4956_4350;
const HANDSHAKE_STATE_SELF: u32 = 0x0001_0002;
const HANDSHAKE_STATE_INITIATOR: u32 = 0x0001_0003;
const HANDSHAKE_STATE_RESPONDER: u32 = 0x0001_0004;
const HANDSHAKE_STATE_READY: u32 = 0x0001_0005;
const MAILBOX_PAYLOAD_SIZE: usize = 0x100;

const SUITE_MAILBOX_MAGIC: u32 = 0x4956_5355;
const SUITE_PROTOCOL_VERSION: u32 = 1;
const SUITE_PHASE_ARCEOS_TO_LINUX_IRQ: u32 = 2;
const SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ: u32 = 2;
const SUITE_STATE_LINUX_IRQ_READY: u32 = 0x3100_0101;
const SUITE_STATE_LINUX_IRQ_REPLIED: u32 = 0x3100_0103;
const SUITE_STATE_ARCEOS_BOOTSTRAP: u32 = 0x3200_0000;
const SUITE_STATE_ARCEOS_OBSERVER_ARMED: u32 = 0x3200_0001;
const SUITE_STATE_ARCEOS_OBSERVER_CLEAN: u32 = 0x3200_0002;
const SUITE_STATE_ARCEOS_REQUEST: u32 = 0x3200_0003;
const SUITE_STATE_ARCEOS_DONE: u32 = 0x3200_0004;
const SUITE_STATE_PORTABLE_RECEIVED: u32 = 0x3300_0002;
const SUITE_STATE_PORTABLE_OBSERVER_ARMED: u32 = 0x3300_0004;
const ARCEOS_TO_LINUX_REQUEST: &[u8] = b"Hello Linux primary, ArceOS requests an MSI-X round trip.";
const LINUX_TO_ARCEOS_REPLY: &[u8] = b"Hello ArceOS, Linux primary received the MSI-X message.";

// Guest scheduling is not part of the device contract: the handshake waits
// long enough for the other guest to reach its smoke.
const HANDSHAKE_TIMEOUT_NANOS: u64 = 30_000_000_000;
const SELF_DOORBELL_TIMEOUT_NANOS: u64 = 5_000_000_000;
const SELF_DOORBELL_UNAVAILABLE_WAIT_NANOS: u64 = 200_000_000;

// The bootargs marker that switches the smoke into the dual-peer handshake;
// the guest FDT carries it from the VM config's [kernel] cmdline.
const CROSS_PEER_BOOTARGS_MARKER: &str = "axvisor.pci_case=ivshmem-cross-peer";
const SUITE_BOOTARGS_MARKER: &str = "axvisor.pci_case=ivshmem-suite-arceos";

#[cfg(feature = "arceos")]
fn main() {
    println!("ARCEOS_IVSHMEM_PCI_START");
    match run() {
        Ok(()) => println!("ARCEOS_IVSHMEM_PCI_PASS"),
        Err(error) => {
            println!("ARCEOS_IVSHMEM_PCI_FAIL {error}");
            if ax_hal_bootargs_contains(SUITE_BOOTARGS_MARKER) {
                println!(
                    "IVSHMEM_PCI_SUITE_FAILED peer=1 role=arceos phase=run step=guest \
                     detail={error}"
                );
            }
        }
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
    let bar2 = read_u32(ecam, function_offset + PCI_BAR2_OFFSET) & 0xffff_fff0;
    let bar0 = read_u32(ecam, function_offset + PCI_BAR0_OFFSET) & 0xffff_fff0;
    let bar2_address = host.bar_cpu_address(bar2, IVSHMEM_BAR_SIZE)?;
    let bar0_address = host.bar_cpu_address(bar0, IVSHMEM_BAR0_SIZE)?;
    let command = read_u16(ecam, function_offset + PCI_COMMAND_OFFSET);
    write_u16(
        ecam,
        function_offset + PCI_COMMAND_OFFSET,
        command | PCI_COMMAND_MEMORY_ENABLE,
    );

    // Read the identity registers before accessing this peer's output section.
    let registers = map_device_range(bar0_address, IVSHMEM_BAR0_SIZE, "ivshmem BAR0")?;
    let peer_id = read_u32(registers, IVSHMEM_REG_ID);
    let max_peers = read_u32(registers, IVSHMEM_REG_MAX_PEERS);
    let suite = ax_hal_bootargs_contains(SUITE_BOOTARGS_MARKER);
    let cross_peer = ax_hal_bootargs_contains(CROSS_PEER_BOOTARGS_MARKER);
    let expected_max_peers = if suite {
        SUITE_MAX_PEERS
    } else if cross_peer {
        2
    } else {
        1
    };
    if max_peers != expected_max_peers
        || peer_id >= max_peers
        || (suite && peer_id != SUITE_ARCEOS_PEER)
    {
        return Err(format!(
            "profile peer_id={peer_id} max_peers={max_peers}, expected peer_id={} \
             max_peers={expected_max_peers}",
            if suite { SUITE_ARCEOS_PEER } else { peer_id }
        ));
    }

    let output_stride = if suite {
        SUITE_OUTPUT_SECTION_STRIDE
    } else {
        SMOKE_OUTPUT_SECTION_STRIDE
    };
    let own_section = SMOKE_OUTPUT_SECTION_BASE + peer_id as usize * output_stride;
    let test_offset = own_section + TEST_OFFSET_IN_SECTION;

    let shared_memory = map_device_range(bar2_address, IVSHMEM_BAR_SIZE, "ivshmem BAR2")?;
    write_u64(shared_memory, test_offset, TEST_VALUE);
    let actual = read_u64(shared_memory, test_offset);
    if actual != TEST_VALUE {
        return Err(format!(
            "BAR2 readback mismatch: expected {TEST_VALUE:#018x}, got {actual:#018x}"
        ));
    }

    // The BAR0 State write must surface in the shared state table (F4);
    // this peer's entry sits at BAR2 offset `peer_id * 4` in the first page.
    let state_table = shared_memory.as_ptr() as *const u32;
    let bootstrap_state = if suite {
        SUITE_STATE_ARCEOS_BOOTSTRAP
    } else {
        HANDSHAKE_STATE_SELF
    };
    write_u32(registers, IVSHMEM_REG_STATE, bootstrap_state);
    // SAFETY: the BAR2 mapping covers the whole state table page and the
    // peer entry offset is inside it.
    let own_state = unsafe { core::ptr::read_volatile(state_table.add(peer_id as usize)) };
    if own_state != bootstrap_state {
        return Err(format!(
            "BAR0 state write did not surface in the state table: {own_state:#010x}"
        ));
    }
    println!(
        "ivshmem-pci ecam={:#x} identity={IVSHMEM_PCI_ID:#010x} bar2={bar2:#x} peer_id={peer_id} \
         test_offset={test_offset:#x} cross_peer={cross_peer} suite={suite}",
        host.ecam_base
    );

    if suite {
        self_doorbell_tests(registers, peer_id)?;
        run_suite_arceos_peer(registers, shared_memory)?;
    } else if cross_peer {
        if peer_id == 0 {
            // Initiator: self-doorbell coverage first, then the exchange.
            self_doorbell_tests(registers, peer_id)?;
            cross_peer_exchange(registers, shared_memory, peer_id)?;
        } else {
            // The responder exchange consumes the request first and runs the
            // self-doorbell checks before publishing its reply. Therefore the
            // initiator's final pass marker proves both ArceOS paths completed.
            cross_peer_exchange(registers, shared_memory, peer_id)?;
        }
    } else {
        self_doorbell_tests(registers, peer_id)?;
    }
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
impl PciHost {
    fn bar_cpu_address(&self, bus_address: u32, size: usize) -> Result<usize, String> {
        if bus_address == 0 {
            return Err("PCI BAR was not assigned".into());
        }
        let offset = u64::from(bus_address)
            .checked_sub(self.memory_bus)
            .ok_or("BAR precedes PCI memory window")?;
        if offset
            .checked_add(size as u64)
            .is_none_or(|end| end > self.memory_size)
        {
            return Err("BAR exceeds PCI memory window".into());
        }
        let address = self
            .memory_cpu
            .checked_add(offset)
            .ok_or("BAR CPU address overflows")?;
        usize::try_from(address).map_err(|_| "BAR CPU address does not fit usize".into())
    }
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
    if ranges.next().is_some() || memory.space != fdt_parser::PciSpace::Memory32 || memory.size == 0
    {
        return Err("PCI host must expose one memory32 window".into());
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

/// Waits until Event Status bit 0 pends and clears it with write-1-to-clear.
///
/// Returns `true` when an event was observed, `false` on timeout.
#[cfg(feature = "arceos")]
fn wait_event(registers: NonNull<u8>, timeout_nanos: u64) -> bool {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(timeout_nanos);
    while ax_hal_monotonic_time_nanos() < deadline {
        if read_u32(registers, IVSHMEM_REG_EVENT_STATUS) & 1 != 0 {
            write_u32(registers, IVSHMEM_REG_EVENT_STATUS, 1);
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Rings this endpoint's own doorbell twice and verifies the W1C/re-pend
/// behavior, then checks that an unsupported vector produces no event.
#[cfg(feature = "arceos")]
fn self_doorbell_tests(registers: NonNull<u8>, peer_id: u32) -> Result<(), String> {
    let doorbell = |target: u32, vector: u32| {
        write_u32(registers, IVSHMEM_REG_DOORBELL, (target << 16) | vector);
    };

    doorbell(peer_id, 0);
    if !wait_event(registers, SELF_DOORBELL_TIMEOUT_NANOS) {
        return Err("first self doorbell timed out".into());
    }
    if read_u32(registers, IVSHMEM_REG_EVENT_STATUS) & 1 != 0 {
        return Err("event status was not cleared by the wait".into());
    }

    doorbell(peer_id, 0);
    if !wait_event(registers, SELF_DOORBELL_TIMEOUT_NANOS) {
        return Err("second self doorbell timed out".into());
    }

    // Vector 1 is outside the current profile: the doorbell is a no-op and
    // no event may arrive.
    doorbell(peer_id, 1);
    if wait_event(registers, SELF_DOORBELL_UNAVAILABLE_WAIT_NANOS) {
        return Err("an unsupported vector produced an event".into());
    }
    Ok(())
}

/// Runs the cross-peer mailbox exchange shared with the Linux smoke.
///
/// The initiator (peer 0) publishes the request in its own output section,
/// publishes its handshake state and rings the responder; the responder
/// validates the request, publishes its reply and rings back. The
/// `fill_mailbox` order (payload, then state, then doorbell) and the remote
/// state poll make the payload visible before the reply is validated.
#[cfg(feature = "arceos")]
fn cross_peer_exchange(
    registers: NonNull<u8>,
    shared_memory: NonNull<u8>,
    peer_id: u32,
) -> Result<(), String> {
    let target = peer_id ^ 1;
    let own_mailbox = shared_memory.as_ptr() as usize
        + SMOKE_OUTPUT_SECTION_BASE
        + peer_id as usize * SMOKE_OUTPUT_SECTION_STRIDE;
    let remote_mailbox = shared_memory.as_ptr() as usize
        + SMOKE_OUTPUT_SECTION_BASE
        + target as usize * SMOKE_OUTPUT_SECTION_STRIDE;

    if peer_id == 0 {
        fill_mailbox(own_mailbox, 0x5a)?;
        write_u32(registers, IVSHMEM_REG_STATE, HANDSHAKE_STATE_INITIATOR);
        // The doorbell is a Device write and does not order the prior
        // Normal-memory mailbox and state stores.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_u32(registers, IVSHMEM_REG_DOORBELL, target << 16);
        println!("ivshmem-pci checkpoint cross-peer-request");

        if !wait_event(registers, HANDSHAKE_TIMEOUT_NANOS) {
            return Err("cross-peer reply event timed out".into());
        }
        wait_remote_state(shared_memory, target, HANDSHAKE_STATE_RESPONDER)?;
        validate_mailbox(remote_mailbox)?;
        println!("ivshmem-pci checkpoint cross-peer-reply");
    } else {
        // Publish readiness only after the event path and shared mappings are
        // usable, matching the Linux responder protocol.
        write_u32(registers, IVSHMEM_REG_STATE, HANDSHAKE_STATE_READY);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        println!("ivshmem-pci checkpoint cross-peer-ready");

        if !wait_event(registers, HANDSHAKE_TIMEOUT_NANOS) {
            return Err("cross-peer request event timed out".into());
        }
        wait_remote_state(shared_memory, target, HANDSHAKE_STATE_INITIATOR)?;
        validate_mailbox(remote_mailbox)?;

        // Event Status is clear after consuming the request, so self events
        // cannot merge with and hide that request. Complete all local event
        // checks before replying; the Linux initiator only passes after this
        // peer reaches the reply doorbell below.
        self_doorbell_tests(registers, peer_id)?;
        fill_mailbox(own_mailbox, 0xa5)?;
        write_u32(registers, IVSHMEM_REG_STATE, HANDSHAKE_STATE_RESPONDER);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        write_u32(registers, IVSHMEM_REG_DOORBELL, target << 16);
        println!("ivshmem-pci checkpoint cross-peer-relay");
    }
    Ok(())
}

/// Polls the remote peer's state-table entry until it reaches `expected`.
#[cfg(feature = "arceos")]
fn wait_remote_state(
    shared_memory: NonNull<u8>,
    peer_id: u32,
    expected: u32,
) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos() + 1_000_000_000;
    let entry = (shared_memory.as_ptr() as usize + peer_id as usize * 4) as *const u32;
    // SAFETY: the BAR2 mapping covers the state table page and the peer
    // entry offset is inside it.
    while unsafe { core::ptr::read_volatile(entry) } != expected {
        if ax_hal_monotonic_time_nanos() >= deadline {
            return Err(format!("remote peer state did not reach {expected:#010x}"));
        }
        core::hint::spin_loop();
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn payload_checksum(payload: &[u8]) -> u32 {
    let mut checksum: u32 = 0x4956_5348;
    for &byte in payload {
        checksum = checksum.wrapping_mul(31).wrapping_add(u32::from(byte));
    }
    checksum
}

/// Writes one mailbox: payload first, then magic and the payload checksum,
/// so a reader that observes the magic also observes the payload.
#[cfg(feature = "arceos")]
fn fill_mailbox(mailbox: usize, seed: u32) -> Result<(), String> {
    // SAFETY: the mailbox lives inside this peer's own output section, which
    // the F5/F6 mapping makes writable, and the mailbox layout fits the 28
    // KiB section.
    let mailbox = NonNull::new(mailbox as *mut u8).ok_or("mailbox pointer is null")?;
    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u32 * 7 + seed) as u8;
    }
    for (index, byte) in payload.iter().enumerate() {
        // SAFETY: the payload offset is naturally aligned and contained in
        // the peer-owned output section mapping.
        unsafe {
            core::ptr::write_volatile(mailbox.as_ptr().add(0x10 + index), *byte);
        }
    }
    let checksum = payload_checksum(&payload);
    write_u32(mailbox, 0x00, MAILBOX_MAGIC);
    write_u32(mailbox, 0x04, checksum);
    Ok(())
}

#[cfg(feature = "arceos")]
fn validate_mailbox(mailbox: usize) -> Result<(), String> {
    // SAFETY: the mailbox lives inside the remote peer's output section,
    // which the F5/F6 mapping makes readable, and the reads are aligned and
    // contained in the 28 KiB section.
    let mailbox = NonNull::new(mailbox as *mut u8).ok_or("mailbox pointer is null")?;
    let magic = read_u32(mailbox, 0x00);
    let checksum = read_u32(mailbox, 0x04);
    if magic != MAILBOX_MAGIC {
        return Err(format!("cross-peer mailbox magic mismatch: {magic:#010x}"));
    }
    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    for (index, byte) in payload.iter_mut().enumerate() {
        // SAFETY: the payload offset is naturally aligned and contained in
        // the remote peer's output section mapping.
        unsafe {
            *byte = core::ptr::read_volatile(mailbox.as_ptr().add(0x10 + index));
        }
    }
    if checksum != payload_checksum(&payload) {
        return Err("cross-peer payload checksum mismatch".into());
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_suite_arceos_peer(registers: NonNull<u8>, shared_memory: NonNull<u8>) -> Result<(), String> {
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=observer step=ready");
    write_u32(
        registers,
        IVSHMEM_REG_STATE,
        SUITE_STATE_ARCEOS_OBSERVER_ARMED,
    );
    ensure_no_event_until_state(
        registers,
        shared_memory,
        SUITE_PORTABLE_PEER,
        SUITE_STATE_PORTABLE_RECEIVED,
    )?;
    write_u32(
        registers,
        IVSHMEM_REG_STATE,
        SUITE_STATE_ARCEOS_OBSERVER_CLEAN,
    );
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=observer step=clean");

    wait_suite_state(
        shared_memory,
        SUITE_PORTABLE_PEER,
        SUITE_STATE_PORTABLE_OBSERVER_ARMED,
    )?;
    wait_suite_state(shared_memory, SUITE_LINUX_PEER, SUITE_STATE_LINUX_IRQ_READY)?;

    let own_mailbox = suite_mailbox(shared_memory, SUITE_ARCEOS_PEER)?;
    fill_suite_mailbox(
        own_mailbox,
        SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
        SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ,
        SUITE_ARCEOS_PEER,
        SUITE_LINUX_PEER,
        ARCEOS_TO_LINUX_REQUEST,
    )?;
    log_suite_mailbox("tx", own_mailbox, "doorbell")?;
    write_u32(registers, IVSHMEM_REG_STATE, SUITE_STATE_ARCEOS_REQUEST);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    write_u32(registers, IVSHMEM_REG_DOORBELL, SUITE_LINUX_PEER << 16);
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=interrupt-request step=sent");

    if !wait_event(registers, HANDSHAKE_TIMEOUT_NANOS) {
        return Err("suite interrupt reply timed out".into());
    }
    wait_suite_state(
        shared_memory,
        SUITE_LINUX_PEER,
        SUITE_STATE_LINUX_IRQ_REPLIED,
    )?;
    let linux_mailbox = suite_mailbox(shared_memory, SUITE_LINUX_PEER)?;
    validate_suite_mailbox(
        linux_mailbox,
        SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
        SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ,
        SUITE_LINUX_PEER,
        SUITE_ARCEOS_PEER,
        LINUX_TO_ARCEOS_REPLY,
    )?;
    log_suite_mailbox("rx", linux_mailbox, "polling")?;
    write_u32(registers, IVSHMEM_REG_STATE, SUITE_STATE_ARCEOS_DONE);
    println!("IVSHMEM_PCI_SUITE_ROLE_PASSED peer=1 role=arceos");
    Ok(())
}

#[cfg(feature = "arceos")]
fn suite_mailbox(shared_memory: NonNull<u8>, peer_id: u32) -> Result<NonNull<u8>, String> {
    let address = shared_memory.as_ptr() as usize
        + SMOKE_OUTPUT_SECTION_BASE
        + peer_id as usize * SUITE_OUTPUT_SECTION_STRIDE;
    NonNull::new(address as *mut u8).ok_or_else(|| "suite mailbox pointer is null".into())
}

#[cfg(feature = "arceos")]
fn fill_suite_mailbox(
    mailbox: NonNull<u8>,
    phase: u32,
    sequence: u32,
    source_peer: u32,
    target_peer: u32,
    message: &[u8],
) -> Result<(), String> {
    if message.is_empty() || message.len() > MAILBOX_PAYLOAD_SIZE {
        return Err("outgoing suite message length is invalid".into());
    }

    write_u32(mailbox, 0x00, 0);
    for index in 0..MAILBOX_PAYLOAD_SIZE {
        // SAFETY: the payload starts at offset 0x20 and the complete suite
        // mailbox fits in the peer-owned 20 KiB output section.
        unsafe {
            core::ptr::write_volatile(mailbox.as_ptr().add(0x20 + index), 0);
        }
    }
    for (index, byte) in message.iter().copied().enumerate() {
        // SAFETY: the message length was checked against the payload capacity.
        unsafe {
            core::ptr::write_volatile(mailbox.as_ptr().add(0x20 + index), byte);
        }
    }
    write_u32(mailbox, 0x04, SUITE_PROTOCOL_VERSION);
    write_u32(mailbox, 0x08, phase);
    write_u32(mailbox, 0x0c, sequence);
    write_u32(mailbox, 0x10, source_peer);
    write_u32(mailbox, 0x14, target_peer);
    write_u32(mailbox, 0x18, payload_checksum(message));
    write_u32(mailbox, 0x1c, message.len() as u32);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    write_u32(mailbox, 0x00, SUITE_MAILBOX_MAGIC);
    Ok(())
}

#[cfg(feature = "arceos")]
fn validate_suite_mailbox(
    mailbox: NonNull<u8>,
    phase: u32,
    sequence: u32,
    source_peer: u32,
    target_peer: u32,
    expected_message: &[u8],
) -> Result<(), String> {
    if read_u32(mailbox, 0x00) != SUITE_MAILBOX_MAGIC {
        return Err("suite mailbox magic mismatch".into());
    }
    if read_u32(mailbox, 0x04) != SUITE_PROTOCOL_VERSION {
        return Err("suite mailbox protocol version mismatch".into());
    }
    if read_u32(mailbox, 0x08) != phase || read_u32(mailbox, 0x0c) != sequence {
        return Err("suite mailbox phase or sequence mismatch".into());
    }
    if read_u32(mailbox, 0x10) != source_peer || read_u32(mailbox, 0x14) != target_peer {
        return Err("suite mailbox route mismatch".into());
    }
    let payload_size = usize::try_from(read_u32(mailbox, 0x1c))
        .map_err(|_| "suite mailbox payload size does not fit usize")?;
    if payload_size != expected_message.len() || payload_size > MAILBOX_PAYLOAD_SIZE {
        return Err("suite mailbox payload size mismatch".into());
    }

    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    for (index, byte) in payload.iter_mut().take(payload_size).enumerate() {
        // SAFETY: the validated payload size fits in the remote suite mailbox.
        unsafe {
            *byte = core::ptr::read_volatile(mailbox.as_ptr().add(0x20 + index));
        }
    }
    let message = &payload[..payload_size];
    if read_u32(mailbox, 0x18) != payload_checksum(message) {
        return Err("suite mailbox payload checksum mismatch".into());
    }
    if message != expected_message {
        return Err("suite mailbox message content mismatch".into());
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn log_suite_mailbox(action: &str, mailbox: NonNull<u8>, path: &str) -> Result<(), String> {
    let payload_size = usize::try_from(read_u32(mailbox, 0x1c))
        .map_err(|_| "suite mailbox payload size does not fit usize")?;
    if payload_size > MAILBOX_PAYLOAD_SIZE {
        return Err("suite mailbox payload is too large to log".into());
    }

    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    for (index, byte) in payload.iter_mut().take(payload_size).enumerate() {
        // SAFETY: the checked payload size fits in the mapped suite mailbox.
        unsafe {
            *byte = core::ptr::read_volatile(mailbox.as_ptr().add(0x20 + index));
        }
    }
    let content = core::str::from_utf8(&payload[..payload_size])
        .map_err(|_| "suite mailbox message is not UTF-8")?;
    println!(
        "IVSHMEM_PCI_SUITE_MESSAGE action={action} phase={} sequence={} source={} target={} \
         path={path} content=\"{content}\"",
        read_u32(mailbox, 0x08),
        read_u32(mailbox, 0x0c),
        read_u32(mailbox, 0x10),
        read_u32(mailbox, 0x14),
    );
    Ok(())
}

#[cfg(feature = "arceos")]
fn wait_suite_state(shared_memory: NonNull<u8>, peer_id: u32, expected: u32) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(HANDSHAKE_TIMEOUT_NANOS);
    let entry = (shared_memory.as_ptr() as usize + peer_id as usize * 4) as *const u32;

    // SAFETY: the BAR2 mapping covers all three state-table entries.
    while unsafe { core::ptr::read_volatile(entry) } != expected {
        if ax_hal_monotonic_time_nanos() >= deadline {
            // SAFETY: the same state-table entry remains mapped for the test.
            let actual = unsafe { core::ptr::read_volatile(entry) };
            return Err(format!(
                "suite peer {peer_id} state is {actual:#010x}, expected {expected:#010x}"
            ));
        }
        core::hint::spin_loop();
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn ensure_no_event_until_state(
    registers: NonNull<u8>,
    shared_memory: NonNull<u8>,
    peer_id: u32,
    expected: u32,
) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(HANDSHAKE_TIMEOUT_NANOS);
    let entry = (shared_memory.as_ptr() as usize + peer_id as usize * 4) as *const u32;

    // The portable peer may advance from RECEIVED to REPLIED before this
    // observer samples it, so this phase deliberately accepts later states.
    // SAFETY: the BAR2 mapping covers all three state-table entries.
    while unsafe { core::ptr::read_volatile(entry) } < expected {
        if read_u32(registers, IVSHMEM_REG_EVENT_STATUS) & 1 != 0 {
            return Err("portable-directed doorbell reached ArceOS".into());
        }
        if ax_hal_monotonic_time_nanos() >= deadline {
            return Err("portable peer did not receive the directed doorbell".into());
        }
        core::hint::spin_loop();
    }
    if read_u32(registers, IVSHMEM_REG_EVENT_STATUS) & 1 != 0 {
        return Err("ArceOS observer has a pending event".into());
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn ax_hal_bootargs_contains(marker: &str) -> bool {
    ax_std::os::arceos::modules::ax_hal::boot::bootargs()
        .is_some_and(|bootargs| bootargs.contains(marker))
}

#[cfg(feature = "arceos")]
fn ax_hal_monotonic_time_nanos() -> u64 {
    ax_std::os::arceos::modules::ax_hal::time::monotonic_time_nanos()
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
fn write_u32(base: NonNull<u8>, offset: usize, value: u32) {
    // SAFETY: the caller provides a mapped device aperture and every constant
    // offset used here is naturally aligned and contained in that aperture.
    unsafe { core::ptr::write_volatile(base.as_ptr().add(offset).cast::<u32>(), value) }
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
