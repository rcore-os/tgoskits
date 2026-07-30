use alloc::sync::Arc;
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

use rdif_block::{ControlEvent, HardIrqHandler, IrqAck, IrqQueueMask};

pub(super) const IRQ_SOURCE_ID: usize = 0;
pub(super) const QUEUE_ID: usize = 0;
pub(super) const CONTROL_EVENT_IRQ: u64 = 1 << 63;

const HOST_CAP: usize = 0x00;
const HOST_GHC: usize = 0x04;
const HOST_IS: usize = 0x08;
const HOST_PI: usize = 0x0c;

const GHC_HR: u32 = 1 << 0;
const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;
const CAP_S64A: u32 = 1 << 31;
const CAP_SSS: u32 = 1 << 27;
const CAP_NP_MASK: u32 = 0x1f;

const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;
const PORT_CLB: usize = 0x00;
const PORT_CLBU: usize = 0x04;
const PORT_FB: usize = 0x08;
const PORT_FBU: usize = 0x0c;
const PORT_IS: usize = 0x10;
const PORT_IE: usize = 0x14;
const PORT_CMD: usize = 0x18;
const PORT_TFD: usize = 0x20;
const PORT_SSTS: usize = 0x28;
const PORT_SERR: usize = 0x30;
const PORT_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const CMD_ICC_ACTIVE: u32 = 1 << 28;

const TFD_ERR: u32 = 1 << 0;
const TFD_DRQ: u32 = 1 << 3;
const TFD_BSY: u32 = 1 << 7;

const SSTS_DET_MASK: u32 = 0xf;
const SSTS_DET_PRESENT: u32 = 3;

pub(super) const PORT_IRQ_FATAL: u32 =
    (1 << 30) | (1 << 29) | (1 << 28) | (1 << 27) | (1 << 26) | (1 << 24) | (1 << 4);
pub(super) const PORT_IRQ_COMPLETIONS: u32 = (1 << 5) | (1 << 3) | (1 << 2) | (1 << 1) | (1 << 0);
pub(super) const PORT_IRQ_LINK: u32 = (1 << 23) | (1 << 22) | (1 << 6);
const PORT_IRQ_ENABLE: u32 = PORT_IRQ_FATAL | PORT_IRQ_COMPLETIONS | PORT_IRQ_LINK;

#[derive(Clone, Copy)]
pub(super) struct HbaRegisters {
    base: NonNull<u8>,
}

// SAFETY: This value is only a fixed MMIO capability. The controller owns
// lifecycle registers, each hctx owns its port command registers, and the IRQ
// token is restricted to status acknowledgement and source masking.
unsafe impl Send for HbaRegisters {}
unsafe impl Sync for HbaRegisters {}

impl HbaRegisters {
    /// # Safety
    ///
    /// `base` must name an exclusively managed AHCI register aperture that
    /// remains mapped until every controller, queue, and IRQ token is dropped.
    pub(super) const unsafe fn new(base: NonNull<u8>) -> Self {
        Self { base }
    }

    pub(super) fn capabilities(self) -> u32 {
        self.read(HOST_CAP)
    }

    pub(super) fn dma_mask(self) -> u64 {
        if self.read(HOST_CAP) & CAP_S64A != 0 {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        }
    }

    pub(super) fn supports_staggered_spin_up(self) -> bool {
        self.read(HOST_CAP) & CAP_SSS != 0
    }

    pub(super) fn initialize_staggered_spin_up_capability(self) -> u32 {
        self.write(HOST_CAP, self.read(HOST_CAP) | CAP_SSS);
        let _posted = self.read(HOST_CAP);
        self.capabilities()
    }

    pub(super) fn begin_reset(self) {
        self.write(HOST_GHC, self.read(HOST_GHC) | GHC_AE | GHC_HR);
    }

    pub(super) fn reset_complete(self) -> bool {
        self.read(HOST_GHC) & GHC_HR == 0
    }

    pub(super) fn enable_ahci(self) {
        self.write(HOST_GHC, (self.read(HOST_GHC) | GHC_AE) & !GHC_IE);
    }

    pub(super) fn set_interrupts_enabled(self, enabled: bool) {
        let mut ghc = self.read(HOST_GHC) | GHC_AE;
        if enabled {
            ghc |= GHC_IE;
        } else {
            ghc &= !GHC_IE;
        }
        self.write(HOST_GHC, ghc);
    }

