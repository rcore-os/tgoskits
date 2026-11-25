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
use arceos_ivshmem_pci::{EVENT_VECTOR_0, IvshmemDevice, IvshmemLayout, OutputSection};
#[cfg(feature = "arceos")]
use ax_std as _;
// F4 reserves the first BAR2 page for the state table; F5/F6 give every peer
// one page-aligned output section, writable only by its owner. The test
// payload must stay inside this peer's own section.
const SUITE_MAX_PEERS: u32 = 3;
const SUITE_LINUX_PEER: u32 = 0;
const SUITE_ARCEOS_PEER: u32 = 1;
const SUITE_PORTABLE_PEER: u32 = 2;
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
    let suite = ax_hal_bootargs_contains(SUITE_BOOTARGS_MARKER);
    let cross_peer = ax_hal_bootargs_contains(CROSS_PEER_BOOTARGS_MARKER);
    let layout = if suite {
        IvshmemLayout::three_peer()
    } else if cross_peer {
        IvshmemLayout::two_peer()
    } else {
        IvshmemLayout::single_peer()
    };
    let device = IvshmemDevice::discover(layout).map_err(|error| error.to_string())?;
    let peer_id = device.peer_id();
    let max_peers = device.max_peers();
    if suite && peer_id != SUITE_ARCEOS_PEER {
        return Err(format!(
            "profile peer_id={peer_id} max_peers={max_peers}, expected \
             peer_id={SUITE_ARCEOS_PEER} max_peers={SUITE_MAX_PEERS}",
        ));
    }

    let test_offset = device
        .layout()
        .output_offset(peer_id)
        .map_err(|error| error.to_string())?
        + TEST_OFFSET_IN_SECTION;

    let own_output = device.own_output().map_err(|error| error.to_string())?;
    own_output
        .write_u64(TEST_OFFSET_IN_SECTION, TEST_VALUE)
        .map_err(|error| error.to_string())?;
    let actual = own_output
        .read_u64(TEST_OFFSET_IN_SECTION)
        .map_err(|error| error.to_string())?;
    if actual != TEST_VALUE {
        return Err(format!(
            "BAR2 readback mismatch: expected {TEST_VALUE:#018x}, got {actual:#018x}"
        ));
    }

    // The BAR0 State write must surface in the shared state table (F4);
    // this peer's entry sits at BAR2 offset `peer_id * 4` in the first page.
    let bootstrap_state = if suite {
        SUITE_STATE_ARCEOS_BOOTSTRAP
    } else {
        HANDSHAKE_STATE_SELF
    };
    device
        .publish_state(bootstrap_state)
        .map_err(|error| error.to_string())?;
    let own_state = device.state(peer_id).map_err(|error| error.to_string())?;
    if own_state != bootstrap_state {
        return Err(format!(
            "BAR0 state write did not surface in the state table: {own_state:#010x}"
        ));
    }
    println!(
        "ivshmem-pci identity={} bar2={:#x} peer_id={peer_id} test_offset={test_offset:#x} \
         cross_peer={cross_peer} suite={suite}",
        device.identity(),
        device.bar2_bus_address(),
    );

    if suite {
        self_doorbell_tests(&device, peer_id)?;
        run_suite_arceos_peer(&device)?;
    } else if cross_peer {
        if peer_id == 0 {
            // Initiator: self-doorbell coverage first, then the exchange.
            self_doorbell_tests(&device, peer_id)?;
            cross_peer_exchange(&device, peer_id)?;
        } else {
            // The responder exchange consumes the request first and runs the
            // self-doorbell checks before publishing its reply. Therefore the
            // initiator's final pass marker proves both ArceOS paths completed.
            cross_peer_exchange(&device, peer_id)?;
        }
    } else {
        self_doorbell_tests(&device, peer_id)?;
    }
    Ok(())
}

/// Waits until Event Status bit 0 pends and clears it with write-1-to-clear.
///
/// Returns `true` when an event was observed, `false` on timeout.
#[cfg(feature = "arceos")]
fn wait_event(device: &IvshmemDevice, timeout_nanos: u64) -> Result<bool, String> {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(timeout_nanos);
    while ax_hal_monotonic_time_nanos() < deadline {
        if device
            .take_events(EVENT_VECTOR_0)
            .map_err(|error| error.to_string())?
            != 0
        {
            return Ok(true);
        }
        core::hint::spin_loop();
    }
    Ok(false)
}

