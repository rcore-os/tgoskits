//! BAR0 register page state of one ivshmem endpoint.
//!
//! The offsets follow the Jailhouse v2 base order and freeze one AxVisor
//! extension: `Event Status` lets the polling path and the later MSI-X
//! backend confirm where an event came from. All registers accept only
//! aligned 32-bit accesses; unknown offsets read as zero and ignore writes.

use super::{error::IvshmemError, link::PeerId};

/// Size of the BAR0 register page.
///
/// One full page keeps page-granular register mappings (UIO consumers) from
/// exposing neighbouring BARs; this deviates from the 256-byte QEMU BAR0 on
/// purpose.
pub const REGISTER_PAGE_SIZE: u64 = 0x1000;

/// Offset of the read-only peer ID register.
pub const ID_OFFSET: u64 = 0x00;
/// Offset of the read-only maximum-peer-count register.
pub const MAXIMUM_PEERS_OFFSET: u64 = 0x04;
/// Offset of the read/write notification-enable register; only bit 0 is
/// implemented, other bits read as zero and ignore writes.
pub const INTERRUPT_CONTROL_OFFSET: u64 = 0x08;
/// Offset of the write-only doorbell register (`target << 16 | vector`).
pub const DOORBELL_OFFSET: u64 = 0x0c;
/// Offset of the read/write endpoint state register.
pub const STATE_OFFSET: u64 = 0x10;
/// Offset of the write-one-to-clear event status register.
pub const EVENT_STATUS_OFFSET: u64 = 0x14;

const INTERRUPT_CONTROL_ENABLE: u32 = 1;

/// Mutable BAR0 register state of one endpoint.
///
/// ID and Maximum Peers come from the link identity and are passed to
/// [`read`](Self::read) so this type stays pure mutable state.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IvshmemRegisters {
    interrupt_control: u32,
    state: u32,
    event_status: u32,
}

impl IvshmemRegisters {
    /// Creates power-on registers with notification disabled and no pending
    /// event.
    pub const fn new() -> Self {
        Self {
            interrupt_control: 0,
            state: 0,
            event_status: 0,
        }
    }

    /// Clears locally mutable registers without touching the shared backing.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Returns whether interrupt notification is enabled for this endpoint.
    pub const fn notifications_enabled(&self) -> bool {
        self.interrupt_control & INTERRUPT_CONTROL_ENABLE != 0
    }

    /// Sets the pending bit (bit 0) of the event status register.
    ///
    /// Multiple pending doorbells merge into this single bit by design;
    /// shared-memory protocol state remains the source of truth. A plain
    /// write suffices: the release-order contract requires producers to
    /// finish shared-memory writes before ringing the doorbell, so no
    /// register-level atomic is needed here.
    pub fn record_event(&mut self) {
        self.event_status |= 1;
    }

    /// Reads one aligned Dword of the BAR0 register page.
    pub fn read(&self, offset: u64, peer: PeerId, max_peers: u16) -> Result<u32, IvshmemError> {
        Self::validate_access(offset)?;
        Ok(match offset {
            ID_OFFSET => peer.value().into(),
            MAXIMUM_PEERS_OFFSET => max_peers.into(),
            INTERRUPT_CONTROL_OFFSET => self.interrupt_control,
            // The doorbell is write-only by specification.
            DOORBELL_OFFSET => 0,
            STATE_OFFSET => self.state,
            EVENT_STATUS_OFFSET => self.event_status,
            _ => 0,
        })
    }

    /// Writes one aligned Dword of the BAR0 register page.
    ///
    /// Doorbell writes never reach this method: the endpoint adapter routes
    /// them through the link before any register lock is taken, so this
    /// state machine only sees the registers it owns.
    pub fn write(&mut self, offset: u64, value: u32) -> Result<(), IvshmemError> {
        Self::validate_access(offset)?;
        match offset {
            INTERRUPT_CONTROL_OFFSET => self.interrupt_control = value & INTERRUPT_CONTROL_ENABLE,
            STATE_OFFSET => self.state = value,
            EVENT_STATUS_OFFSET => self.event_status &= !value,
            // Read-only registers and unknown offsets ignore writes.
            _ => {}
        }
        Ok(())
    }

