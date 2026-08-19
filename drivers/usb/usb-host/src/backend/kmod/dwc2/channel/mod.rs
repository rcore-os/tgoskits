pub mod iso;
pub mod non_iso;

use alloc::{sync::Arc, vec::Vec};
use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    task::Context,
};

use ax_sync::SpinLock as Mutex;
use dma_api::{ContiguousArray, DmaDirection};
use futures::task::AtomicWaker;
use mbarrier::mb;
use tock_registers::interfaces::{Readable, Writeable};
use usb_if::{endpoint::EndpointInfo, err::TransferError, host::hub::Speed};

use super::{
    Kernel,
    reg::{DWC2_COMPLETION_DISCONNECTED, DWC2_DMA_ALIGN, DWC2_MAX_CHANNELS, Dwc2Registers},
};
use crate::backend::kmod::dwc2::{HCFG_FRLISTEN_64, HCFG_FRLISTEN_MASK, HCFG_PERSCHEDENA};

/// 端点级通道配置
pub(crate) struct ChannelConfig {
    pub(crate) device_address: u8,
    pub(crate) info: EndpointInfo,
    pub(crate) port_speed: Speed,
}

// ═══════════════════════════════════════════
// 控制器级 Host Periodic Frame List
// ═══════════════════════════════════════════

/// DWC2 Host Periodic Frame List（Linux HFLBADDR + HCFG.PERSCHEDENA）。
///
/// 64 项 u32 帧位图，bit n = 通道 n 在对应帧被周期调度。DDMA 模式下
/// 周期传输必须通过帧列表调度（Linux `dwc2_per_sched_enable` 在首个周期
/// QH 时使能），ISO 通道激活时置位、释放时清位。
pub(crate) struct Dwc2PeriodicSchedule {
    inner: Arc<Dwc2PeriodicScheduleInner>,
}

struct Dwc2PeriodicScheduleInner {
    data: ContiguousArray<u8>,
    dma_addr: u32,
    enabled: AtomicBool,
    gate: Mutex<()>,
}

impl Dwc2PeriodicSchedule {
    pub(crate) fn new(kernel: &Kernel) -> Result<Self, anyhow::Error> {
        let data = kernel
            .contiguous_array_zero_with_align::<u8>(64 * 4, DWC2_DMA_ALIGN, DmaDirection::ToDevice)
            .map_err(|err| anyhow!("DWC2 frame list alloc failed: {err}"))?;
        let dma_addr = u32::try_from(data.dma_addr().as_u64())
            .map_err(|_| anyhow!("DWC2 frame list DMA above 32-bit mask"))?;
        Ok(Self {
            inner: Arc::new(Dwc2PeriodicScheduleInner {
                data,
                dma_addr,
                enabled: AtomicBool::new(false),
                gate: Mutex::new(()),
            }),
        })
    }

    /// 首次 ISO 通道激活时使能周期调度并装载帧列表（幂等）。
    pub(crate) fn ensure_enabled(&self, regs: Dwc2Registers) {
        let _guard = self.inner.gate.lock();
        if self.inner.enabled.swap(true, Ordering::AcqRel) {
            return;
        }
        regs.regs().hflbaddr.set(self.inner.dma_addr);
        let hcfg = regs.regs().hcfg.get();
        regs.regs()
            .hcfg
            .set((hcfg & !HCFG_FRLISTEN_MASK) | HCFG_FRLISTEN_64 | HCFG_PERSCHEDENA);
        log::debug!(
            "dwc2: periodic schedule enabled (frame list @ {:#010x})",
            self.inner.dma_addr
        );
    }

    /// 在帧位图中置位通道的每个服务帧。
    pub(crate) fn set_channel(&self, channel: u8, frames: &[usize]) {
        let _guard = self.inner.gate.lock();
        let ptr = self.inner.data.as_ptr().as_ptr() as *mut u32;
        for &frame in frames {
            let index = frame & 63;
            let value = unsafe { ptr.add(index).read_volatile() };
            unsafe { ptr.add(index).write_volatile(value | (1u32 << channel)) };
        }
        self.inner.data.sync_for_device_all();
        mb();
    }

