//! ISO 传输状态机

use alloc::{collections::VecDeque, vec::Vec};
use core::{sync::atomic::Ordering, task::Context};

use mbarrier::mb;
use usb_if::{
    endpoint::{IsoPacketResult, RequestId, TransferCompletion, TransferRequest, TransferStatus},
    err::TransferError,
    host::hub::Speed,
    transfer::Direction,
};

use crate::backend::kmod::{
    Kernel,
    dwc2::{
        Dwc2EpType, Dwc2Pid, Dwc2TransferFault,
        channel::{ChannelConfig, ChannelLease, HostChannelPool},
        dma::{DmaDescriptor, DmaDescriptors, Dwc2DmaBuffer, Dwc2DmaBufferPool},
        dma_addr32, endpoint_number, fault_to_transfer_error, hcchar, hcint_fault, hctsiz_ddma,
        reg::{
            DWC2_COMPLETION_DISCONNECTED, DWC2_DMA_ALIGN, Dwc2Registers, HCINT_AHBERR,
            HCINT_BBLERR, HCINT_CHHLTD, HCINT_FRMOVRN, HCINT_XACTERR, HCINT_XFERCOMPL,
        },
        stats::Dwc2Stats,
    },
};

// ═══════════════════════════════════════════
// ISO 周期性调度计算（Linux hcd_ddma.c/hcd_queue.c 语义）
// ═══════════════════════════════════════════

/// ISO 描述符环容量（Linux MAX_DMA_DESC_NUM_*）：HS 每帧 8 个微帧槽位，
/// 256 项 = 32 帧；FS 每帧 1 个槽位，64 项 = 64 帧。
const DWC2_ISO_RING_HS: usize = 256;
const DWC2_ISO_RING_FS: usize = 64;

/// 每个 ISO 包的最大传输字节（Linux MAX_ISOC_XFER_SIZE_*）。
const DWC2_ISO_MAX_XFER_HS: usize = 3072; // mult(3) × 1024
const DWC2_ISO_MAX_XFER_FS: usize = 1023;

/// ISO 描述符环容量：HS 每帧 8 个微帧槽位（256 = 32 帧），FS 每帧 1 槽位（64 帧）。
fn iso_ring_size(speed: Speed) -> usize {
    match speed {
        Speed::High => DWC2_ISO_RING_HS,
        _ => DWC2_ISO_RING_FS,
    }
}

/// 每个 ISO 包的最大传输字节（Linux MAX_ISOC_XFER_SIZE_*）。
fn iso_max_xfer_size(speed: Speed) -> usize {
    match speed {
        Speed::High => DWC2_ISO_MAX_XFER_HS,
        _ => DWC2_ISO_MAX_XFER_FS,
    }
}

/// HCTSIZ.SCHINFO：HS 为每帧内被服务的微帧位图（bit n = 微帧 n），
/// 非 HS 为 0xff（每帧）。Linux `dwc2_update_frame_list` 同款计算。
fn iso_schinfo(speed: Speed, interval: u32) -> u32 {
    if !matches!(speed, Speed::High) {
        return 0xff;
    }
    let interval = interval.max(1);
    let inc = 8_u32.div_ceil(interval);
    let mut schinfo = 0u32;
    let mut bit = 1u32;
    for _ in 0..inc {
        schinfo |= bit;
        bit = bit.wrapping_shl(interval.min(31));
    }
    schinfo & 0xff
}

/// ISO 的 HCTSIZ.PID/MC 编码（Linux `dwc2_set_pid_isoc`）：
/// HS IN mult1→DATA0、mult2→DATA1、mult3→DATA2；HS OUT mult1→DATA0 其余 MDATA；
/// FS/LS 恒 DATA0。
fn iso_pid(speed: Speed, direction: Direction, mult: u8) -> Dwc2Pid {
    match speed {
        Speed::High => match direction {
            Direction::In => match mult.max(1) {
                1 => Dwc2Pid::Data0,
                2 => Dwc2Pid::Data1,
                _ => Dwc2Pid::Data2,
            },
            Direction::Out => {
                if mult.max(1) <= 1 {
                    Dwc2Pid::Data0
                } else {
                    Dwc2Pid::MData
                }
            }
        },
        _ => Dwc2Pid::Data0,
    }
}

/// 帧号 → 描述符环起始索引（Linux `dwc2_frame_to_desc_idx`）：
/// HS 每帧 8 个微帧槽位、起始对齐微帧 0；FS 每帧 1 槽位。
fn frame_to_desc_idx(speed: Speed, full_frame: u32) -> usize {
    match speed {
        Speed::High => ((full_frame & 0x1f) as usize) * 8,
        _ => (full_frame & 0x3f) as usize,
    }
}

