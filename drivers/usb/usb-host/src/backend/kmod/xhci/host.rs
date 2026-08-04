use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

use ::xhci::{
    extended_capabilities::usb_legacy_support_capability::UsbLegacySupport,
    registers::{
        doorbell,
        runtime::{EventRingDequeuePointerRegister, InterrupterManagementRegister},
    },
    ring::trb::{command, event::CommandCompletion},
};
use ax_kspin::{SpinRaw, SpinRawGuard, SpinRwLock as RwLock};
use dma_api::DmaDirection;
use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use usb_if::err::{TransferError, USBError};

use super::{
    Device, SlotId,
    cmd::CommandRing,
    context::{DeviceContextList, ScratchpadBufferArray},
    event::{EventRing, EventRingInfo},
    hub::{PortChangeWaker, XhciRootHub},
    reg::{MemMapper, XhciRegisters, scan_extended_capabilities},
    transfer::TransferResultHandler,
};
use crate::{
    DeviceAddressInfo, KernelOp, Mmio,
    backend::{
        kmod::{hub::HubOp, kcore::CoreOp, xhci::reg::SlotBell},
        ty::{ControllerIrqState, DeviceOp, Event, EventHandlerOp},
    },
    err::Result,
    osal::{Kernel, SpinWhile},
    queue::Finished,
};

pub struct Xhci {
    pub(crate) reg: Arc<RwLock<XhciRegisters>>,
    pub(crate) kernel: Kernel,
    pub(crate) cmd: CommandRing,
    dev_ctx: Option<DeviceContextList>,
    event_handler: Option<EventHandler>,
    event_ring_info: EventRingInfo,
    scratchpad_buf_arr: Option<ScratchpadBufferArray>,
    pub(crate) transfer_result_handler: TransferResultHandler,
    root_hub: Option<XhciRootHub>,
    irq_state: ControllerIrqState,
    irq_mask: Arc<XhciIrqMaskState>,
}

unsafe impl Send for Xhci {}
unsafe impl Sync for Xhci {}

impl CoreOp for Xhci {
    fn root_hub(&mut self) -> Box<dyn HubOp> {
        Box::new(
            self.root_hub
                .take()
                .expect("Root hub can only be taken once"),
        )
    }

    fn prepare_controller<'a>(&'a mut self) -> BoxFuture<'a, core::result::Result<(), USBError>> {
        self._init().boxed()
    }

    fn new_addressed_device<'a>(
        &'a mut self,
        addr: DeviceAddressInfo,
    ) -> BoxFuture<'a, Result<Box<dyn DeviceOp>>> {
        self.new_device(addr).boxed()
    }

    fn create_event_handler(&mut self) -> Box<dyn EventHandlerOp> {
        Box::new(
            self.event_handler
                .take()
                .expect("Event handler can only be created once"),
        )
    }

    fn enable_irq(&mut self) -> Result<()> {
        Self::enable_irq(self);
        Ok(())
    }

    fn disable_irq(&mut self) -> Result<()> {
        Self::disable_irq(self);
        Ok(())
    }

    fn kernel(&self) -> &Kernel {
        &self.kernel
    }
}

impl Xhci {
    pub fn new(mmio: Mmio, kernel: &'static dyn KernelOp) -> Result<Self> {
        let reg = XhciRegisters::new(mmio);

        // 检查 xHCI 控制器的寻址能力（HCCPARAMS1 寄存器）
        let hccparams1 = reg.capability.hccparams1.read_volatile();
        let ac64 = hccparams1.addressing_capability(); // Bit[0]: 64-bit Addressing Capability

        info!(
            "xHCI: Addressing Capability (AC64) = {} ({}-bit addressing)",
            ac64,
            if ac64 { "64" } else { "32" }
        );

        // 根据 AC64 位调整 DMA mask
        let dma_mask = if ac64 {
            u64::MAX as usize
        } else {
            // 控制器只支持 32 位地址，强制限制在 32 位
            u32::MAX as usize
        };

        let kernel = Kernel::new(dma_mask as _, kernel);

        let reg_shared = Arc::new(RwLock::new(reg.clone()));

        let cmd = CommandRing::new(DmaDirection::Bidirectional, &kernel, reg_shared.clone())?;
        let cmd_finished = cmd.finished_handle();
        let max_event_ring_segments = reg
            .capability
            .hcsparams2
            .read_volatile()
            .event_ring_segment_table_max() as usize;
        let event_ring = EventRing::new(max_event_ring_segments, &kernel)?;
        let event_ring_info = event_ring.info();

        let root_hub = XhciRootHub::new(reg.clone())?;

        let transfer_result_handler = TransferResultHandler::new(reg_shared.clone());
        let ports = root_hub.waker();
        let irq_state = ControllerIrqState::new(false);
        let irq_mask = Arc::new(XhciIrqMaskState::masked());

        Ok(Xhci {
            reg: reg_shared,
            kernel,
            cmd,
            dev_ctx: None,
            transfer_result_handler: transfer_result_handler.clone(),
            event_handler: Some(EventHandler::new(
                reg,
                cmd_finished,
                event_ring,
                transfer_result_handler,
                ports,
                irq_state.clone(),
                irq_mask.clone(),
            )),
            root_hub: Some(root_hub),
            event_ring_info,
            scratchpad_buf_arr: None,
            irq_state,
            irq_mask,
        })
    }