    pub(super) fn implemented_ports(self) -> u32 {
        self.read(HOST_PI)
    }

    pub(super) fn initialize_port_map(self, fallback: u32) -> u32 {
        let implemented = self.implemented_ports();
        if implemented != 0 || fallback == 0 {
            return implemented;
        }
        self.write(HOST_PI, fallback);
        let _posted = self.read(HOST_PI);
        self.implemented_ports()
    }

    pub(super) fn max_ports(self) -> u8 {
        ((self.read(HOST_CAP) & CAP_NP_MASK) + 1) as u8
    }

    pub(super) fn port(self, index: u8) -> PortRegisters {
        PortRegisters { hba: self, index }
    }

    fn read(self, offset: usize) -> u32 {
        // SAFETY: The constructor contract keeps the MMIO aperture mapped and
        // every accessed register is naturally aligned and within AHCI BAR5.
        unsafe { core::ptr::read_volatile(self.base.as_ptr().add(offset).cast::<u32>()) }
    }

    fn write(self, offset: usize, value: u32) {
        // SAFETY: See `read`; volatile access preserves device ordering.
        unsafe { core::ptr::write_volatile(self.base.as_ptr().add(offset).cast::<u32>(), value) }
    }
}

#[derive(Clone, Copy)]
pub(super) struct PortRegisters {
    hba: HbaRegisters,
    index: u8,
}

// SAFETY: See `HbaRegisters`. Copies are capabilities for disjoint register
// roles, not aliases to normal memory.
unsafe impl Send for PortRegisters {}
unsafe impl Sync for PortRegisters {}

impl PortRegisters {
    pub(super) const fn index(self) -> u8 {
        self.index
    }

    pub(super) fn command_state(self) -> u32 {
        self.read(PORT_CMD)
    }

    pub(super) fn task_file_status(self) -> u32 {
        self.read(PORT_TFD)
    }

    pub(super) fn sata_status(self) -> u32 {
        self.read(PORT_SSTS)
    }

    pub(super) fn sata_error(self) -> u32 {
        self.read(PORT_SERR)
    }

    pub(super) fn command_issue(self) -> u32 {
        self.read(PORT_CI)
    }

    pub(super) fn link_present(self) -> bool {
        self.read(PORT_SSTS) & SSTS_DET_MASK == SSTS_DET_PRESENT
    }

    pub(super) fn task_file_ready(self) -> bool {
        self.read(PORT_TFD) & (TFD_BSY | TFD_DRQ) == 0
    }

    pub(super) fn task_file_error(self) -> bool {
        self.read(PORT_TFD) & TFD_ERR != 0
    }

    pub(super) fn stop_command_engine(self) {
        self.write(PORT_CMD, self.read(PORT_CMD) & !CMD_ST);
        let _posted = self.read(PORT_CMD);
    }

    pub(super) fn command_engine_stopped(self) -> bool {
        self.read(PORT_CMD) & CMD_CR == 0
    }

    pub(super) fn stop_fis_receive(self) {
        self.write(PORT_CMD, self.read(PORT_CMD) & !CMD_FRE);
        let _posted = self.read(PORT_CMD);
    }

    pub(super) fn fis_receive_stopped(self) -> bool {
        self.read(PORT_CMD) & CMD_FR == 0
    }

    pub(super) fn engine_stopped(self) -> bool {
        self.command_engine_stopped() && self.fis_receive_stopped()
    }

    pub(super) fn program_dma_bases(self, command_list: u64, received_fis: u64) {
        self.write(PORT_CLB, command_list as u32);
        self.write(PORT_CLBU, (command_list >> 32) as u32);
        self.write(PORT_FB, received_fis as u32);
        self.write(PORT_FBU, (received_fis >> 32) as u32);
    }

    pub(super) fn power_up(self, staggered_spin_up: bool) {
        let mut command = (self.read(PORT_CMD) & !(0xf << 28)) | CMD_ICC_ACTIVE;
        if staggered_spin_up {
            command |= CMD_SUD;
        }
        self.write(PORT_CMD, command);
        let _posted = self.read(PORT_CMD);
    }

    pub(super) fn start_fis_receive(self) {
        self.write(PORT_CMD, self.read(PORT_CMD) | CMD_FRE);
        let _posted = self.read(PORT_CMD);
    }