/// 计算 ISO 起始整帧号：HS 按当前微帧位置跳过 1–2 个整帧，FS 跳过 2 帧。
fn iso_starting_frame(raw_frame: u32, speed: Speed) -> u32 {
    match speed {
        Speed::High => {
            let microframe = raw_frame & 0x7;
            let skip = if microframe >= 5 { 2 } else { 1 };
            raw_frame.wrapping_add(skip * 8) >> 3
        }
        _ => raw_frame.wrapping_add(2) & 0xffff,
    }
}

/// 最大公约数（调度网格步长与环容量反压共用；b 为 0 时回退 1）。
fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

/// 沿调度网格把写游标推进到 `>= target` 的下一个合法索引（模 ring）。
/// 通道被服务的时刻为 `T0 + m·interval`，对应描述符索引需落在
/// `T0 mod gcd(interval, ring)` 的同余类；因此以 `gcd(interval, ring)`
/// 为步长对齐（interval 整除 ring 时等价于保持 mod interval 同余）。
fn advance_along_grid(td_last: usize, target: usize, interval: usize, ring: usize) -> usize {
    let step = gcd(interval.max(1), ring.max(1));
    let delta = (td_last as isize - target as isize).rem_euclid(step as isize) as usize;
    (target + delta) % ring
}

/// 单请求允许的最大包数：desc 索引以 interval 步进、模 ring 回绕，碰撞
/// 当且仅当 `(k2 − k1) · interval ≡ 0 (mod ring)`。最大包数 =
/// `ring / gcd(interval, ring) − 1`（Linux 会话模型允许填满，但一次性
/// 请求模型下满环会与首包碰撞）。
fn iso_max_packets(interval: u32, speed: Speed) -> usize {
    let ring = iso_ring_size(speed) as u32;
    let interval = interval.max(1);
    let gcd = gcd(interval as usize, ring as usize) as u32;
    (ring / gcd).saturating_sub(1) as usize
}

/// 通道在 FrameList 中被调度的帧索引（Linux `dwc2_update_frame_list` 的
/// 帧步进）：HS 每 `ceil(interval/8)` 帧一次（interval 为微帧数），
/// 非 HS 每 `interval` 帧一次（interval 为帧数）。返回 64 项视野内的
/// 全部服务帧索引（覆盖整个回绕周期）。
fn iso_service_frames(start_full_frame: u32, interval_slots: u32, speed: Speed) -> Vec<usize> {
    let inc = match speed {
        Speed::High => (interval_slots.max(1).div_ceil(8)) as usize,
        _ => interval_slots.max(1) as usize,
    };
    let start = (start_full_frame & 0xffff) as usize & 63;
    let mut out = Vec::new();
    let mut frame = start;
    loop {
        out.push(frame);
        frame = (frame + inc) & 63;
        if frame == start {
            break;
        }
    }
    out
}

/// 常驻通道的 ISO 调度参数（首次 submit 时从 ChannelConfig 计算一次）。
struct IsoChannelSchedule {
    direction: Direction,
    pid: Dwc2Pid,
    schinfo: u32,
    hcchar: u32,
    ring_size: usize,
}

/// 单个包的计划与实际结算结果。
struct IsoPacketPlan {
    planned: usize,
    desc_index: usize,
    actual: usize,
    status: TransferStatus,
}

/// 已在描述符环中编程、按提交顺序等待结算的 ISO 请求。
struct IsoActiveRequest {
    id: RequestId,
    transfer: Dwc2DmaBuffer,
    packets: Vec<IsoPacketPlan>,
}

/// 会话级中止原因：cancel 或通道故障。置位后等待 CHHLTD 再整会话结算。
#[derive(Clone, Copy)]
enum IsoHaltReason {
    Cancelled,
    Fault(Dwc2TransferFault, u32),
}

impl IsoHaltReason {
    fn error(self) -> TransferError {
        match self {
            IsoHaltReason::Cancelled => TransferError::Cancelled,
            IsoHaltReason::Fault(fault, hcint) => fault_to_transfer_error(fault, hcint),
        }
    }
}

/// ISO 传输状态机：持有一条常驻通道、一个描述符环与一个在飞请求队列。
pub(crate) struct IsoChannelState {
    regs: Dwc2Registers,
    kernel: Kernel,
    stats: Dwc2Stats,
    dma_pool: Dwc2DmaBufferPool,
    pool: HostChannelPool,
    next_request_id: u64,
    schedule: Option<IsoChannelSchedule>,
    lease: Option<ChannelLease>,
    ring: Option<DmaDescriptors>,
    td_last: usize,
    start_full_frame: u32,
    pending: VecDeque<IsoActiveRequest>,
    completed: VecDeque<(
        RequestId,
        core::result::Result<TransferCompletion, TransferError>,
    )>,
    fatal: Option<IsoHaltReason>,
}

impl Drop for IsoChannelState {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.as_ref()
            && lease.hardware_active.load(Ordering::Acquire)
        {
            error!(
                "dwc2: leaking ISO channel {} because hardware still references it",
                lease.channel
            );
            // 硬件仍可能引用该通道寄存器/描述符，直接释放会触发租约 Drop 的
            // quarantine 或释放仍被 DMA 使用的内存；按 non_iso 同类做法遗忘。
            if let Some(lease) = self.lease.take() {
                core::mem::forget(lease);
            }
            if let Some(ring) = self.ring.take() {
                core::mem::forget(ring);
            }
            while let Some(active) = self.pending.pop_front() {
                core::mem::forget(active);
            }
        }
    }
}