/// Rings this endpoint's own doorbell twice and verifies the W1C/re-pend
/// behavior, then checks that an unsupported vector produces no event.
#[cfg(feature = "arceos")]
fn self_doorbell_tests(device: &IvshmemDevice, peer_id: u32) -> Result<(), String> {
    device
        .ring_doorbell(peer_id, 0)
        .map_err(|error| error.to_string())?;
    if !wait_event(device, SELF_DOORBELL_TIMEOUT_NANOS)? {
        return Err("first self doorbell timed out".into());
    }
    if device.event_status().map_err(|error| error.to_string())? & EVENT_VECTOR_0 != 0 {
        return Err("event status was not cleared by the wait".into());
    }

    device
        .ring_doorbell(peer_id, 0)
        .map_err(|error| error.to_string())?;
    if !wait_event(device, SELF_DOORBELL_TIMEOUT_NANOS)? {
        return Err("second self doorbell timed out".into());
    }

    // Vector 1 is outside the current profile: the doorbell is a no-op and
    // no event may arrive.
    device
        .ring_doorbell(peer_id, 1)
        .map_err(|error| error.to_string())?;
    if wait_event(device, SELF_DOORBELL_UNAVAILABLE_WAIT_NANOS)? {
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
fn cross_peer_exchange(device: &IvshmemDevice, peer_id: u32) -> Result<(), String> {
    let target = peer_id ^ 1;
    let own_mailbox = device.own_output().map_err(|error| error.to_string())?;
    let remote_mailbox = device.output(target).map_err(|error| error.to_string())?;

    if peer_id == 0 {
        fill_mailbox(&own_mailbox, 0x5a)?;
        device
            .publish_state(HANDSHAKE_STATE_INITIATOR)
            .map_err(|error| error.to_string())?;
        // `ring_doorbell` fences the preceding Normal-memory mailbox and state
        // publications before the Device register write.
        device
            .ring_doorbell(target, 0)
            .map_err(|error| error.to_string())?;
        println!("ivshmem-pci checkpoint cross-peer-request");

        if !wait_event(device, HANDSHAKE_TIMEOUT_NANOS)? {
            return Err("cross-peer reply event timed out".into());
        }
        wait_remote_state(device, target, HANDSHAKE_STATE_RESPONDER)?;
        validate_mailbox(&remote_mailbox)?;
        println!("ivshmem-pci checkpoint cross-peer-reply");
    } else {
        // Publish readiness only after the event path and shared mappings are
        // usable, matching the Linux responder protocol.
        device
            .publish_state(HANDSHAKE_STATE_READY)
            .map_err(|error| error.to_string())?;
        println!("ivshmem-pci checkpoint cross-peer-ready");

        if !wait_event(device, HANDSHAKE_TIMEOUT_NANOS)? {
            return Err("cross-peer request event timed out".into());
        }
        wait_remote_state(device, target, HANDSHAKE_STATE_INITIATOR)?;
        validate_mailbox(&remote_mailbox)?;

        // Event Status is clear after consuming the request, so self events
        // cannot merge with and hide that request. Complete all local event
        // checks before replying; the Linux initiator only passes after this
        // peer reaches the reply doorbell below.
        self_doorbell_tests(device, peer_id)?;
        fill_mailbox(&own_mailbox, 0xa5)?;
        device
            .publish_state(HANDSHAKE_STATE_RESPONDER)
            .map_err(|error| error.to_string())?;
        device
            .ring_doorbell(target, 0)
            .map_err(|error| error.to_string())?;
        println!("ivshmem-pci checkpoint cross-peer-relay");
    }
    Ok(())
}

/// Polls the remote peer's state-table entry until it reaches `expected`.
#[cfg(feature = "arceos")]
fn wait_remote_state(device: &IvshmemDevice, peer_id: u32, expected: u32) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos() + 1_000_000_000;
    while device.state(peer_id).map_err(|error| error.to_string())? != expected {
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
fn fill_mailbox(mailbox: &OutputSection<'_>, seed: u32) -> Result<(), String> {
    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = (index as u32 * 7 + seed) as u8;
    }
    mailbox
        .write_bytes(0x10, &payload)
        .map_err(|error| error.to_string())?;
    let checksum = payload_checksum(&payload);
    mailbox
        .write_u32(0x00, MAILBOX_MAGIC)
        .map_err(|error| error.to_string())?;
    mailbox
        .write_u32(0x04, checksum)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(feature = "arceos")]
fn validate_mailbox(mailbox: &OutputSection<'_>) -> Result<(), String> {
    let magic = mailbox.read_u32(0x00).map_err(|error| error.to_string())?;
    let checksum = mailbox.read_u32(0x04).map_err(|error| error.to_string())?;
    if magic != MAILBOX_MAGIC {
        return Err(format!("cross-peer mailbox magic mismatch: {magic:#010x}"));
    }
    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    mailbox
        .read_bytes(0x10, &mut payload)
        .map_err(|error| error.to_string())?;
    if checksum != payload_checksum(&payload) {
        return Err("cross-peer payload checksum mismatch".into());
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn run_suite_arceos_peer(device: &IvshmemDevice) -> Result<(), String> {
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=observer step=ready");
    device
        .publish_state(SUITE_STATE_ARCEOS_OBSERVER_ARMED)
        .map_err(|error| error.to_string())?;
    ensure_no_event_until_state(device, SUITE_PORTABLE_PEER, SUITE_STATE_PORTABLE_RECEIVED)?;
    device
        .publish_state(SUITE_STATE_ARCEOS_OBSERVER_CLEAN)
        .map_err(|error| error.to_string())?;
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=observer step=clean");

    wait_suite_state(
        device,
        SUITE_PORTABLE_PEER,
        SUITE_STATE_PORTABLE_OBSERVER_ARMED,
    )?;
    wait_suite_state(device, SUITE_LINUX_PEER, SUITE_STATE_LINUX_IRQ_READY)?;

    let own_mailbox = suite_mailbox(device, SUITE_ARCEOS_PEER)?;
    fill_suite_mailbox(
        &own_mailbox,
        SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
        SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ,
        SUITE_ARCEOS_PEER,
        SUITE_LINUX_PEER,
        ARCEOS_TO_LINUX_REQUEST,
    )?;
    log_suite_mailbox("tx", &own_mailbox, "doorbell")?;
    device
        .publish_state(SUITE_STATE_ARCEOS_REQUEST)
        .map_err(|error| error.to_string())?;
    device
        .ring_doorbell(SUITE_LINUX_PEER, 0)
        .map_err(|error| error.to_string())?;
    println!("IVSHMEM_PCI_SUITE_CHECKPOINT peer=1 role=arceos phase=interrupt-request step=sent");

    if !wait_event(device, HANDSHAKE_TIMEOUT_NANOS)? {
        return Err("suite interrupt reply timed out".into());
    }
    wait_suite_state(device, SUITE_LINUX_PEER, SUITE_STATE_LINUX_IRQ_REPLIED)?;
    let linux_mailbox = suite_mailbox(device, SUITE_LINUX_PEER)?;
    validate_suite_mailbox(
        &linux_mailbox,
        SUITE_PHASE_ARCEOS_TO_LINUX_IRQ,
        SUITE_SEQUENCE_ARCEOS_TO_LINUX_IRQ,
        SUITE_LINUX_PEER,
        SUITE_ARCEOS_PEER,
        LINUX_TO_ARCEOS_REPLY,
    )?;
    log_suite_mailbox("rx", &linux_mailbox, "polling")?;
    device
        .publish_state(SUITE_STATE_ARCEOS_DONE)
        .map_err(|error| error.to_string())?;
    println!("IVSHMEM_PCI_SUITE_ROLE_PASSED peer=1 role=arceos");
    Ok(())
}

#[cfg(feature = "arceos")]
fn suite_mailbox(device: &IvshmemDevice, peer_id: u32) -> Result<OutputSection<'_>, String> {
    device.output(peer_id).map_err(|error| error.to_string())
}

#[cfg(feature = "arceos")]
fn fill_suite_mailbox(
    mailbox: &OutputSection<'_>,
    phase: u32,
    sequence: u32,
    source_peer: u32,
    target_peer: u32,
    message: &[u8],
) -> Result<(), String> {
    if message.is_empty() || message.len() > MAILBOX_PAYLOAD_SIZE {
        return Err("outgoing suite message length is invalid".into());
    }

    mailbox
        .write_u32(0x00, 0)
        .map_err(|error| error.to_string())?;
    mailbox
        .write_bytes(0x20, &[0; MAILBOX_PAYLOAD_SIZE])
        .map_err(|error| error.to_string())?;
    mailbox
        .write_bytes(0x20, message)
        .map_err(|error| error.to_string())?;
    for (offset, value) in [
        (0x04, SUITE_PROTOCOL_VERSION),
        (0x08, phase),
        (0x0c, sequence),
        (0x10, source_peer),
        (0x14, target_peer),
        (0x18, payload_checksum(message)),
        (0x1c, message.len() as u32),
    ] {
        mailbox
            .write_u32(offset, value)
            .map_err(|error| error.to_string())?;
    }
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    mailbox
        .write_u32(0x00, SUITE_MAILBOX_MAGIC)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(feature = "arceos")]
fn validate_suite_mailbox(
    mailbox: &OutputSection<'_>,
    phase: u32,
    sequence: u32,
    source_peer: u32,
    target_peer: u32,
    expected_message: &[u8],
) -> Result<(), String> {
    if mailbox.read_u32(0x00).map_err(|error| error.to_string())? != SUITE_MAILBOX_MAGIC {
        return Err("suite mailbox magic mismatch".into());
    }
    if mailbox.read_u32(0x04).map_err(|error| error.to_string())? != SUITE_PROTOCOL_VERSION {
        return Err("suite mailbox protocol version mismatch".into());
    }
    if mailbox.read_u32(0x08).map_err(|error| error.to_string())? != phase
        || mailbox.read_u32(0x0c).map_err(|error| error.to_string())? != sequence
    {
        return Err("suite mailbox phase or sequence mismatch".into());
    }
    if mailbox.read_u32(0x10).map_err(|error| error.to_string())? != source_peer
        || mailbox.read_u32(0x14).map_err(|error| error.to_string())? != target_peer
    {
        return Err("suite mailbox route mismatch".into());
    }
    let payload_size = usize::try_from(mailbox.read_u32(0x1c).map_err(|error| error.to_string())?)
        .map_err(|_| "suite mailbox payload size does not fit usize")?;
    if payload_size != expected_message.len() || payload_size > MAILBOX_PAYLOAD_SIZE {
        return Err("suite mailbox payload size mismatch".into());
    }

    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    mailbox
        .read_bytes(0x20, &mut payload[..payload_size])
        .map_err(|error| error.to_string())?;
    let message = &payload[..payload_size];
    if mailbox.read_u32(0x18).map_err(|error| error.to_string())? != payload_checksum(message) {
        return Err("suite mailbox payload checksum mismatch".into());
    }
    if message != expected_message {
        return Err("suite mailbox message content mismatch".into());
    }
    Ok(())
}

#[cfg(feature = "arceos")]
fn log_suite_mailbox(action: &str, mailbox: &OutputSection<'_>, path: &str) -> Result<(), String> {
    let payload_size = usize::try_from(mailbox.read_u32(0x1c).map_err(|error| error.to_string())?)
        .map_err(|_| "suite mailbox payload size does not fit usize")?;
    if payload_size > MAILBOX_PAYLOAD_SIZE {
        return Err("suite mailbox payload is too large to log".into());
    }

    let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
    mailbox
        .read_bytes(0x20, &mut payload[..payload_size])
        .map_err(|error| error.to_string())?;
    let content = core::str::from_utf8(&payload[..payload_size])
        .map_err(|_| "suite mailbox message is not UTF-8")?;
    println!(
        "IVSHMEM_PCI_SUITE_MESSAGE action={action} phase={} sequence={} source={} target={} \
         path={path} content=\"{content}\"",
        mailbox.read_u32(0x08).map_err(|error| error.to_string())?,
        mailbox.read_u32(0x0c).map_err(|error| error.to_string())?,
        mailbox.read_u32(0x10).map_err(|error| error.to_string())?,
        mailbox.read_u32(0x14).map_err(|error| error.to_string())?,
    );
    Ok(())
}

#[cfg(feature = "arceos")]
fn wait_suite_state(device: &IvshmemDevice, peer_id: u32, expected: u32) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(HANDSHAKE_TIMEOUT_NANOS);

    while device.state(peer_id).map_err(|error| error.to_string())? != expected {
        if ax_hal_monotonic_time_nanos() >= deadline {
            let actual = device.state(peer_id).map_err(|error| error.to_string())?;
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
    device: &IvshmemDevice,
    peer_id: u32,
    expected: u32,
) -> Result<(), String> {
    let deadline = ax_hal_monotonic_time_nanos().saturating_add(HANDSHAKE_TIMEOUT_NANOS);

    // The portable peer may advance from RECEIVED to REPLIED before this
    // observer samples it, so this phase deliberately accepts later states.
    while device.state(peer_id).map_err(|error| error.to_string())? < expected {
        if device.event_status().map_err(|error| error.to_string())? & EVENT_VECTOR_0 != 0 {
            return Err("portable-directed doorbell reached ArceOS".into());
        }
        if ax_hal_monotonic_time_nanos() >= deadline {
            return Err("portable peer did not receive the directed doorbell".into());
        }
        core::hint::spin_loop();
    }
    if device.event_status().map_err(|error| error.to_string())? & EVENT_VECTOR_0 != 0 {
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