    const fn validate_access(offset: u64) -> Result<(), IvshmemError> {
        if offset.is_multiple_of(4) && offset < REGISTER_PAGE_SIZE {
            Ok(())
        } else {
            Err(IvshmemError::InvalidRegisterAccess {
                offset,
                page_size: REGISTER_PAGE_SIZE,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::*;

    const PEER: PeerId = PeerId::new(1);
    const MAX_PEERS: u16 = 2;

    #[test]
    fn reads_identity_from_the_link_not_from_mutable_state() {
        let registers = IvshmemRegisters::new();
        assert_eq!(registers.read(ID_OFFSET, PEER, MAX_PEERS).unwrap(), 1);
        assert_eq!(
            registers
                .read(MAXIMUM_PEERS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            2
        );
        // Identity registers are read-only.
        let mut registers = registers;
        registers.write(ID_OFFSET, 0xffff).unwrap();
        registers.write(MAXIMUM_PEERS_OFFSET, 0xffff).unwrap();
        assert_eq!(registers.read(ID_OFFSET, PEER, MAX_PEERS).unwrap(), 1);
        assert_eq!(
            registers
                .read(MAXIMUM_PEERS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            2
        );
    }

    #[test]
    fn interrupt_control_keeps_only_the_enable_bit() {
        let mut registers = IvshmemRegisters::new();
        registers
            .write(INTERRUPT_CONTROL_OFFSET, 0xffff_ffff)
            .unwrap();
        assert_eq!(
            registers
                .read(INTERRUPT_CONTROL_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            INTERRUPT_CONTROL_ENABLE
        );
        registers.write(INTERRUPT_CONTROL_OFFSET, 0).unwrap();
        assert_eq!(
            registers
                .read(INTERRUPT_CONTROL_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            0
        );
    }

    #[test]
    fn state_register_stores_the_full_dword() {
        let mut registers = IvshmemRegisters::new();
        registers.write(STATE_OFFSET, 0xdead_beef).unwrap();
        assert_eq!(
            registers.read(STATE_OFFSET, PEER, MAX_PEERS).unwrap(),
            0xdead_beef
        );
    }

    #[test]
    fn doorbell_reads_zero_and_ignores_writes() {
        let mut registers = IvshmemRegisters::new();
        registers.write(DOORBELL_OFFSET, 0x0001_0002).unwrap();
        assert_eq!(registers.read(DOORBELL_OFFSET, PEER, MAX_PEERS).unwrap(), 0);
        // The ignored doorbell must not leak into neighbour registers.
        assert_eq!(registers.state, 0);
        assert_eq!(registers.interrupt_control, 0);
    }

    #[test]
    fn event_status_starts_clear_and_only_clears_on_write_one() {
        let mut registers = IvshmemRegisters::new();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            0
        );
        registers.record_event();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            1
        );
        // Writing zero does not clear the pending bit (W1C semantics).
        registers.write(EVENT_STATUS_OFFSET, 0).unwrap();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            1
        );
        registers.write(EVENT_STATUS_OFFSET, 0xffff_ffff).unwrap();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            0
        );
    }

    #[test]
    fn recorded_events_merge_into_one_pending_bit() {
        let mut registers = IvshmemRegisters::new();
        registers.record_event();
        registers.record_event();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            1
        );
        // Recording after a W1C clear pends the bit again.
        registers.write(EVENT_STATUS_OFFSET, 1).unwrap();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            0
        );
        registers.record_event();
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            1
        );
    }

    #[test]
    fn unknown_aligned_offsets_read_zero_and_ignore_writes() {
        let mut registers = IvshmemRegisters::new();
        for offset in [0x18, 0x800, REGISTER_PAGE_SIZE - 4] {
            registers.write(offset, 0xffff).unwrap();
            assert_eq!(registers.read(offset, PEER, MAX_PEERS).unwrap(), 0);
        }
    }

    #[test]
    fn rejects_unaligned_and_out_of_page_accesses() {
        let registers = IvshmemRegisters::new();
        let expected = |offset: u64| IvshmemError::InvalidRegisterAccess {
            offset,
            page_size: REGISTER_PAGE_SIZE,
        };
        for offset in [
            1,
            2,
            3,
            5,
            0x102,
            REGISTER_PAGE_SIZE,
            REGISTER_PAGE_SIZE + 4,
        ] {
            assert_eq!(
                registers.read(offset, PEER, MAX_PEERS),
                Err(expected(offset))
            );
        }
        assert_eq!(
            format!("{}", expected(2)),
            "ivshmem register access at offset 0x2 is not an aligned 32-bit offset inside the \
             0x1000-byte register page"
        );
    }

    #[test]
    fn reset_clears_local_registers_only() {
        let mut registers = IvshmemRegisters::new();
        registers.write(INTERRUPT_CONTROL_OFFSET, 1).unwrap();
        registers.write(STATE_OFFSET, 0x1234).unwrap();
        registers.record_event();
        registers.reset();
        assert_eq!(registers, IvshmemRegisters::new());
        assert_eq!(
            registers
                .read(EVENT_STATUS_OFFSET, PEER, MAX_PEERS)
                .unwrap(),
            0
        );
    }
}