impl IsoChannelState {
    pub(crate) fn new(
        regs: Dwc2Registers,
        kernel: Kernel,
        stats: Dwc2Stats,
        pool: HostChannelPool,
    ) -> Self {
        Self {
            regs,
            kernel,
            stats,
            dma_pool: Dwc2DmaBufferPool::default(),
            pool,
            next_request_id: 1,
            schedule: None,
            lease: None,
            ring: None,
            td_last: 0,
            start_full_frame: 0,
            pending: VecDeque::new(),
            completed: VecDeque::new(),
            fatal: None,
        }
    }

    /// 在飞或已完成的请求 id（端点回收/配置切换前停稳用）。
    pub(crate) fn in_flight_request_id(&self) -> Option<RequestId> {
        self.pending
            .front()
            .map(|request| request.id)
            .or_else(|| self.completed.front().map(|(id, _)| *id))
    }

    fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        RequestId::new(id)
    }

    fn channel(&self) -> u8 {
        self.lease
            .as_ref()
            .expect("ISO channel must be leased before register access")
            .channel
    }

    /// 在生命周期 IRQ-save 门（防本地 DWC2 事件重入）与通道租约
    /// （排除任务侧寄存器改写）内执行寄存器操作。
    fn with_lease_guard<T>(
        &self,
        operation: impl FnOnce(u8) -> core::result::Result<T, TransferError>,
    ) -> core::result::Result<T, TransferError> {
        let lease = self
            .lease
            .as_ref()
            .expect("ISO channel must be leased for register access");
        lease.completions.with_connected(|| {
            // SAFETY: the lifecycle IRQ-save gate prevents local DWC2 event
            // re-entry, while the channel lease excludes task-side mutation.
            let _guard = unsafe { lease.gate.lock_raw() };
            operation(lease.channel)
        })
    }

    /// ISO 中断使能：XFERCOMPL（IOC 到达）+ CHHLTD（停稳）+ 致命故障位；
    /// IN 侧额外使能 XACTERR/BBLERR（Linux `dwc2_hc_enable_slave_ints` ISO 分支）。
    /// 注意不使能 ACK：Linux 在 IRQ 处理器里单独处理 ACK，本驱动任务侧结算
    /// 模型下 ACK 中断会造成无 XFERCOMPL 的误发布。
    fn iso_hcintmsk(direction: Direction) -> u32 {
        let mut mask = HCINT_XFERCOMPL | HCINT_CHHLTD | HCINT_FRMOVRN | HCINT_AHBERR;
        if matches!(direction, Direction::In) {
            mask |= HCINT_XACTERR | HCINT_BBLERR;
        }
        mask
    }

    /// 首次 submit 时初始化常驻通道的“非使能”部分：租借通道、分配描述符环、
    /// 计算调度参数并编程通道寄存器。返回 `true` 表示本次新建（需要随后
    /// `start_channel`）。已有通道时直接返回 `false` 复用。
    fn ensure_channel(
        &mut self,
        cfg: &ChannelConfig,
        direction: Direction,
        interval_slots: u32,
        speed: Speed,
        ring_size: usize,
    ) -> core::result::Result<bool, TransferError> {
        if self.lease.is_some() {
            return Ok(false);
        }
        let lease = self.pool.acquire(false)?;
        let ring = DmaDescriptors::new(&self.kernel, ring_size, DWC2_DMA_ALIGN).map_err(|err| {
            TransferError::Other(anyhow!("DWC2 ISO descriptor ring alloc failed: {err}"))
        })?;
        self.stats.record_dma_alloc();
        self.lease = Some(lease);
        self.ring = Some(ring);

        let mult = cfg.info.packets_per_microframe.max(1) as u8;
        self.schedule = Some(IsoChannelSchedule {
            direction,
            pid: iso_pid(speed, direction, mult),
            schinfo: iso_schinfo(speed, interval_slots),
            hcchar: hcchar(
                cfg.device_address,
                endpoint_number(cfg.info.address.raw()),
                direction,
                Dwc2EpType::Isochronous,
                cfg.info.max_packet_size.max(1),
                false,
                mult,
            ),
            ring_size,
        });

        let start_full = iso_starting_frame(self.regs.frame_number(), speed);
        self.start_full_frame = start_full;
        self.td_last = frame_to_desc_idx(speed, start_full);

        // 通道仍禁用：硬件不会取描述符，可以安全编程寄存器。IRQ-save 门
        // 防止事件处理器在寄存器写入中间重入。
        let schedule = self.schedule.as_ref().expect("schedule set above");
        let ring_addr = self.ring.as_ref().expect("ring set above").dma_addr();
        let mask = Self::iso_hcintmsk(direction);
        let hctsiz = hctsiz_ddma(schedule.pid, schedule.ring_size as u32, schedule.schinfo);
        self.with_lease_guard(|channel| {
            let ch = self.regs.channel(channel);
            ch.set_hcsplt(0);
            ch.clear_all_irqs();
            ch.set_hcintmsk(mask);
            ch.set_hctsiz(hctsiz);
            mb();
            ch.set_hcdma(ring_addr);
            mb();
            Ok(())
        })
        .inspect_err(|_| {
            // 连接已断开：回滚半初始化状态（通道从未使能，安全释放）。
            self.release_idle_channel();
        })?;
        Ok(true)
    }

    /// 装载 FrameList 并 CHENA 使能通道（首次使能；描述符已就绪后才调用，
    /// 避免硬件在首个服务帧取到零描述符）。`mark_iso` 在使能前置位，
    /// 防止早期 XFERCOMPL 被事件处理器按非 ISO 路径 defer+关闭。
    fn start_channel(
        &mut self,
        interval_slots: u32,
        speed: Speed,
    ) -> core::result::Result<(), TransferError> {
        self.pool.completions.mark_iso(self.channel(), true);
        self.pool.periodic.ensure_enabled(self.regs);
        let frames = iso_service_frames(self.start_full_frame, interval_slots, speed);
        self.pool.periodic.set_channel(self.channel(), &frames);
        let schedule = self
            .schedule
            .as_ref()
            .expect("schedule must exist when starting channel");
        self.with_lease_guard(|channel| {
            self.regs.channel(channel).enable(schedule.hcchar);
            Ok(())
        })?;
        self.lease
            .as_ref()
            .expect("lease must exist when starting channel")
            .hardware_active
            .store(true, Ordering::Release);
        log::debug!(
            "dwc2: iso channel {} enabled start_frame={} interval={} ring={}",
            self.channel(),
            self.start_full_frame,
            interval_slots,
            schedule.ring_size,
        );
        Ok(())
    }

    /// 释放常驻通道（无在飞请求时调用）：清 FrameList、标记非 ISO、清空并
    /// 释放描述符环与租约。不访问寄存器，因此断开路径也安全。
    fn release_idle_channel(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        self.pool.periodic.clear_channel(lease.channel);
        self.pool.completions.mark_iso(lease.channel, false);
        // 通道已停（或从未使能/已断开），清空环描述符（A=0）后再释放，
        // 避免硬件读到陈旧 A 位。
        if let Some(ring) = self.ring.take() {
            ring.clear_all();
        }
        self.schedule = None;
        self.td_last = 0;
        self.start_full_frame = 0;
        lease.release();
    }

    fn prepare_request(
        &mut self,
        cfg: &ChannelConfig,
        id: RequestId,
        request: TransferRequest,
        interval_slots: u32,
    ) -> core::result::Result<IsoActiveRequest, TransferError> {
        // submit 已校验请求形态（Isochronous + 非空包 + 有缓冲）。
        let direction = request.direction();
        let buffer = request
            .buffer()
            .expect("ISO request buffer must exist after submit validation");
        let packets = request.iso_packets();
        let speed = cfg.port_speed;
        let ring_size = iso_ring_size(speed);

        // 1. 包长校验：每包不得超过速度对应的最大 ISO 传输长度与描述符
        //    12 位 ISO NBYTES 上限（Linux MAX_ISOC_XFER_SIZE_* 同款语义）。
        let max_xfer = iso_max_xfer_size(speed);
        for (i, packet) in packets.iter().enumerate() {
            if packet.length > max_xfer || packet.length > DmaDescriptor::ISO_NBYTES_LIMIT as usize
            {
                return Err(TransferError::Other(anyhow!(
                    "DWC2 ISO packet {i} length {} exceeds limit {max_xfer}",
                    packet.length
                )));
            }
        }

        // 2. 分配 DMA 传输缓冲：coherent 分配 + IN 清零/OUT 拷贝调用方数据。
        //    ISO 按各包精确长度之和分配（desc 按精确长度编程，不取整）。
        self.stats.record_transfer();
        let len = packets.iter().map(|packet| packet.length).sum::<usize>();
        if buffer.len != len {
            return Err(TransferError::Other(anyhow!(
                "DWC2 ISO request buffer len {} != sum of packet lengths {len}",
                buffer.len
            )));
        }
        let transfer = Dwc2DmaBuffer::new(
            &self.kernel,
            &mut self.dma_pool,
            &self.stats,
            Some(buffer),
            direction,
            len,
        )?;
        // 3. 计算各包 DMA 地址
        let mut paddrs = Vec::with_capacity(packets.len());
        let mut offset = 0u64;
        for packet in packets {
            paddrs.push(dma_addr32(transfer.dma_addr() + offset)?);
            offset += packet.length as u64;
        }
        // 4. 初始化常驻通道（仅首次 submit）：租借通道、分配描述符环、
        //    计算调度参数并编程通道寄存器；已有通道时直接复用。
        let first_time = self.ensure_channel(cfg, direction, interval_slots, speed, ring_size)?;

        // 5. 计算插入索引：队尾请求仍未服务（其末包描述符 A 位仍置位，硬件
        //    取指至少一个 interval 之后）时紧跟环尾插入，流水线无缝衔接；
        //    否则按当前帧推进到下一个未来网格槽位，避免把描述符编程到硬件
        //    已越过的位置。
        let cur_full = iso_starting_frame(self.regs.frame_number(), speed);
        let target = frame_to_desc_idx(speed, cur_full);
        let insert = if self.pending_tail_active() {
            self.td_last
        } else {
            advance_along_grid(self.td_last, target, interval_slots as usize, ring_size)
        };

        // 6. 写入描述符环：每包一个 desc、按 interval 步进，末包置 IOC；
        //    同时构建结算用计划表（desc_index 与计划长度）。
        let ring = self
            .ring
            .as_ref()
            .expect("ring must exist after ensure_channel");
        let mut plans = Vec::with_capacity(packets.len());
        for (i, (packet, paddr)) in packets.iter().zip(paddrs).enumerate() {
            let index = (insert + i * interval_slots as usize) % ring_size;
            let last = i + 1 == packets.len();
            ring.write_descs(
                index,
                core::slice::from_ref(&DmaDescriptor::new_iso(paddr, packet.length as u32, last)),
            );
            plans.push(IsoPacketPlan {
                planned: packet.length,
                desc_index: index,
                actual: 0,
                status: TransferStatus::Completed,
            });
        }

        // 7. 首次使能通道：描述符已写入环后才 CHENA，避免硬件在首个服务
        //    帧取到零描述符。
        if first_time && let Err(err) = self.start_channel(interval_slots, speed) {
            // 使能失败（如连接断开）：尽力停通道后释放，避免留下永不启动的
            // 半初始化常驻通道。
            let _ = self.halt_channel();
            self.release_idle_channel();
            return Err(err);
        }

        // 8. 推进环尾游标（下次续插位置），记录统计与调试日志。
        self.stats.record_stage();
        self.td_last = (insert + packets.len() * interval_slots as usize) % ring_size;
        log::debug!(
            "dwc2: iso submit ch={} id={} packets={} insert={} td_last={} queued={}",
            self.channel(),
            id.raw(),
            packets.len(),
            insert,
            self.td_last,
            self.pending.len() + 1,
        );

        Ok(IsoActiveRequest {
            id,
            transfer,
            packets: plans,
        })
    }

    fn halt_channel(&self) -> core::result::Result<(), TransferError> {
        if self.lease.is_none() {
            return Ok(());
        }
        self.with_lease_guard(|channel| {
            self.regs.channel(channel).disable();
            Ok(())
        })
    }

    /// 结算一个完成请求：读回各包描述符、计算实际长度与错误状态，清空本请求
    /// 描述符（A=0）以防环回绕时被重复服务。通道保持使能，FrameList 不清。
    fn settle_request(
        &self,
        active: &mut IsoActiveRequest,
    ) -> core::result::Result<TransferCompletion, TransferError> {
        let ring = self.ring.as_ref().expect("ISO ring must exist");
        let schedule = self.schedule.as_ref().expect("ISO schedule must exist");
        let is_in = matches!(schedule.direction, Direction::In);
        let mut actual_total = 0usize;
        for plan in active.packets.iter_mut() {
            let desc = &ring.read_descs(plan.desc_index, 1)[0];
            let actual = if is_in {
                plan.planned.saturating_sub(desc.iso_remaining() as usize)
            } else {
                plan.planned
            };
            plan.actual = actual;
            plan.status = if desc.iso_status_error() {
                TransferStatus::Error
            } else {
                TransferStatus::Completed
            };
            actual_total += actual;
        }
        for plan in &active.packets {
            ring.clear(plan.desc_index);
        }

        // IN：拷回整段请求区（未收满的槽位保持初始 0，调用方可整槽读取）。
        active
            .transfer
            .copy_in_to_request(active.transfer.buffer_len())?;
        Ok(TransferCompletion {
            request_id: active.id,
            status: TransferStatus::Completed,
            actual_length: actual_total,
            iso_packets: active
                .packets
                .iter()
                .map(|plan| IsoPacketResult {
                    requested_length: plan.planned,
                    actual_length: plan.actual,
                    status: plan.status,
                })
                .collect(),
        })
    }

    /// 队尾请求的最后一个包描述符是否仍为活动（未服务）。活动时环尾
    /// `td_last` 至少在下一个 interval 之后才被硬件取指，可安全续插，
    /// 流水线无需等待上一条完成。
    fn pending_tail_active(&self) -> bool {
        self.pending
            .back()
            .is_some_and(|request| self.last_desc_active(request))
    }

    /// 队首请求是否已被硬件服务（其最后一个包描述符的 A 位被硬件回写清零）。
    /// 服务先于中断发生，因此回读必在中断触发后的任务侧可见；该回读用于
    /// 覆盖 XFERCOMPL 位被同批消费（中断合并）时丢失的后续完成。
    fn head_serviced(&self) -> bool {
        !self.last_desc_active(self.pending.front().expect("called with non-empty pending"))
    }

    /// 请求最后一个包描述符的 A 位回读（硬件服务后随状态字回写清零）。
    fn last_desc_active(&self, request: &IsoActiveRequest) -> bool {
        let index = request
            .packets
            .last()
            .expect("ISO request has at least one packet")
            .desc_index;
        self.ring
            .as_ref()
            .expect("ISO ring must exist while requests are pending")
            .read_descs(index, 1)[0]
            .is_active()
    }

    /// 结算队首已服务请求并移入完成队列。`bit_done` 为本次消费到的
    /// XFERCOMPL 位（覆盖第一个请求的结算，不依赖 A 位回写），其后逐项
    /// 以 A 位回读结算同一批中已服务的请求。
    fn drain_serviced(&mut self, bit_done: bool) {
        let mut done = bit_done;
        while !self.pending.is_empty() {
            if !(done || self.head_serviced()) {
                break;
            }
            let mut head = self.pending.pop_front().expect("non-empty checked above");
            let result = self.settle_request(&mut head);
            self.dma_pool.reclaim(head.transfer);
            self.completed.push_back((head.id, result));
            done = false;
        }
    }

    /// 终止整会话：把全部在飞请求结算为同一错误并释放常驻通道。
    /// 不访问寄存器，因此断开路径也安全。
    fn settle_all_with(&mut self, mk_err: impl FnMut() -> TransferError) {
        self.fatal = None;
        let mut mk_err = mk_err;
        while let Some(active) = self.pending.pop_front() {
            self.dma_pool.reclaim(active.transfer);
            self.completed.push_back((active.id, Err(mk_err())));
        }
        self.release_idle_channel();
    }

    /// 从完成队列取走指定 id 的结果（允许乱序 reclaim）。
    fn take_completed(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        let pos = self
            .completed
            .iter()
            .position(|(done_id, _)| *done_id == id)?;
        self.completed.remove(pos).map(|(_, result)| result)
    }

    /// 推进状态机：消费通道完成位，结算所有已服务的在飞请求；cancel/故障/
    /// 断开则整会话结算。所有结果进入 `completed` 队列，由 `reclaim` 取走。
    fn poll_channel(&mut self) {
        if self.lease.is_none() {
            return;
        }
        let channel = self.channel();
        let Some(hcint) = self.pool.completions.take(channel) else {
            return;
        };

        if hcint & DWC2_COMPLETION_DISCONNECTED != 0 {
            self.settle_all_with(|| TransferError::Disconnected);
            return;
        }

        if let Some(fault) = hcint_fault(hcint) {
            self.stats.record_fault(fault);
            warn!("dwc2: iso channel fault ch={channel} hcint={hcint:#x}");
            if hcint & HCINT_CHHLTD != 0 {
                // 硬件已自行停稳（CHHLTD 同批到达），直接整会话结算。
                self.settle_all_with(|| fault_to_transfer_error(fault, hcint));
            } else {
                // 硬件尚未停：置中止原因并下发 halt，等 CHHLTD 确认后结算。
                self.fatal = Some(IsoHaltReason::Fault(fault, hcint));
                let _ = self.halt_channel();
            }
            return;
        }

        if let Some(reason) = self.fatal {
            if hcint & HCINT_CHHLTD == 0 {
                // 停稳确认未到：halt 已在置中止原因时下发，继续等待。
                return;
            }
            self.settle_all_with(move || reason.error());
            return;
        }

        if hcint & HCINT_CHHLTD != 0 {
            // 无中止原因却停稳：通道意外停摆，防御性整会话结算。
            warn!("dwc2: iso channel halted without fault ch={channel} hcint={hcint:#x}");
            self.settle_all_with(|| {
                TransferError::Other(anyhow!(
                    "DWC2 ISO channel halted without completion hcint={hcint:#x}"
                ))
            });
            return;
        }

        // 正常路径：结算本批已服务的请求（本次 XFERCOMPL 位 + A 位回读）。
        self.drain_serviced(hcint & HCINT_XFERCOMPL != 0);
    }
}