    async fn _init(&mut self) -> Result {
        // Runtime interrupter registers are not writable until reset completes
        // and CNR clears, so mask only the operational global gate here.
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.clear_interrupter_enable();
        });
        // 4.2 Host Controller Initialization
        self.init_ext_caps().await?;
        // After Chip Hardware Reset6 wait until the Controller Not Ready (CNR) flag
        // in the USBSTS is ‘0’ before writing any xHC Operational or Runtime
        // registers.
        self.chip_hardware_reset().await?;

        self.disable_irq();

        // Program the Max Device Slots Enabled (MaxSlotsEn) field in the CONFIG
        // register (5.4.7) to enable the device slots that system software is going to
        // use.
        let max_slots = self.setup_max_device_slots();
        self.dev_ctx = Some(DeviceContextList::new(max_slots as _, self.kernel())?);

        // Program the Device Context Base Address Array Pointer (DCBAAP)
        // register (5.4.6) with a 64-bit address pointing to where the Device
        // Context Base Address Array is located.
        self.setup_dcbaap()?;

        // Define the Command Ring Dequeue Pointer by programming the
        // Command Ring Control Register (5.4.5) with a 64-bit address pointing to
        // the starting address of the first TRB of the Command Ring.
        self.set_cmd_ring()?;
        self.init_irq()?;
        self.setup_scratchpads()?;
        // At this point, the host controller is up and running and the Root Hub ports
        // (5.4.8) will begin reporting device connects, etc., and system software may begin
        // enumerating devices. System software may follow the procedures described in
        // section 4.3, to enumerate attached devices.
        self.start();
        mb();

        self.wait_for_running().await;

        Ok(())
    }

    async fn new_device(&mut self, info: DeviceAddressInfo) -> Result<Box<dyn DeviceOp>> {
        let mut device = Device::new(self).await?;
        device.init(self, &info).await?;

        Ok(Box::new(device))
    }

    async fn init_ext_caps(&mut self) -> Result {
        let (mmio_base, hccparams1) = {
            let reg = self.reg.read();
            (reg.mmio_base, reg.capability.hccparams1.read_volatile())
        };
        let caps = scan_extended_capabilities(mmio_base, hccparams1, MemMapper {});
        debug!("Extended capabilities: {:?}", caps.count);

        if let Some(usb_legacy_support) = caps.usb_legacy_support {
            self.legacy_init(usb_legacy_support).await?;
        }

        Ok(())
    }

    async fn chip_hardware_reset(&mut self) -> Result {
        debug!("Reset begin ...");
        self.reg.write().operational.usbcmd.update_volatile(|c| {
            c.clear_run_stop();
        });

        SpinWhile::new(|| {
            !self
                .reg
                .read()
                .operational
                .usbsts
                .read_volatile()
                .hc_halted()
        })
        .await;

        debug!("Halted");
        debug!("Wait for ready...");

        SpinWhile::new(|| {
            self.reg
                .read()
                .operational
                .usbsts
                .read_volatile()
                .controller_not_ready()
        })
        .await;

        debug!("Ready");

        self.reg.write().operational.usbcmd.update_volatile(|f| {
            f.set_host_controller_reset();
        });

        debug!("Reset HC");

        SpinWhile::new(|| {
            self.reg
                .read()
                .operational
                .usbcmd
                .read_volatile()
                .host_controller_reset()
                || self
                    .reg
                    .read()
                    .operational
                    .usbsts
                    .read_volatile()
                    .controller_not_ready()
        })
        .await;

        debug!("Reset finish");

        Ok(())
    }

    async fn legacy_init(&mut self, mut usb_legacy_support: UsbLegacySupport<MemMapper>) -> Result {
        debug!("legacy init");
        usb_legacy_support.usblegsup.update_volatile(|r| {
            r.set_hc_os_owned_semaphore();
        });

        loop {
            let up = usb_legacy_support.usblegsup.read_volatile();
            if up.hc_os_owned_semaphore() && !up.hc_bios_owned_semaphore() {
                break;
            }
        }

        debug!("claimed ownership from BIOS");

        usb_legacy_support.usblegctlsts.update_volatile(|r| {
            r.clear_usb_smi_enable();
            r.clear_smi_on_host_system_error_enable();
            r.clear_smi_on_os_ownership_enable();
            r.clear_smi_on_pci_command_enable();
            r.clear_smi_on_bar_enable();

            r.clear_smi_on_bar();
            r.clear_smi_on_pci_command();
            r.clear_smi_on_os_ownership_change();
        });

        Ok(())
    }

    fn setup_max_device_slots(&mut self) -> u8 {
        let mut regs = self.reg.write();
        let max_slots = regs
            .capability
            .hcsparams1
            .read_volatile()
            .number_of_device_slots();

        regs.operational.config.update_volatile(|r| {
            r.set_max_device_slots_enabled(max_slots);
        });

        debug!("Max device slots: {max_slots}");

        max_slots
    }

    pub(crate) fn dev(&self) -> Result<&DeviceContextList> {
        self.dev_ctx.as_ref().ok_or(USBError::NotInitialized)
    }

    pub(crate) fn dev_mut(&mut self) -> Result<&mut DeviceContextList> {
        self.dev_ctx.as_mut().ok_or(USBError::NotInitialized)
    }

    pub fn disable_irq(&mut self) {
        debug!("Disable interrupts");
        self.irq_state.set_enabled(false, || {
            self.irq_mask.begin_masking();
            let mut regs = self.reg.write();
            let mut primary = regs.interrupter_register_set.interrupter_mut(0);
            primary
                .iman
                .update_volatile(|register| prepare_iman_rearm(register, false));
            let _flushed = primary.iman.read_volatile();
            self.irq_mask.finish_masking();

            regs.operational.usbcmd.update_volatile(|r| {
                r.clear_interrupter_enable();
            });
        });
    }

    pub fn enable_irq(&mut self) {
        debug!("Enable interrupts");
        self.irq_state.set_enabled(true, || {
            let mut regs = self.reg.write();
            regs.operational.usbcmd.update_volatile(|r| {
                r.set_interrupter_enable();
            });
            mb();

            if !self.irq_mask.begin_rearm() {
                return;
            }
            let mut primary = regs.interrupter_register_set.interrupter_mut(0);
            primary
                .iman
                .update_volatile(|register| prepare_iman_rearm(register, true));
            let _flushed = primary.iman.read_volatile();
            if self.irq_mask.finish_rearm() == RearmCompletion::Remask {
                primary
                    .iman
                    .update_volatile(|register| prepare_iman_rearm(register, false));
                let _flushed = primary.iman.read_volatile();
            }
        });
    }

    fn setup_dcbaap(&mut self) -> Result {
        let dcbaa_addr = self.dev()?.dcbaa.dma_addr();
        debug!("DCBAAP: {dcbaa_addr}");
        self.reg.write().operational.dcbaap.update_volatile(|r| {
            r.set(dcbaa_addr.as_u64());
        });
        Ok(())
    }

    fn set_cmd_ring(&mut self) -> Result {
        let crcr = self.cmd.bus_addr();
        let cycle = self.cmd.cycle();

        debug!("CRCR: {crcr:?}");
        self.reg.write().operational.crcr.update_volatile(|r| {
            r.set_command_ring_pointer(crcr.into());
            if cycle {
                r.set_ring_cycle_state();
            } else {
                r.clear_ring_cycle_state();
            }
        });

        Ok(())
    }

    fn init_irq(&mut self) -> Result {
        let erstz = self.event_ring_info.erstz;
        let erdp = self.event_ring_info.erdp;
        let erstba = self.event_ring_info.erstba;

        {
            let mut reg = self.reg.write();
            let mut ir0 = reg.interrupter_register_set.interrupter_mut(0);

            debug!("ERSTZ: {erstz:x}");
            ir0.erstsz.update_volatile(|r| r.set(erstz as _));
            debug!("ERSTBA: {erstba:X}");
            ir0.erstba.update_volatile(|r| {
                r.set(erstba);
            });

            debug!("ERDP: {erdp:x}");
            ir0.erdp
                .update_volatile(|register| prepare_initial_erdp(register, erdp));

            ir0.imod.update_volatile(|im| {
                im.set_interrupt_moderation_interval(0x1F);
                im.set_interrupt_moderation_counter(0);
            });
        }

        {
            debug!("Masking primary interrupter during controller prepare.");
            self.reg
                .write()
                .interrupter_register_set
                .interrupter_mut(0)
                .iman
                .update_volatile(prepare_iman_initialization);
        }

        // Set the HCD state before we enable the irqs
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.set_host_system_error_enable();
            r.set_enable_wrap_event();
        });
        Ok(())
    }

    fn setup_scratchpads(&mut self) -> Result {
        let scratchpad_buf_arr = {
            let buf_count = {
                let count = self
                    .reg
                    .read()
                    .capability
                    .hcsparams2
                    .read_volatile()
                    .max_scratchpad_buffers();
                debug!("Scratch buf count: {count}");
                count
            };
            if buf_count == 0 {
                return Ok(());
            }
            let scratchpad_buf_arr = ScratchpadBufferArray::new(buf_count as _, &self.kernel)?;

            let bus_addr = scratchpad_buf_arr.bus_addr();

            self.dev_mut()?.dcbaa.set_cpu(0, bus_addr);

            debug!("Setting up {buf_count} scratchpads, at {bus_addr:#0x}");
            scratchpad_buf_arr
        };

        self.scratchpad_buf_arr = Some(scratchpad_buf_arr);

        Ok(())
    }

    fn start(&mut self) {
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.set_run_stop();
        });
        debug!("Start run");
    }

    async fn wait_for_running(&mut self) {
        SpinWhile::new(|| {
            let sts = self.reg.read().operational.usbsts.read_volatile();
            sts.hc_halted() || sts.controller_not_ready()
        })
        .await;

        info!("Running");

        // 必须等待至少200ms，否则 port enable = false
        self.kernel.delay(Duration::from_millis(200));

        self.reg
            .write()
            .doorbell
            .write_volatile_at(0, doorbell::Register::default());
    }

    pub(crate) fn cmd_request(
        &mut self,
        trb: command::Allowed,
    ) -> impl Future<Output = core::result::Result<CommandCompletion, TransferError>> {
        self.cmd.cmd_request(trb)
    }

    pub(crate) fn is_64bit_ctx(&self) -> bool {
        self.reg
            .read()
            .capability
            .hccparams1
            .read_volatile()
            .context_size()
    }

    pub(crate) fn new_slot_bell(&self, slot: SlotId) -> SlotBell {
        SlotBell::new(slot, self.reg.read().clone())
    }

    pub(crate) async fn device_slot_assignment(
        &mut self,
    ) -> core::result::Result<SlotId, TransferError> {
        // enable slot
        let result = self
            .cmd_request(command::Allowed::EnableSlot(command::EnableSlot::default()))
            .await?;

        let slot_id = result.slot_id();
        trace!("assigned slot id: {slot_id}");
        Ok(slot_id.into())
    }
}