    pub(super) fn start_command_engine(self) {
        self.write(PORT_CMD, self.read(PORT_CMD) | CMD_ST);
        let _posted = self.read(PORT_CMD);
    }

    pub(super) fn clear_stale_status(self) {
        self.write(PORT_SERR, u32::MAX);
        self.write(PORT_IS, u32::MAX);
        self.hba.write(HOST_IS, 1_u32 << self.index);
    }

    pub(super) fn set_interrupts_enabled(self, enabled: bool) {
        self.write(PORT_IE, if enabled { PORT_IRQ_ENABLE } else { 0 });
    }

    pub(super) fn slot_zero_active(self) -> bool {
        self.read(PORT_CI) & 1 != 0
    }

    pub(super) fn issue_slot_zero(self) {
        self.write(PORT_CI, 1);
    }

    fn pending_interrupts(self) -> u32 {
        self.read(PORT_IS)
    }

    fn acknowledge_interrupts(self, status: u32) {
        self.write(PORT_IS, status);
        self.hba.write(HOST_IS, 1_u32 << self.index);
    }

    fn host_interrupt_pending(self) -> bool {
        self.hba.read(HOST_IS) & (1_u32 << self.index) != 0
    }

    fn read(self, offset: usize) -> u32 {
        self.hba
            .read(PORT_BASE + usize::from(self.index) * PORT_STRIDE + offset)
    }

    fn write(self, offset: usize, value: u32) {
        self.hba.write(
            PORT_BASE + usize::from(self.index) * PORT_STRIDE + offset,
            value,
        );
    }
}

pub(super) struct AhciIrqHandler {
    port: PortRegisters,
    status_latch: Arc<AtomicU32>,
}

impl AhciIrqHandler {
    pub(super) fn new(port: PortRegisters, status_latch: Arc<AtomicU32>) -> Self {
        Self { port, status_latch }
    }
}

impl HardIrqHandler for AhciIrqHandler {
    fn ack(&mut self) -> IrqAck {
        if !self.port.host_interrupt_pending() {
            return IrqAck::spurious(IRQ_SOURCE_ID);
        }

        let status = self.port.pending_interrupts();
        if status == 0 {
            self.port.hba.write(HOST_IS, 1_u32 << self.port.index);
            return IrqAck::spurious(IRQ_SOURCE_ID);
        }
        let relevant = status & PORT_IRQ_ENABLE;
        if relevant == 0 {
            self.port.acknowledge_interrupts(status);
            return IrqAck::spurious(IRQ_SOURCE_ID);
        }

        // Mask before W1C acknowledgement. Any edge arriving while the hctx is
        // draining remains latched in PxIS and asserts after task-context
        // rearm, closing the check-to-sleep race without touching queue state.
        self.port.set_interrupts_enabled(false);
        self.port.acknowledge_interrupts(status);
        self.status_latch.fetch_or(relevant, Ordering::Release);
        IrqAck::masked_needs_rearm(
            IrqQueueMask::from_queue(QUEUE_ID),
            ControlEvent::new(IRQ_SOURCE_ID, CONTROL_EVENT_IRQ | u64::from(relevant)),
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use rdif_block::IrqDisposition;

    use super::*;

    fn registers(words: &mut [u32]) -> HbaRegisters {
        let base = NonNull::new(words.as_mut_ptr().cast::<u8>()).unwrap();
        // SAFETY: The test allocation covers the complete register offsets
        // used by port zero for the duration of each handler call.
        unsafe { HbaRegisters::new(base) }
    }

    #[test]
    fn hard_irq_only_masks_and_acknowledges_latched_status() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        let port = hba.port(0);
        let status_latch = Arc::new(AtomicU32::new(0));
        mmio[HOST_IS / 4] = 1;
        mmio[(PORT_BASE + PORT_IE) / 4] = PORT_IRQ_ENABLE;
        mmio[(PORT_BASE + PORT_IS) / 4] = 1;

        let ack = AhciIrqHandler::new(port, Arc::clone(&status_latch)).ack();

        assert_eq!(ack.disposition(), IrqDisposition::MaskedNeedsRearm);
        assert!(ack.queues().contains(QUEUE_ID));
        assert_ne!(ack.control_event().bits(), 0);
        assert_eq!(mmio[(PORT_BASE + PORT_IE) / 4], 0);
        // A normal-memory fake observes the exact W1C value written.
        assert_eq!(mmio[(PORT_BASE + PORT_IS) / 4], 1);
        assert_eq!(mmio[HOST_IS / 4], 1);
        assert_eq!(status_latch.load(Ordering::Acquire), 1);
    }

    #[test]
    fn hard_irq_rejects_other_shared_sources_without_touching_port() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        mmio[(PORT_BASE + PORT_IE) / 4] = PORT_IRQ_ENABLE;

        let ack = AhciIrqHandler::new(hba.port(0), Arc::new(AtomicU32::new(0))).ack();

        assert!(ack.is_spurious());
        assert_eq!(mmio[(PORT_BASE + PORT_IE) / 4], PORT_IRQ_ENABLE);
    }