// ═══════════════════════════════════════════
// 状态机对外接口
// ═══════════════════════════════════════════

impl IsoChannelState {
    /// 提交一次 ISO 传输并立即编程入环（常驻通道在首次 submit 时自行租借）。
    /// 环容量不足（在飞包数总和达到 `iso_max_packets`）时返回 `QueueFull`。
    pub(crate) fn submit(
        &mut self,
        cfg: &ChannelConfig,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        // 1. 入口校验：中止中的会话（QueueFull）、Low-speed（NotSupported）、
        //    非 ISO 请求或空包/无缓冲（InvalidEndpoint）一律拒绝。
        if self.fatal.is_some() {
            // 会话正在中止（cancel/故障），停稳结算后由下一次 submit 建立新会话。
            return Err(TransferError::QueueFull);
        }
        if matches!(cfg.port_speed, Speed::Low) {
            return Err(TransferError::NotSupported);
        }
        if !matches!(request, TransferRequest::Isochronous { .. }) {
            return Err(TransferError::InvalidEndpoint);
        }
        if request.iso_packets().is_empty() {
            return Err(TransferError::Other(anyhow!(
                "DWC2 ISO request has no packets"
            )));
        }
        if request.buffer().is_none() {
            return Err(TransferError::Other(anyhow!(
                "DWC2 ISO request has no buffer"
            )));
        }

        // 2. 环容量反压：在飞包数总和不得超过 ring/gcd(interval, ring) − 1，
        //    否则描述符索引按 interval 步进回绕会与未服务槽位碰撞。
        let interval_slots = u32::from(cfg.info.interval.max(1));
        let max_packets = iso_max_packets(interval_slots, cfg.port_speed);
        let in_ring = self
            .pending
            .iter()
            .map(|request| request.packets.len())
            .sum::<usize>();
        let packet_count = request.iso_packets().len();
        if in_ring + packet_count > max_packets {
            if self.pending.is_empty() {
                return Err(TransferError::Other(anyhow!(
                    "DWC2 ISO request has {packet_count} packets exceeding ring capacity \
                     {max_packets} (interval={interval_slots}, ring={})",
                    iso_ring_size(cfg.port_speed),
                )));
            }
            return Err(TransferError::QueueFull);
        }

        // 3. 分配请求 id，编程入环后入队，之后由硬件按帧调度服务。
        let id = self.allocate_request_id();
        let active = self.prepare_request(cfg, id, request, interval_slots)?;
        self.pending.push_back(active);
        Ok(id)
    }