    /// 清掉通道在帧位图中的所有位（释放通道时调用）。
    pub(crate) fn clear_channel(&self, channel: u8) {
        let _guard = self.inner.gate.lock();
        let ptr = self.inner.data.as_ptr().as_ptr() as *mut u32;
        for index in 0..64 {
            let value = unsafe { ptr.add(index).read_volatile() };
            unsafe { ptr.add(index).write_volatile(value & !(1u32 << channel)) };
        }
        self.inner.data.sync_for_device_all();
        mb();
    }
}

struct Dwc2ChannelCompletionSlot {
    hcint: AtomicU32,
    deferred_hcint: AtomicU32,
    busy: AtomicBool,
    iso: AtomicBool,
    waker: AtomicWaker,
}

impl Dwc2ChannelCompletionSlot {
    fn new() -> Self {
        Self {
            hcint: AtomicU32::new(0),
            deferred_hcint: AtomicU32::new(0),
            busy: AtomicBool::new(false),
            iso: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        }
    }

    fn try_begin_request(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn end_request(&self) {
        self.busy.store(false, Ordering::Release);
        self.iso.store(false, Ordering::Release);
        self.clear();
    }

    fn clear(&self) {
        self.hcint.store(0, Ordering::Release);
        self.deferred_hcint.store(0, Ordering::Release);
    }

    fn publish(&self, hcint: u32) {
        let deferred = self.deferred_hcint.swap(0, Ordering::AcqRel);
        self.hcint.fetch_or(hcint | deferred, Ordering::AcqRel);
        self.waker.wake();
    }

    fn defer(&self, hcint: u32) {
        self.deferred_hcint.fetch_or(hcint, Ordering::AcqRel);
    }

    fn take(&self) -> Option<u32> {
        let hcint = self.hcint.swap(0, Ordering::AcqRel);
        (hcint != 0).then_some(hcint)
    }

    fn register_waker(&self, cx: &mut Context<'_>) {
        self.waker.register(cx.waker());
    }

    fn mark_iso(&self, iso: bool) {
        self.iso.store(iso, Ordering::Release);
    }

    fn is_iso(&self) -> bool {
        self.iso.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub(crate) struct Dwc2ChannelCompletions {
    slots: Arc<Vec<Dwc2ChannelCompletionSlot>>,
    connected: Arc<AtomicBool>,
    lifecycle_gate: Arc<Mutex<()>>,
}

impl Dwc2ChannelCompletions {
    pub(crate) fn new() -> Self {
        Self {
            slots: Arc::new(
                (0..usize::from(DWC2_MAX_CHANNELS))
                    .map(|_| Dwc2ChannelCompletionSlot::new())
                    .collect(),
            ),
            connected: Arc::new(AtomicBool::new(true)),
            lifecycle_gate: Arc::new(Mutex::new(())),
        }
    }

    fn slot(&self, channel: u8) -> &Dwc2ChannelCompletionSlot {
        &self.slots[usize::from(channel)]
    }

    pub(crate) fn try_begin_request(&self, channel: u8) -> bool {
        self.slot(channel).try_begin_request()
    }

    pub(crate) fn end_request(&self, channel: u8) {
        self.slot(channel).end_request();
    }

    pub(crate) fn clear(&self, channel: u8) {
        self.slot(channel).clear();
    }

    pub(crate) fn publish(&self, channel: u8, hcint: u32) {
        self.slot(channel).publish(hcint);
    }

    pub(crate) fn defer(&self, channel: u8, hcint: u32) {
        self.slot(channel).defer(hcint);
    }

    pub(crate) fn take(&self, channel: u8) -> Option<u32> {
        self.slot(channel).take()
    }

    pub(crate) fn register_waker(&self, channel: u8, cx: &mut Context<'_>) {
        self.slot(channel).register_waker(cx);
    }

    /// 标记通道当前处于 ISO 常驻模式（IRQ 侧据此直发 XFERCOMPL 而不关通道）。
    pub(crate) fn mark_iso(&self, channel: u8, iso: bool) {
        self.slot(channel).mark_iso(iso);
    }

    pub(crate) fn is_iso(&self, channel: u8) -> bool {
        self.slot(channel).is_iso()
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) fn mark_connected(&self, configure_irqs: impl FnOnce()) {
        let _guard = self.lifecycle_gate.lock_irqsave();
        configure_irqs();
        self.connected.store(true, Ordering::Release);
    }

    pub(crate) fn disconnect_all_with(&self, mask_channel_irqs: impl FnOnce()) {
        let _guard = self.lifecycle_gate.lock_irqsave();
        // DISCONNINT is the controller's hardware confirmation that the host
        // bus transaction has ended. From this point the host-channel
        // registers may no longer be accessed, so publish terminal ownership
        // release directly instead of trying to synthesize CHHLTD or CHDIS.
        self.connected.store(false, Ordering::Release);
        mask_channel_irqs();
        for slot in self
            .slots
            .iter()
            .filter(|slot| slot.busy.load(Ordering::Acquire))
        {
            slot.publish(DWC2_COMPLETION_DISCONNECTED);
        }
    }

    pub(crate) fn with_connected<T>(
        &self,
        operation: impl FnOnce() -> core::result::Result<T, TransferError>,
    ) -> core::result::Result<T, TransferError> {
        let _guard = self.lifecycle_gate.lock_irqsave();
        if !self.is_connected() {
            return Err(TransferError::Disconnected);
        }
        operation()
    }
}

#[derive(Clone)]
pub(crate) struct HostChannelPool {
    pub(crate) channel_count: u8,
    pub(crate) completions: Dwc2ChannelCompletions,
    pub(crate) periodic: Arc<Dwc2PeriodicSchedule>,
    channel_gates: Arc<Vec<Arc<Mutex<()>>>>,
}

impl HostChannelPool {
    pub(crate) fn new(
        channel_count: u8,
        completions: Dwc2ChannelCompletions,
        periodic: Arc<Dwc2PeriodicSchedule>,
    ) -> Self {
        let channel_gates = (0..usize::from(channel_count.max(1)))
            .map(|_| Arc::new(Mutex::new(())))
            .collect();

        Self {
            channel_count,
            completions,
            periodic,
            channel_gates: Arc::new(channel_gates),
        }
    }

    /// 获取一个可用的通道
    pub(crate) fn acquire(&self, control: bool) -> Result<ChannelLease, TransferError> {
        if !self.completions.is_connected() {
            return Err(TransferError::Disconnected);
        }
        let channels = if control { 0..1 } else { 1..self.channel_count };
        for channel in channels {
            if self.completions.try_begin_request(channel) {
                self.completions.clear(channel);
                let gate = self.channel_gates[usize::from(channel)].clone();

                return Ok(ChannelLease {
                    channel,
                    gate,
                    completions: self.completions.clone(),
                    released: false,
                    hardware_active: AtomicBool::new(false),
                });
            }
        }
        Err(TransferError::QueueFull)
    }
}

// ═══════════════════════════════════════════
// 通道租约（non_iso 与 iso 状态机共用）
// ═══════════════════════════════════════════

pub(crate) struct ChannelLease {
    pub(crate) channel: u8,
    pub(crate) gate: Arc<Mutex<()>>,
    pub(crate) completions: Dwc2ChannelCompletions,
    pub(crate) hardware_active: AtomicBool,
    pub(crate) released: bool,
}

impl ChannelLease {
    pub(crate) fn release(mut self) {
        self.completions.end_request(self.channel);
        self.released = true;
    }
}

impl Drop for ChannelLease {
    fn drop(&mut self) {
        if !self.released && !self.hardware_active.load(Ordering::Acquire) {
            self.completions.end_request(self.channel);
        } else if !self.released {
            error!(
                "dwc2: quarantining host channel {} because hardware stop was not confirmed",
                self.channel
            );
        }
    }
}
