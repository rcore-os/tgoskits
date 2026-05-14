use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{cell::UnsafeCell, time::Duration};

use ::xhci::{
    ExtendedCapability,
    extended_capabilities::{List, usb_legacy_support_capability::UsbLegacySupport},
    registers::doorbell,
    ring::trb::{command, event::CommandCompletion},
};
use dma_api::DmaDirection;
use futures::{FutureExt, future::BoxFuture};
use mbarrier::mb;
use spin::RwLock;
use usb_if::err::{TransferError, USBError};

use super::{
    Device, SlotId,
    cmd::CommandRing,
    context::{DeviceContextList, ScratchpadBufferArray},
    event::{EventRing, EventRingInfo},
    hub::{PortChangeWaker, XhciRootHub},
    reg::{MemMapper, XhciRegisters},
    transfer::TransferResultHandler,
};
use crate::{
    DeviceAddressInfo, KernelOp, Mmio,
    backend::{
        kmod::{hub::HubOp, kcore::CoreOp, xhci::reg::SlotBell},
        ty::{DeviceOp, Event, EventHandlerOp},
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

    fn init<'a>(&'a mut self) -> BoxFuture<'a, core::result::Result<(), USBError>> {
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
            )),
            root_hub: Some(root_hub),
            event_ring_info,
            scratchpad_buf_arr: None,
        })
    }

    async fn _init(&mut self) -> Result {
        self.disable_irq();
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

        self.enable_irq();
        // self.reset_ports().await;

        Ok(())
    }

    async fn new_device(&mut self, info: DeviceAddressInfo) -> Result<Box<dyn DeviceOp>> {
        let mut device = Device::new(self).await?;
        device.init(self, &info).await?;

        Ok(Box::new(device))
    }

    async fn init_ext_caps(&mut self) -> Result {
        let caps = self.extended_capabilities();
        debug!("Extended capabilities: {:?}", caps.len());

        for cap in self.extended_capabilities() {
            if let ExtendedCapability::UsbLegacySupport(usb_legacy_support) = cap {
                self.legacy_init(usb_legacy_support).await?;
            }
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

    fn extended_capabilities(&self) -> Vec<ExtendedCapability<MemMapper>> {
        let hccparams1 = self.reg.read().capability.hccparams1.read_volatile();
        let mapper = MemMapper {};
        let mut out = Vec::new();
        let mut l = match unsafe { List::new(self.reg.read().mmio_base, hccparams1, mapper) } {
            Some(v) => v,
            None => return out,
        };

        for one in &mut l {
            if let Ok(cap) = one {
                out.push(cap);
            } else {
                break;
            }
        }
        out
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
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.clear_interrupter_enable();
        });
    }

    pub fn enable_irq(&mut self) {
        debug!("Enable interrupts");
        self.reg.write().operational.usbcmd.update_volatile(|r| {
            r.set_interrupter_enable();
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

            debug!("ERDP: {erdp:x}");

            ir0.erdp.update_volatile(|r| {
                r.set_event_ring_dequeue_pointer(erdp & !0xf);
                r.set_dequeue_erst_segment_index((erdp & 0x7) as u8);
                r.clear_event_handler_busy();
            });

            debug!("ERSTZ: {erstz:x}");
            ir0.erstsz.update_volatile(|r| r.set(erstz as _));
            debug!("ERSTBA: {erstba:X}");
            ir0.erstba.update_volatile(|r| {
                r.set(erstba);
            });

            ir0.imod.update_volatile(|im| {
                im.set_interrupt_moderation_interval(0x1F);
                im.set_interrupt_moderation_counter(0);
            });
        }

        {
            debug!("Enabling primary interrupter.");
            self.reg
                .write()
                .interrupter_register_set
                .interrupter_mut(0)
                .iman
                .update_volatile(|im| {
                    im.set_interrupt_enable();
                    im.clear_interrupt_pending();
                });
        }

        /* Set the HCD state before we enable the irqs */
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

            self.dev_mut()?.dcbaa.set(0, bus_addr);

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
    reg: UnsafeCell<XhciRegisters>,
    cmd_finished: Finished<CommandCompletion>,
    event_ring: UnsafeCell<EventRing>,
    transfer_result_handler: TransferResultHandler,
    ports: PortChangeWaker,
}

unsafe impl Send for EventHandler {}
unsafe impl Sync for EventHandler {}

impl EventHandler {
    fn new(
        reg: XhciRegisters,
        cmd_finished: Finished<CommandCompletion>,
        event_ring: EventRing,
        transfer_result_handler: TransferResultHandler,
        ports: PortChangeWaker,
    ) -> Self {
        Self {
            reg: UnsafeCell::new(reg),
            cmd_finished,
            event_ring: UnsafeCell::new(event_ring),
            transfer_result_handler,
            ports,
        }
    }

    #[allow(clippy::mut_from_ref)]
    fn event_ring(&self) -> &mut EventRing {
        unsafe { &mut *self.event_ring.get() }
    }

    #[allow(clippy::mut_from_ref)]
    fn reg(&self) -> &mut XhciRegisters {
        unsafe { &mut *self.reg.get() }
    }

    fn update_erdp(&self, clear_ehb: bool) {
        let erdp = self.event_ring().erdp();
        let segment_index = self.event_ring().segment_index();
        self.reg()
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

    fn clean_event_ring(&self) -> Event {
        use xhci::ring::trb::event::Allowed;
        let mut event = Event::Nothing;
        let mut command_events = 0usize;
        let mut port_events = 0usize;
        let mut transfer_events = 0usize;
        let mut other_events = 0usize;
        let mut event_loop = 0usize;

        while let Some(allowed) = self.event_ring().next() {
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
                        "xhci: event transfer slot={} ep={} ptr={:#x} code={:?} len={} event_data={}",
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
                self.update_erdp(false);
                event_loop = 0;
            }
        }
        trace!(
            "xhci: event ring drained command={} port={} transfer={} other={} erdp={:#x}",
            command_events,
            port_events,
            transfer_events,
            other_events,
            self.event_ring().erst_dequeue_pointer()
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
    fn handle_event(&self) -> Event {
        let mut res = Event::Nothing;
        let sts = self.reg().operational.usbsts.read_volatile();
        let has_event_interrupt = sts.event_interrupt();
        let has_pending_event = self.event_ring().has_pending_event();

        if !has_event_interrupt && !has_pending_event {
            return res;
        }

        {
            let irq = self.reg().interrupter_register_set.interrupter_mut(0);
            let iman = irq.iman.read_volatile();
            let erdp = irq.erdp.read_volatile();
            if has_event_interrupt {
                trace!(
                    "xhci: handle_event USBSTS.EINT=1 IMAN.IP={} IMAN.IE={} EHB={} ERDP={:#x} sw_erdp={:#x}",
                    iman.interrupt_pending(),
                    iman.interrupt_enable(),
                    erdp.event_handler_busy(),
                    erdp.event_ring_dequeue_pointer(),
                    self.event_ring().erst_dequeue_pointer()
                );
            } else {
                trace!(
                    "xhci: handle_event draining pending event with USBSTS.EINT=0 IMAN.IP={} IMAN.IE={} EHB={} ERDP={:#x} sw_erdp={:#x}",
                    iman.interrupt_pending(),
                    iman.interrupt_enable(),
                    erdp.event_handler_busy(),
                    erdp.event_ring_dequeue_pointer(),
                    self.event_ring().erst_dequeue_pointer()
                );
            }
        }

        if has_event_interrupt {
            self.reg().operational.usbsts.update_volatile(|r| {
                r.clear_event_interrupt();
            });
        }

        // 【关键】GIC 中断模式下，需要手动清除 IMAN.IP
        // 参考: Linux xhci_irq() in xhci-ring.c:3054-3059
        let mut irq = self.reg().interrupter_register_set.interrupter_mut(0);
        irq.iman.update_volatile(|r| {
            r.clear_interrupt_pending();
        });

        res = self.clean_event_ring();
        self.update_erdp(true);

        res
    }
}