    /// 取走指定 id 的完成结果。结果未就绪时推进状态机后再取；id 仍
    /// 在飞返回 `None`，未知 id 返回 `InvalidEndpoint`。
    pub(crate) fn reclaim(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        if let Some(result) = self.take_completed(id) {
            return Some(result);
        }
        self.poll_channel();
        if let Some(result) = self.take_completed(id) {
            return Some(result);
        }
        if self.pending.iter().any(|request| request.id == id) {
            None
        } else {
            Some(Err(TransferError::InvalidEndpoint))
        }
    }

    pub(crate) fn register_waker(&self, id: RequestId, cx: &mut Context<'_>) {
        if self.pending.iter().any(|request| request.id == id)
            && let Some(lease) = self.lease.as_ref()
        {
            lease.completions.register_waker(lease.channel, cx);
        }
    }

    /// 取消请求：结果已入完成队列则改写为 Cancelled；在飞请求则停通道、
    /// 停稳后整会话结算为 Cancelled（Linux `dwc2_hcd_urb_dequeue` 同款
    /// 会话终止语义，后续 submit 建立新会话）。
    pub(crate) fn cancel(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        if let Some(entry) = self
            .completed
            .iter_mut()
            .find(|(done_id, _)| *done_id == id)
        {
            entry.1 = Err(TransferError::Cancelled);
            return Ok(());
        }
        if !self.pending.iter().any(|request| request.id == id) {
            return Err(TransferError::InvalidEndpoint);
        }
        if self.fatal.is_some() {
            // 会话已在终止（cancel/故障）等待停稳，重复取消为无操作。
            return Ok(());
        }
        self.fatal = Some(IsoHaltReason::Cancelled);
        let result = self.halt_channel();
        if matches!(result, Err(TransferError::Disconnected)) {
            return Ok(());
        }
        result
    }