pub struct EventHandler {
    event_reg: UnsafeCell<XhciRegisters>,
    irq_ack_reg: SpinRaw<XhciRegisters>,
    irq_rearm_reg: UnsafeCell<XhciRegisters>,
    cmd_finished: Finished<CommandCompletion>,
    event_ring: UnsafeCell<EventRing>,
    transfer_result_handler: TransferResultHandler,
    ports: PortChangeWaker,
    irq_state: ControllerIrqState,
    irq_mask: Arc<XhciIrqMaskState>,
    task_gate: SpinRaw<()>,
    event_gate: SpinRaw<()>,
}

// SAFETY: `task_gate` serializes every task-context entry. `event_gate`
// additionally protects the event register view and event ring, while the
// rearm register view is used only under `task_gate`. Hard IRQ owns the
// independently mapped acknowledgement view behind `irq_ack_reg`; it touches
// only USBSTS and IMAN. `irq_mask` orders the one shared IMAN hardware field
// across acknowledgement and rearm.
unsafe impl Send for EventHandler {}
unsafe impl Sync for EventHandler {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum XhciIrqMaskPhase {
    Unmasked = 0,
    Masking  = 1,
    Masked   = 2,
    Rearming = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RearmCompletion {
    Unmasked,
    Remask,
}

struct XhciIrqMaskState {
    phase: AtomicU8,
}

fn prepare_iman_rearm(register: &mut InterrupterManagementRegister, enabled: bool) {
    // IP is RW1C. Rearm/disable must write zero so an event that arrived while
    // the source was masked remains pending and is delivered after IE opens.
    register.set_0_interrupt_pending();
    if enabled {
        register.set_interrupt_enable();
    } else {
        register.clear_interrupt_enable();
    }
}

fn prepare_iman_initialization(register: &mut InterrupterManagementRegister) {
    register.set_0_interrupt_pending();
    register.clear_interrupt_enable();
}

fn prepare_initial_erdp(register: &mut EventRingDequeuePointerRegister, erdp: u64) {
    register.set_event_ring_dequeue_pointer(erdp & !0xf);
    register.set_dequeue_erst_segment_index((erdp & 0x7) as u8);
    // EHB is RW1C. Writing zero keeps a pending handler-busy indication.
    register.set_0_event_handler_busy();
}

impl XhciIrqMaskState {
    #[cfg(test)]
    const fn unmasked() -> Self {
        Self {
            phase: AtomicU8::new(XhciIrqMaskPhase::Unmasked as u8),
        }
    }