    #[test]
    fn hard_irq_acks_unenabled_port_status_without_waking_the_queue() {
        const UNENABLED_STATUS: u32 = 1 << 7;

        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        let status_latch = Arc::new(AtomicU32::new(0));
        mmio[HOST_IS / 4] = 1;
        mmio[(PORT_BASE + PORT_IE) / 4] = PORT_IRQ_ENABLE;
        mmio[(PORT_BASE + PORT_IS) / 4] = UNENABLED_STATUS;

        let ack = AhciIrqHandler::new(hba.port(0), Arc::clone(&status_latch)).ack();

        assert!(ack.is_spurious());
        assert_eq!(mmio[(PORT_BASE + PORT_IE) / 4], PORT_IRQ_ENABLE);
        assert_eq!(mmio[(PORT_BASE + PORT_IS) / 4], UNENABLED_STATUS);
        assert_eq!(mmio[HOST_IS / 4], 1);
        assert_eq!(status_latch.load(Ordering::Acquire), 0);
    }

    #[test]
    fn engine_stop_orders_command_before_fis_receive() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        let port = hba.port(0);
        mmio[(PORT_BASE + PORT_CMD) / 4] = CMD_ST | CMD_FRE | CMD_CR | CMD_FR;

        port.stop_command_engine();
        assert_eq!(mmio[(PORT_BASE + PORT_CMD) / 4], CMD_FRE | CMD_CR | CMD_FR);
        assert!(!port.command_engine_stopped());

        mmio[(PORT_BASE + PORT_CMD) / 4] &= !CMD_CR;
        assert!(port.command_engine_stopped());
        port.stop_fis_receive();
        assert_eq!(mmio[(PORT_BASE + PORT_CMD) / 4], CMD_FR);
        assert!(!port.fis_receive_stopped());

        mmio[(PORT_BASE + PORT_CMD) / 4] &= !CMD_FR;
        assert!(port.engine_stopped());
    }

    #[test]
    fn platform_port_map_fallback_only_repairs_an_empty_pi_register() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);

        assert_eq!(hba.initialize_port_map(1), 1);
        assert_eq!(mmio[HOST_PI / 4], 1);

        mmio[HOST_PI / 4] = 1 << 3;
        assert_eq!(hba.initialize_port_map(1), 1 << 3);
        assert_eq!(mmio[HOST_PI / 4], 1 << 3);
    }

    #[test]
    fn platform_spin_up_quirk_preserves_existing_capabilities() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        mmio[HOST_CAP / 4] = CAP_S64A | 3;

        assert_eq!(
            hba.initialize_staggered_spin_up_capability(),
            CAP_S64A | CAP_SSS | 3
        );
    }

    #[test]
    fn rearm_preserves_completion_latched_before_handler_installation() {
        let mut mmio = vec![0_u32; (PORT_BASE + PORT_STRIDE) / 4];
        let hba = registers(&mut mmio);
        let port = hba.port(0);
        mmio[HOST_IS / 4] = 1;
        mmio[(PORT_BASE + PORT_IS) / 4] = 1;

        port.set_interrupts_enabled(true);
        hba.set_interrupts_enabled(true);

        assert_eq!(mmio[HOST_IS / 4], 1);
        assert_eq!(mmio[(PORT_BASE + PORT_IS) / 4], 1);
        assert_eq!(mmio[(PORT_BASE + PORT_IE) / 4], PORT_IRQ_ENABLE);
    }
}