    pub(crate) fn reset(&mut self) -> core::result::Result<(), TransferError> {
        if self.pending.is_empty() && self.completed.is_empty() && self.fatal.is_none() {
            self.td_last = 0;
            Ok(())
        } else {
            Err(TransferError::QueueFull)
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;

    use super::*;

    #[test]
    fn iso_ring_size_follows_speed() {
        assert_eq!(iso_ring_size(Speed::High), 256);
        assert_eq!(iso_ring_size(Speed::Full), 64);
        assert_eq!(iso_ring_size(Speed::Low), 64);
    }

    #[test]
    fn iso_schinfo_bitmap_matches_linux() {
        // HS 每帧微帧位图：interval=1 → 全部 8 微帧；2 → 0,2,4,6；8 → 仅微帧 0。
        assert_eq!(iso_schinfo(Speed::High, 1), 0xff);
        assert_eq!(iso_schinfo(Speed::High, 2), 0x55);
        assert_eq!(iso_schinfo(Speed::High, 4), 0x11);
        assert_eq!(iso_schinfo(Speed::High, 8), 0x01);
        assert_eq!(iso_schinfo(Speed::High, 16), 0x01);
        // 非 HS 恒 0xff。
        assert_eq!(iso_schinfo(Speed::Full, 1), 0xff);
        assert_eq!(iso_schinfo(Speed::Full, 16), 0xff);
    }

    #[test]
    fn iso_pid_matches_linux_pid_table() {
        assert_eq!(iso_pid(Speed::High, Direction::In, 1), Dwc2Pid::Data0);
        assert_eq!(iso_pid(Speed::High, Direction::In, 2), Dwc2Pid::Data1);
        assert_eq!(iso_pid(Speed::High, Direction::In, 3), Dwc2Pid::Data2);
        assert_eq!(iso_pid(Speed::High, Direction::Out, 1), Dwc2Pid::Data0);
        assert_eq!(iso_pid(Speed::High, Direction::Out, 2), Dwc2Pid::MData);
        assert_eq!(iso_pid(Speed::Full, Direction::In, 3), Dwc2Pid::Data0);
        assert_eq!(iso_pid(Speed::Full, Direction::Out, 3), Dwc2Pid::Data0);
    }

    #[test]
    fn frame_to_desc_idx_aligns_hs_to_frame_0_microframe() {
        // HS：整帧 0 → 索引 0，整帧 1 → 索引 8，整帧 32 → 回绕 0。
        assert_eq!(frame_to_desc_idx(Speed::High, 0), 0);
        assert_eq!(frame_to_desc_idx(Speed::High, 1), 8);
        assert_eq!(frame_to_desc_idx(Speed::High, 31), 248);
        assert_eq!(frame_to_desc_idx(Speed::High, 32), 0);
        // FS：帧号 mod 64 直接作索引。
        assert_eq!(frame_to_desc_idx(Speed::Full, 0), 0);
        assert_eq!(frame_to_desc_idx(Speed::Full, 63), 63);
        assert_eq!(frame_to_desc_idx(Speed::Full, 64), 0);
    }

    #[test]
    fn iso_starting_frame_skips_race_margin() {
        // HS 微帧位 <5 跳过 1 帧，>=5 跳过 2 帧，结果对齐整帧。
        assert_eq!(iso_starting_frame(0x10, Speed::High), (0x10 >> 3) + 1);
        assert_eq!(iso_starting_frame(0x15, Speed::High), (0x15 >> 3) + 2);
        // FS 恒跳过 2 帧。
        assert_eq!(iso_starting_frame(0x10, Speed::Full), 0x12);
    }

    #[test]
    fn advance_along_grid_keeps_schedule_residue() {
        // HS interval=8（整除 256）：保持 mod 8 同余，推进到 >= target。
        assert_eq!(advance_along_grid(8, 20, 8, 256), 24);
        // 已越过时对齐到下一网格点。
        assert_eq!(advance_along_grid(8, 4, 8, 256), 8);
        // 环回绕：target 低、td_last 高时推进到下一圈。
        assert_eq!(advance_along_grid(248, 4, 8, 256), 8);
        // interval=3 与 ring=256 互质：任意索引均在调度网格上，直接取 target。
        assert_eq!(advance_along_grid(0, 10, 3, 256), 10);
        // FS interval=4（gcd(4,64)=4）：推进到 mod 4 同余。
        assert_eq!(advance_along_grid(8, 10, 4, 64), 12);
    }

    #[test]
    fn iso_max_packets_prevents_ring_collision() {
        // HS interval 整除 256：max = 256/interval − 1。
        assert_eq!(iso_max_packets(1, Speed::High), 255);
        assert_eq!(iso_max_packets(8, Speed::High), 31);
        assert_eq!(iso_max_packets(16, Speed::High), 15);
        // FS interval=1 → 63；interval=48（gcd(48,64)=16）→ 3。
        assert_eq!(iso_max_packets(1, Speed::Full), 63);
        assert_eq!(iso_max_packets(48, Speed::Full), 3);
        assert_eq!(iso_max_packets(7, Speed::Full), 63);
    }

    #[test]
    fn iso_service_frames_covers_full_rollover_cycle() {
        // FS interval=8：从起始帧起每 8 帧一个服务帧，共 8 个。
        let frames = iso_service_frames(0, 8, Speed::Full);
        assert_eq!(frames, vec![0, 8, 16, 24, 32, 40, 48, 56]);
        // HS interval=8（微帧）→ 每 1 帧服务，覆盖全部 64 帧。
        let frames = iso_service_frames(3, 8, Speed::High);
        assert_eq!(frames.len(), 64);
        assert!(frames.contains(&3));
        // HS interval=16（微帧）→ 每 2 帧服务，从奇数起始帧开始覆盖 32 个奇数帧。
        let frames = iso_service_frames(1, 16, Speed::High);
        assert_eq!(frames.len(), 32);
        assert!(frames.iter().all(|f| f % 2 == 1));
        // FS interval=3（gcd(3,64)=1）覆盖全部 64 帧。
        let frames = iso_service_frames(0, 3, Speed::Full);
        assert_eq!(frames.len(), 64);
    }
}