    const fn masked() -> Self {
        Self {
            phase: AtomicU8::new(XhciIrqMaskPhase::Masked as u8),
        }
    }

    fn begin_masking(&self) {
        self.phase
            .store(XhciIrqMaskPhase::Masking as u8, Ordering::Release);
    }

    fn finish_masking(&self) {
        self.phase
            .store(XhciIrqMaskPhase::Masked as u8, Ordering::Release);
    }

    fn begin_rearm(&self) -> bool {
        self.phase
            .compare_exchange(
                XhciIrqMaskPhase::Masked as u8,
                XhciIrqMaskPhase::Rearming as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn cancel_rearm(&self) {
        let _result = self.phase.compare_exchange(
            XhciIrqMaskPhase::Rearming as u8,
            XhciIrqMaskPhase::Masked as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn finish_rearm(&self) -> RearmCompletion {
        if self
            .phase
            .compare_exchange(
                XhciIrqMaskPhase::Rearming as u8,
                XhciIrqMaskPhase::Unmasked as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            RearmCompletion::Unmasked
        } else {
            RearmCompletion::Remask
        }
    }

    #[cfg(test)]
    fn phase(&self) -> XhciIrqMaskPhase {
        match self.phase.load(Ordering::Acquire) {
            value if value == XhciIrqMaskPhase::Unmasked as u8 => XhciIrqMaskPhase::Unmasked,
            value if value == XhciIrqMaskPhase::Masking as u8 => XhciIrqMaskPhase::Masking,
            value if value == XhciIrqMaskPhase::Masked as u8 => XhciIrqMaskPhase::Masked,
            value if value == XhciIrqMaskPhase::Rearming as u8 => XhciIrqMaskPhase::Rearming,
            _ => unreachable!("invalid xHCI IRQ mask phase"),
        }
    }
}

impl EventHandler {
    fn new(
        reg: XhciRegisters,
        cmd_finished: Finished<CommandCompletion>,
        event_ring: EventRing,
        transfer_result_handler: TransferResultHandler,
        ports: PortChangeWaker,
        irq_state: ControllerIrqState,
        irq_mask: Arc<XhciIrqMaskState>,
    ) -> Self {
        Self {
            event_reg: UnsafeCell::new(reg.clone()),
            irq_ack_reg: SpinRaw::new(reg.clone()),
            irq_rearm_reg: UnsafeCell::new(reg),
            cmd_finished,
            event_ring: UnsafeCell::new(event_ring),
            transfer_result_handler,
            ports,
            irq_state,
            irq_mask,
            task_gate: SpinRaw::new(()),
            event_gate: SpinRaw::new(()),
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn event_ring(&self, _guard: &SpinRawGuard<'_, ()>) -> &mut EventRing {
        // SAFETY: the private entry points can obtain this guard only from
        // `register_gate`, which serializes every event-ring access.
        unsafe { &mut *self.event_ring.get() }
    }

    fn update_erdp(&self, guard: &SpinRawGuard<'_, ()>, clear_ehb: bool) {
        let erdp = self.event_ring(guard).erdp();
        let segment_index = self.event_ring(guard).segment_index();
        // SAFETY: `guard` proves that `event_gate` serializes this register
        // view. It accesses only ERDP, disjoint from the hard-IRQ USBSTS/IMAN
        // endpoint.
        unsafe { &mut *self.event_reg.get() }
            .interrupter_register_set
            .interrupter_mut(0)
            .erdp
            .update_volatile(|r| {
                r.set_event_ring_dequeue_pointer(erdp);
                r.set_dequeue_erst_segment_index(segment_index);
                if clear_ehb {
                    r.clear_event_handler_busy();
                } else {
                    r.set_0_event_handler_busy();
                }
            });
    }

    fn clean_event_ring(&self, guard: &SpinRawGuard<'_, ()>) -> Event {
        use xhci::ring::trb::event::Allowed;
        let mut event = Event::Nothing;
        let mut command_events = 0usize;
        let mut port_events = 0usize;
        let mut transfer_events = 0usize;
        let mut other_events = 0usize;
        let mut event_loop = 0usize;

        while let Some(allowed) = self.event_ring(guard).next() {
            match allowed {
                Allowed::CommandCompletion(c) => {
                    command_events += 1;
                    let addr = c.command_trb_pointer();
                    trace!(
                        "xhci: event command ptr={:#x} slot={} code={:?}",
                        addr,
                        c.slot_id(),
                        c.completion_code()
                    );
                    self.cmd_finished.set_finished(addr.into(), c);
                }
                Allowed::PortStatusChange(st) => {
                    port_events += 1;
                    let port_id = st.port_id();
                    trace!("xhci: event port status change port={}", port_id);
                    self.ports.set_port_changed(port_id);

                    event = Event::PortChange {
                        port: st.port_id() as _,
                    };
                }
                Allowed::TransferEvent(c) => {
                    transfer_events += 1;
                    let slot_id = c.slot_id();
                    let ep_id = c.endpoint_id();
                    let ptr = c.trb_pointer();
                    trace!(
                        "xhci: event transfer slot={} ep={} ptr={:#x} code={:?} len={} \
                         event_data={}",
                        slot_id,
                        ep_id,
                        ptr,
                        c.completion_code(),
                        c.trb_transfer_length(),
                        c.event_data()
                    );

                    // Interrupts synchronize queue state only. Do not call
                    // into OS glue or take manager/file/device locks here; the
                    // waiter that owns the queue will advance the transfer flow.
                    unsafe {
                        self.transfer_result_handler
                            .set_finished(slot_id, ep_id, ptr.into(), c)
                    };
                }
                _ => {
                    other_events += 1;
                    trace!("xhci: event other {:?}", allowed);
                }
            }
            event_loop += 1;
            if event_loop > super::ring::TRBS_PER_SEGMENT / 2 {
                self.update_erdp(guard, false);
                event_loop = 0;
            }
        }
        trace!(
            "xhci: event ring drained command={} port={} transfer={} other={} erdp={:#x}",
            command_events,
            port_events,
            transfer_events,
            other_events,
            self.event_ring(guard).erst_dequeue_pointer()
        );
        if matches!(event, Event::Nothing) && transfer_events > 0 {
            event = Event::TransferActivity {
                count: transfer_events,
            };
        }
        event
    }
}

impl EventHandlerOp for EventHandler {
    fn acknowledge_irq(&self) -> bool {
        let Some(mut irq_reg) = self.irq_ack_reg.try_lock() else {
            // Only another acknowledgement can own this endpoint. That owner
            // will mask and acknowledge the same level-triggered source.
            return false;
        };
        let sts = irq_reg.operational.usbsts.read_volatile();
        let has_event_interrupt = sts.event_interrupt();

        if !has_event_interrupt {
            return false;
        }

        self.irq_mask.begin_masking();
        irq_reg.operational.usbsts.update_volatile(|r| {
            r.clear_event_interrupt();
        });

        // GIC level delivery requires clearing IMAN.IP explicitly, matching
        // Linux xhci_irq() after USBSTS.EINT is acknowledged.
        let mut irq = irq_reg.interrupter_register_set.interrupter_mut(0);
        irq.iman.update_volatile(|r| {
            r.clear_interrupt_enable();
            r.clear_interrupt_pending();
        });
        // Match Linux xhci_disable_interrupter(): IMAN may be posted MMIO, so
        // do not publish Masked until the write has reached the controller.
        let _flushed = irq.iman.read_volatile();
        self.irq_mask.finish_masking();
        true
    }

    fn drain_event(&self) -> Event {
        let _task_guard = self.task_gate.lock();
        let event_guard = self.event_gate.lock();
        let event = if self.event_ring(&event_guard).has_pending_event() {
            self.clean_event_ring(&event_guard)
        } else {
            Event::Nothing
        };
        self.update_erdp(&event_guard, true);
        event
    }

    fn rearm_irq(&self) {
        let _task_guard = self.task_gate.lock();
        if !self.irq_mask.begin_rearm() {
            return;
        }
        self.irq_state.apply_enabled(|enabled| {
            mb();
            // SAFETY: `_task_guard` serializes rearm calls. IMAN sharing with
            // hard IRQ is governed by `irq_mask`, and each side owns a
            // distinct accessor.
            let mut irq = unsafe { &mut *self.irq_rearm_reg.get() }
                .interrupter_register_set
                .interrupter_mut(0);
            irq.iman
                .update_volatile(|register| prepare_iman_rearm(register, enabled));
            // Match Linux xhci_enable_interrupter()/disable_interrupter().
            // The state transition must not outrun a posted IMAN write.
            let _flushed = irq.iman.read_volatile();

            if !enabled {
                self.irq_mask.cancel_rearm();
            } else if self.irq_mask.finish_rearm() == RearmCompletion::Remask {
                // A new hard acknowledgement won while IMAN was being
                // enabled. Reconcile the hardware with its Masking/Masked
                // publication before releasing task ownership.
                irq.iman
                    .update_volatile(|register| prepare_iman_rearm(register, false));
                let _flushed = irq.iman.read_volatile();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acknowledgement_during_rearm_requires_a_final_hardware_mask() {
        let state = XhciIrqMaskState::masked();
        assert!(state.begin_rearm());

        state.begin_masking();
        state.finish_masking();

        assert_eq!(state.finish_rearm(), RearmCompletion::Remask);
        assert_eq!(state.phase(), XhciIrqMaskPhase::Masked);
    }

    #[test]
    fn stale_rearm_cannot_enter_while_acknowledgement_is_masking() {
        let state = XhciIrqMaskState::unmasked();
        state.begin_masking();

        assert!(!state.begin_rearm());
        state.finish_masking();
        assert!(state.begin_rearm());
    }

    #[test]
    fn rearm_write_preserves_a_new_hardware_pending_bit() {
        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let mut register = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(1) };

        prepare_iman_rearm(&mut register, true);

        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let write_value =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(register) };
        assert_eq!(
            write_value & 1,
            0,
            "RW1C IP must be written as zero so rearm cannot consume a new event"
        );
    }

    #[test]
    fn initial_interrupter_setup_keeps_source_masked() {
        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let mut register = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(0) };

        prepare_iman_initialization(&mut register);

        // SAFETY: InterrupterManagementRegister is repr(transparent) over u32.
        let write_value =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(register) };
        assert_eq!(write_value & 0b10, 0, "prepare must leave IMAN.IE masked");
    }

    #[test]
    fn initial_interrupter_setup_preserves_pending_and_ehb() {
        // SAFETY: Both xHCI register wrappers are repr(transparent) over their
        // corresponding integer register widths.
        let mut iman = unsafe { core::mem::transmute::<u32, InterrupterManagementRegister>(1) };
        let mut erdp =
            unsafe { core::mem::transmute::<u64, EventRingDequeuePointerRegister>(1 << 3) };

        prepare_iman_initialization(&mut iman);
        prepare_initial_erdp(&mut erdp, 0x1000);

        // SAFETY: See the representation argument above.
        let iman_write =
            unsafe { core::mem::transmute::<InterrupterManagementRegister, u32>(iman) };
        let erdp_write =
            unsafe { core::mem::transmute::<EventRingDequeuePointerRegister, u64>(erdp) };
        assert_eq!(iman_write & 1, 0, "prepare must not acknowledge IMAN.IP");
        assert_eq!(erdp_write & (1 << 3), 0, "prepare must not clear ERDP.EHB");
    }
}
