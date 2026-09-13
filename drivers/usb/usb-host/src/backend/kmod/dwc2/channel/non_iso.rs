//! 非 ISO 传输状态机（control/bulk/interrupt 的 DDMA 阶段推进）。
//!
//! 一次传输 = 若干 stage（每条 stage 是一条 A 全置、链尾 IOC+EOL 的 desc 链），
//! 每 stage 一次通道编程、一次 CHHLTD 完成中断；结算与推进全部在任务侧完成
//! （IRQ 只通过完成槽发布 hcint 位）。

use alloc::{vec, vec::Vec};
use core::{sync::atomic::Ordering, task::Context};

use dma_api::CoherentArray;
use mbarrier::mb;
use usb_if::{
    descriptor::EndpointType,
    endpoint::{RequestId, TransferCompletion, TransferRequest, TransferStatus},
    err::TransferError,
    host::{ControlSetup, hub::Speed},
    transfer::{BmRequestType, Direction},
};

use crate::backend::kmod::{
    Kernel,
    dwc2::{
        Dwc2EpType, Dwc2Pid,
        channel::{ChannelConfig, ChannelLease, HostChannelPool},
        dma::{DmaDescriptor, DmaDescriptors, Dwc2DmaBuffer, Dwc2DmaBufferPool, initial_len},
        dma_addr32, endpoint_number, fault_to_transfer_error, hcchar, hcint_fault, hctsiz_ddma,
        reg::{
            DWC2_COMPLETION_DISCONNECTED, DWC2_DMA_ALIGN, DWC2_MAX_DMA_DESCS, DWC2_STATUS_BUF_SIZE,
            Dwc2Registers, HCCHAR_EPDIR,
        },
        stats::Dwc2Stats,
    },
};

// ═══════════════════════════════════════════
// stage 模型
// ═══════════════════════════════════════════

/// 一次通道编程（一个 stage = 一条 desc 链，链尾 EOL 后通道自停一次）。
#[derive(Debug, Clone)]
struct Dwc2TransferStage {
    hcchar: u32,
    hctsiz: u32,       // DDMA 编码（PID | NTD = desc 数 − 1 | SCHINFO）
    dma_addr: u32,     // 本 stage 数据缓冲基址（desc.buf 从这起按 expect 逐块推进）
    desc_base: usize,  // 本 stage 在请求 desc 数组中的起始索引（prepare 时分配）
    descs: Vec<usize>, // 每 desc 的期望长度
}

impl Dwc2TransferStage {
    fn total_len(&self) -> usize {
        self.descs.iter().sum()
    }
}

#[derive(Debug, Clone)]
struct Dwc2ControlPlan {
    setup: Dwc2TransferStage,
    data: Vec<Dwc2TransferStage>,
    status: Dwc2TransferStage,
}

#[derive(Clone, Copy, Debug)]
enum Dwc2StageRole {
    ControlSetup,
    ControlData,
    ControlStatus,
    Data {
        direction: Direction,
        max_packet_size: u16,
    },
}

#[derive(Clone, Debug)]
struct Dwc2QueuedStage {
    stage: Dwc2TransferStage,
    role: Dwc2StageRole,
}

struct Dwc2ActiveRequest {
    id: RequestId,
    channel: ChannelLease,
    transfer: Dwc2DmaBuffer,
    _setup_dma: Option<CoherentArray<u8>>,
    descs: DmaDescriptors,
    stages: Vec<Dwc2QueuedStage>,
    next_stage: usize,
    in_flight: Option<Dwc2QueuedStage>,
    actual_length: usize,
    cancelled: bool,
}

struct Dwc2PreparedStages {
    stages: Vec<Dwc2QueuedStage>,
    setup_dma: Option<CoherentArray<u8>>,
}

/// 通道数据 toggle（跨请求延续，DDMA 下由 HCTSIZ.PID 编程）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DataToggle(bool);

impl DataToggle {
    const fn data0() -> Self {
        Self(false)
    }

    const fn data1() -> Self {
        Self(true)
    }

    fn pid(self) -> Dwc2Pid {
        if self.0 {
            Dwc2Pid::Data1
        } else {
            Dwc2Pid::Data0
        }
    }

    fn advance(&mut self, packet_count: u32) {
        if packet_count % 2 == 1 {
            self.0 = !self.0;
        }
    }
}

// ═══════════════════════════════════════════
// 构建与结算
// ═══════════════════════════════════════════

/// 单 stage 结算：OUT 用计划总长（DDMA 下 OUT 无硬件回写）；
/// IN 逐 desc 读回写剩余字节：actual = Σ min(initial_i − remaining_i, expect_i)。
/// 未处理的 desc（短包中断链）保留编程值 → 贡献 0。
fn stage_actual_length(stage: &Dwc2TransferStage, descs: &[DmaDescriptor]) -> usize {
    if stage.hcchar & HCCHAR_EPDIR == 0 {
        return stage.total_len();
    }
    let mps = stage.hcchar & 0x7ff;
    stage
        .descs
        .iter()
        .zip(descs)
        .map(|(expect, desc)| {
            initial_len(*expect, mps, true)
                .saturating_sub(desc.remaining() as usize)
                .min(*expect)
        })
        .sum()
}

/// 按 stage 角色构建其整条 desc 链：A 全置、链尾 IOC+EOL、setup 附加 SUP；
/// buf 从 `stage.dma_addr` 起按 expect 长度逐块推进。
fn build_stage_descs(queued: &Dwc2QueuedStage) -> Vec<DmaDescriptor> {
    let mps = queued.stage.hcchar & 0x7ff;
    let is_in = queued.stage.hcchar & HCCHAR_EPDIR != 0;
    let n = queued.stage.descs.len();
    let mut buf = queued.stage.dma_addr;
    let mut out = Vec::with_capacity(n);
    for (i, expect) in queued.stage.descs.iter().enumerate() {
        let last = i + 1 == n;
        out.push(match queued.role {
            Dwc2StageRole::ControlSetup => DmaDescriptor::new_setup(buf, 8),
            _ if is_in => DmaDescriptor::new_in(buf, *expect as u32, mps, last),
            _ => DmaDescriptor::new_out(buf, *expect as u32, mps, last),
        });
        buf = buf.wrapping_add(*expect as u32);
    }
    out
}

fn build_control_plan(
    request: &TransferRequest,
    device: u8,
    max_packet_size: u16,
    setup_dma: u32,
    data_dma: u32,
    status_dma: u32,
) -> core::result::Result<Dwc2ControlPlan, TransferError> {
    let TransferRequest::Control {
        direction, buffer, ..
    } = request
    else {
        return Err(TransferError::InvalidEndpoint);
    };

    let data_len = buffer.map(|buffer| buffer.len).unwrap_or(0);
    let setup = Dwc2TransferStage {
        hcchar: hcchar(
            device,
            0,
            Direction::Out,
            Dwc2EpType::Control,
            max_packet_size,
            false,
            1,
        ),
        hctsiz: hctsiz_ddma(Dwc2Pid::Setup, 1, 0),
        dma_addr: setup_dma,
        desc_base: 0,
        descs: vec![8],
    };

    // 数据段合并为单 stage 多 desc 链（EOL 只在链尾 → 整段一次中断）；
    // 超过 NTD 上限（256 desc）时按组拆分，组间独立通道编程。
    let mut data = Vec::new();
    if data_len > 0 {
        let chunks = split_dma_lengths(data_len, max_packet_size);
        let mut offset = 0usize;
        let mut toggle = DataToggle::data1();
        for group in chunks.chunks(DWC2_MAX_DMA_DESCS) {
            let group_len = group.iter().sum::<usize>();
            let packets = packet_count(group_len, max_packet_size);
            data.push(Dwc2TransferStage {
                hcchar: hcchar(
                    device,
                    0,
                    *direction,
                    Dwc2EpType::Control,
                    max_packet_size,
                    false,
                    1,
                ),
                hctsiz: hctsiz_ddma(toggle.pid(), group.len() as u32, 0),
                dma_addr: data_dma.wrapping_add(offset as u32),
                desc_base: 0,
                descs: group.to_vec(),
            });
            toggle.advance(packets);
            offset += group_len;
        }
    }

    let status_direction = if data_len > 0 {
        match direction {
            Direction::In => Direction::Out,
            Direction::Out => Direction::In,
        }
    } else {
        Direction::In
    };
    let status = Dwc2TransferStage {
        hcchar: hcchar(
            device,
            0,
            status_direction,
            Dwc2EpType::Control,
            max_packet_size,
            false,
            1,
        ),
        hctsiz: hctsiz_ddma(Dwc2Pid::Data1, 1, 0),
        dma_addr: match status_direction {
            Direction::In => status_dma,
            Direction::Out => setup_dma,
        },
        desc_base: 0,
        descs: vec![0],
    };
    Ok(Dwc2ControlPlan {
        setup,
        data,
        status,
    })
}

fn packet_count(len: usize, max_packet_size: u16) -> u32 {
    if len == 0 {
        return 1;
    }
    let max_packet_size = u32::from(max_packet_size.max(1));
    (len as u32).div_ceil(max_packet_size)
}

fn split_dma_lengths(len: usize, max_packet_size: u16) -> Vec<usize> {
    if len == 0 {
        return vec![0];
    }

    let mps = usize::from(max_packet_size.max(1));
    // 单 desc 的 NBYTES 上限（17 位）预留一个包，避免 IN 取整溢出。
    let max_len = (DmaDescriptor::NBYTES_LIMIT as usize)
        .saturating_sub(mps - 1)
        .max(1);
    // 切块对齐到 mps 整数倍：保证各 desc 取整后的 NBYTES 之和不越过
    // 按整请求 initial_len 分配的 IN 缓冲（非 2 的幂 mps 下防越界）。
    let max_chunk = max_len / mps * mps;
    let mut left = len;
    let mut out = Vec::new();
    while left > max_chunk {
        out.push(max_chunk);
        left -= max_chunk;
    }
    out.push(left);
    out
}

fn successful_packet_count(actual: usize, requested: usize, max_packet_size: u16) -> u32 {
    if requested == 0 || actual == 0 {
        1
    } else {
        packet_count(actual, max_packet_size)
    }
}

fn setup_packet_bytes(setup: &ControlSetup, direction: Direction, len: usize) -> [u8; 8] {
    let request_type = BmRequestType::new(direction, setup.request_type, setup.recipient);
    let value = setup.value.to_le_bytes();
    let index = setup.index.to_le_bytes();
    let len = (len as u16).to_le_bytes();
    [
        request_type.into(),
        setup.request.into(),
        value[0],
        value[1],
        index[0],
        index[1],
        len[0],
        len[1],
    ]
}

// ═══════════════════════════════════════════
// 状态机
// ═══════════════════════════════════════════

/// 非 ISO 传输状态机：持有一个在飞请求与一个待回收完成
pub(crate) struct NonIsoChannelState {
    regs: Dwc2Registers,
    kernel: Kernel,
    stats: Dwc2Stats,
    dma_pool: Dwc2DmaBufferPool,
    pool: HostChannelPool,
    status_buf: Option<CoherentArray<u8>>,
    data_toggle: DataToggle,
    next_request_id: u64,
    active: Option<Dwc2ActiveRequest>,
    completed: Option<(
        RequestId,
        core::result::Result<TransferCompletion, TransferError>,
    )>,
}

impl Drop for NonIsoChannelState {
    fn drop(&mut self) {
        if let Some(active) = self.active.take()
            && active.channel.hardware_active.load(Ordering::Acquire)
        {
            error!(
                "dwc2: leaking request {:?} DMA because channel {} still references it",
                active.id, active.channel.channel
            );
            core::mem::forget(active);
        }
    }
}

impl NonIsoChannelState {
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
            status_buf: None,
            data_toggle: DataToggle::data0(),
            next_request_id: 1,
            active: None,
            completed: None,
        }
    }

    /// 在飞或已完成的请求 id（端点回收/配置切换前停稳用）。
    pub(crate) fn in_flight_request_id(&self) -> Option<RequestId> {
        self.active
            .as_ref()
            .map(|active| active.id)
            .or_else(|| self.completed.as_ref().map(|(id, _)| *id))
    }

    fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        RequestId::new(id)
    }

    fn prepare_request(
        &mut self,
        cfg: &ChannelConfig,
        id: RequestId,
        channel: ChannelLease,
        request: TransferRequest,
    ) -> core::result::Result<Dwc2ActiveRequest, TransferError> {
        if matches!(request, TransferRequest::Isochronous { .. }) {
            return Err(TransferError::NotSupported);
        }

        self.stats.record_transfer();
        let mps = match &request {
            TransferRequest::Control { .. } => cfg.info.max_packet_size.max(8),
            _ => cfg.info.max_packet_size.max(1),
        };
        // 分配长度：IN 按 initial_len 取整（设备可合法收满取整量，防 DMA
        // 越界），OUT/零长至少 1 字节，desc 始终有合法 buf 地址。
        let direction = request.direction();
        let buffer = request.buffer();
        let data_len = buffer.map_or(0, |buffer| buffer.len);
        let alloc_len = match direction {
            Direction::In => initial_len(data_len, mps as u32, true).max(1),
            Direction::Out => data_len.max(1),
        };
        let transfer = Dwc2DmaBuffer::new(
            &self.kernel,
            &mut self.dma_pool,
            &self.stats,
            buffer,
            direction,
            alloc_len,
        )?;
        let mut prepared = match &request {
            TransferRequest::Control { .. } => self.control_stages(cfg, &request, &transfer)?,
            TransferRequest::Bulk { .. } | TransferRequest::Interrupt { .. } => {
                Dwc2PreparedStages {
                    stages: self.data_stages(cfg, &request, &transfer)?,
                    setup_dma: None,
                }
            }
            TransferRequest::Isochronous { .. } => return Err(TransferError::NotSupported),
        };

        // 为各 stage 分配 desc 数组中的连续区间（desc_base），再一次性写入
        // 整条链（每 stage 一条 A 全置、链尾 IOC+EOL 的 desc 链）。
        let mut desc_base = 0usize;
        for queued in &mut prepared.stages {
            queued.stage.desc_base = desc_base;
            desc_base += queued.stage.descs.len();
        }
        let descs = DmaDescriptors::new(&self.kernel, desc_base, DWC2_DMA_ALIGN)
            .map_err(|err| TransferError::Other(anyhow!("DWC2 desc array alloc failed: {err}")))?;
        let desc_vec: Vec<DmaDescriptor> =
            prepared.stages.iter().flat_map(build_stage_descs).collect();
        if log::log_enabled!(log::Level::Debug) {
            for (i, desc) in desc_vec.iter().enumerate() {
                log::debug!(
                    "dwc2: desc[{i}] status={:#010x} buf={:#010x}",
                    desc.status.get(),
                    desc.paddr,
                );
            }
        }
        descs.write_descs(0, &desc_vec);

        Ok(Dwc2ActiveRequest {
            id,
            channel,
            transfer,
            _setup_dma: prepared.setup_dma,
            descs,
            stages: prepared.stages,
            next_stage: 0,
            in_flight: None,
            actual_length: 0,
            cancelled: false,
        })
    }

    fn control_stages(
        &mut self,
        cfg: &ChannelConfig,
        request: &TransferRequest,
        transfer: &Dwc2DmaBuffer,
    ) -> core::result::Result<Dwc2PreparedStages, TransferError> {
        let TransferRequest::Control {
            setup, direction, ..
        } = request
        else {
            return Err(TransferError::InvalidEndpoint);
        };

        let mut setup_dma = self
            .kernel
            .coherent_array_zero_with_align::<u8>(8, DWC2_DMA_ALIGN)
            .map_err(|err| {
                TransferError::Other(anyhow!("DWC2 setup DMA allocation failed: {err}"))
            })?;
        self.stats.record_dma_alloc();
        let setup_bytes = setup_packet_bytes(setup, *direction, transfer.buffer_len());
        setup_dma.write_with_cpu(8, |dst| dst.copy_from_slice(&setup_bytes));
        let setup_addr = dma_addr32(setup_dma.dma_addr().as_u64())?;
        let data_addr = dma_addr32(transfer.dma_addr())?;
        let status_addr = dma_addr32(self.status_buf_addr()?)?;
        let plan = build_control_plan(
            request,
            cfg.device_address,
            cfg.info.max_packet_size.max(8),
            setup_addr,
            data_addr,
            status_addr,
        )?;

        let mut stages = Vec::with_capacity(plan.data.len() + 2);
        stages.push(Dwc2QueuedStage {
            stage: plan.setup,
            role: Dwc2StageRole::ControlSetup,
        });
        for stage in plan.data {
            stages.push(Dwc2QueuedStage {
                stage,
                role: Dwc2StageRole::ControlData,
            });
        }
        stages.push(Dwc2QueuedStage {
            stage: plan.status,
            role: Dwc2StageRole::ControlStatus,
        });
        Ok(Dwc2PreparedStages {
            stages,
            setup_dma: Some(setup_dma),
        })
    }

    /// 控制传输 status IN 阶段的可写缓冲（64B，惰性分配、跨请求复用）。
    /// desc 编程 NBYTES = mps ≤ 64，设备合法收满时不会越界。
    fn status_buf_addr(&mut self) -> core::result::Result<u64, TransferError> {
        if self.status_buf.is_none() {
            let status_buf = self
                .kernel
                .coherent_array_zero_with_align::<u8>(DWC2_STATUS_BUF_SIZE, DWC2_DMA_ALIGN)
                .map_err(|err| {
                    TransferError::Other(anyhow!("DWC2 status DMA allocation failed: {err}"))
                })?;
            self.stats.record_dma_alloc();
            self.status_buf = Some(status_buf);
        }
        Ok(self
            .status_buf
            .as_ref()
            .expect("status buffer must exist after allocation")
            .dma_addr()
            .as_u64())
    }

    fn data_stages(
        &self,
        cfg: &ChannelConfig,
        request: &TransferRequest,
        transfer: &Dwc2DmaBuffer,
    ) -> core::result::Result<Vec<Dwc2QueuedStage>, TransferError> {
        let (direction, ep_type) = match request {
            TransferRequest::Bulk { direction, .. } => (*direction, Dwc2EpType::Bulk),
            TransferRequest::Interrupt { direction, .. } => (*direction, Dwc2EpType::Interrupt),
            _ => return Err(TransferError::InvalidEndpoint),
        };

        let mps = cfg.info.max_packet_size.max(1);
        let endpoint = endpoint_number(cfg.info.address.raw());
        // 数据段合并为单 stage 多 desc 链（一次通道编程、一次中断）；
        // 超过 NTD 上限时按组拆分，组间独立通道编程。
        let chunks = split_dma_lengths(transfer.buffer_len(), mps);
        let mut stages = Vec::new();
        let mut toggle = self.data_toggle;
        let mut offset = 0u64;
        for group in chunks.chunks(DWC2_MAX_DMA_DESCS) {
            let group_len = group.iter().sum::<usize>();
            let packets = packet_count(group_len, mps);
            let mut stage = Dwc2TransferStage {
                hcchar: hcchar(
                    cfg.device_address,
                    endpoint,
                    direction,
                    ep_type,
                    mps,
                    matches!(cfg.port_speed, Speed::Low),
                    1,
                ),
                hctsiz: hctsiz_ddma(toggle.pid(), group.len() as u32, 0),
                dma_addr: dma_addr32(transfer.dma_addr() + offset)?,
                desc_base: 0,
                descs: group.to_vec(),
            };
            if matches!(ep_type, Dwc2EpType::Interrupt) {
                stage.hcchar |= self.regs.periodic_odd_frame_bit();
            }
            stages.push(Dwc2QueuedStage {
                stage,
                role: Dwc2StageRole::Data {
                    direction,
                    max_packet_size: mps,
                },
            });
            toggle.advance(packets);
            offset += group_len as u64;
        }
        Ok(stages)
    }

    fn start_active_request(
        &mut self,
        mut active: Dwc2ActiveRequest,
    ) -> core::result::Result<Dwc2ActiveRequest, TransferError> {
        self.start_next_stage(&mut active)?;
        Ok(active)
    }

    fn start_next_stage(
        &self,
        active: &mut Dwc2ActiveRequest,
    ) -> core::result::Result<bool, TransferError> {
        let Some(queued) = active.stages.get(active.next_stage).cloned() else {
            return Ok(false);
        };
        active.next_stage += 1;
        let desc_addr = active
            .descs
            .dma_addr()
            .wrapping_add((queued.stage.desc_base as u32).wrapping_mul(DmaDescriptor::SIZE as u32));
        self.start_stage(&active.channel, queued.stage.clone(), desc_addr)?;
        active.in_flight = Some(queued);
        Ok(true)
    }

    fn start_stage(
        &self,
        channel: &ChannelLease,
        stage: Dwc2TransferStage,
        desc_addr: u32,
    ) -> core::result::Result<(), TransferError> {
        channel.completions.with_connected(|| {
            let regs = self.regs.channel(channel.channel);
            if regs.is_enabled() {
                return Err(TransferError::QueueFull);
            }

            self.stats.record_stage();
            channel.completions.clear(channel.channel);
            // SAFETY: the lifecycle IRQ-save gate prevents local DWC2 event
            // re-entry, while the channel lease excludes task-side mutation.
            let _guard = unsafe { channel.gate.lock_raw() };
            regs.set_hcsplt(0);
            regs.clear_all_irqs();
            regs.enable_irqs();
            regs.set_hctsiz(stage.hctsiz);
            mb();
            regs.set_hcdma(desc_addr);
            mb();
            regs.enable(stage.hcchar);
            channel.hardware_active.store(true, Ordering::Release);
            log::debug!(
                "dwc2: stage start ch={} hcchar={:#010x} hctsiz={:#010x} hcdma={:#010x}",
                channel.channel,
                stage.hcchar | 1 << 31,
                stage.hctsiz,
                desc_addr,
            );
            Ok(())
        })
    }

    fn poll_active_request(
        &mut self,
        mut active: Dwc2ActiveRequest,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        let Some(hcint) = active.channel.completions.take(active.channel.channel) else {
            self.active = Some(active);
            return None;
        };
        active
            .channel
            .hardware_active
            .store(false, Ordering::Release);
        if hcint & DWC2_COMPLETION_DISCONNECTED != 0 {
            return Some(self.complete_active_request(active, Err(TransferError::Disconnected)));
        }
        if active.cancelled {
            return Some(self.complete_active_request(active, Err(TransferError::Cancelled)));
        }
        let Some(queued) = active.in_flight.take() else {
            return Some(self.complete_active_request(
                active,
                Err(TransferError::Other(anyhow!(
                    "DWC2 completion arrived without an in-flight stage"
                ))),
            ));
        };

        // DDMA 下无软件 NAK/XACT 重试：NAK 由硬件自动重试，其余故障
        // 均以 CHHLTD + 故障位终止，直接上报。
        if let Some(fault) = hcint_fault(hcint) {
            self.stats.record_fault(fault);
            warn!(
                "dwc2: transfer fault channel={} role={:?} hcint={:#x}",
                active.channel.channel, queued.role, hcint,
            );
            return Some(
                self.complete_active_request(active, Err(fault_to_transfer_error(fault, hcint))),
            );
        }

        let read = active
            .descs
            .read_descs(queued.stage.desc_base, queued.stage.descs.len());
        let actual = stage_actual_length(&queued.stage, read);
        log::debug!(
            "dwc2: stage done ch={} base={} ndesc={} role={:?} hcint={:#x} actual={} total={}",
            active.channel.channel,
            queued.stage.desc_base,
            queued.stage.descs.len(),
            queued.role,
            hcint,
            actual,
            active.actual_length,
        );
        match queued.role {
            Dwc2StageRole::ControlSetup | Dwc2StageRole::ControlStatus => {}
            Dwc2StageRole::ControlData => {
                active.actual_length = active.actual_length.saturating_add(actual);
            }
            Dwc2StageRole::Data {
                direction,
                max_packet_size,
            } => {
                active.actual_length = active.actual_length.saturating_add(actual);
                self.data_toggle.advance(successful_packet_count(
                    actual,
                    queued.stage.total_len(),
                    max_packet_size,
                ));
                if matches!(direction, Direction::In) && actual < queued.stage.total_len() {
                    return Some(self.complete_active_request(active, Ok(())));
                }
            }
        }

        match self.start_next_stage(&mut active) {
            Ok(true) => {
                self.active = Some(active);
                None
            }
            Ok(false) => Some(self.complete_active_request(active, Ok(()))),
            Err(err) => Some(self.complete_active_request(active, Err(err))),
        }
    }

    fn complete_active_request(
        &mut self,
        active: Dwc2ActiveRequest,
        result: core::result::Result<(), TransferError>,
    ) -> core::result::Result<TransferCompletion, TransferError> {
        let id = active.id;
        let completion = result.and_then(|()| {
            active.transfer.copy_in_to_request(active.actual_length)?;
            Ok(TransferCompletion {
                request_id: id,
                status: TransferStatus::Completed,
                actual_length: active.actual_length,
                iso_packets: Vec::new(),
            })
        });
        self.dma_pool.reclaim(active.transfer);
        active.channel.release();
        completion
    }
}

// ═══════════════════════════════════════════
// 状态机对外接口
// ═══════════════════════════════════════════

impl NonIsoChannelState {
    /// 提交一次传输：从池中租借通道（控制端点独占通道 0，其余在 1..n 中
    /// 选取）后编程启动；prepare/启动失败时租约随 Drop 归还通道槽。
    pub(crate) fn submit(
        &mut self,
        cfg: &ChannelConfig,
        request: TransferRequest,
    ) -> core::result::Result<RequestId, TransferError> {
        if self.active.is_some() || self.completed.is_some() {
            return Err(TransferError::QueueFull);
        }
        let channel = self
            .pool
            .acquire(matches!(cfg.info.transfer_type, EndpointType::Control))?;
        let id = self.allocate_request_id();
        let active = self.prepare_request(cfg, id, channel, request)?;
        let active = self.start_active_request(active)?;
        self.active = Some(active);
        Ok(id)
    }

    pub(crate) fn reclaim(
        &mut self,
        id: RequestId,
    ) -> Option<core::result::Result<TransferCompletion, TransferError>> {
        if let Some((completed_id, _)) = self.completed.as_ref() {
            if *completed_id != id {
                return Some(Err(TransferError::InvalidEndpoint));
            }
            return self.completed.take().map(|(_, result)| result);
        }

        let active = self.active.take()?;
        if active.id != id {
            self.active = Some(active);
            return Some(Err(TransferError::InvalidEndpoint));
        }
        self.poll_active_request(active)
    }

    pub(crate) fn register_waker(&self, id: RequestId, cx: &mut Context<'_>) {
        if let Some(active) = self.active.as_ref().filter(|active| active.id == id) {
            active
                .channel
                .completions
                .register_waker(active.channel.channel, cx);
        }
    }

    pub(crate) fn cancel(&mut self, id: RequestId) -> core::result::Result<(), TransferError> {
        if self
            .completed
            .as_ref()
            .is_some_and(|(done_id, _)| *done_id == id)
        {
            self.completed = Some((id, Err(TransferError::Cancelled)));
            return Ok(());
        }
        let Some(active) = self.active.as_mut() else {
            return Err(TransferError::InvalidEndpoint);
        };
        if active.id != id {
            return Err(TransferError::InvalidEndpoint);
        }
        active.cancelled = true;
        let channel = active.channel.channel;
        let result = active.channel.completions.with_connected(|| {
            // SAFETY: The lifecycle IRQ-save gate prevents local DWC2
            // event re-entry, while the channel lease excludes task-side
            // register mutation.
            let _guard = unsafe { active.channel.gate.lock_raw() };
            self.regs.channel(channel).disable();
            Ok(())
        });
        if matches!(result, Err(TransferError::Disconnected)) {
            return Ok(());
        }
        result
    }

    pub(crate) fn reset(&mut self) -> core::result::Result<(), TransferError> {
        if self.active.is_some() || self.completed.is_some() {
            Err(TransferError::QueueFull)
        } else {
            self.data_toggle = DataToggle::data0();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec;

    use tock_registers::interfaces::Readable;
    use usb_if::{
        descriptor::EndpointType,
        endpoint::{EndpointAddress, EndpointInfo, TransferRequest},
        host::{ControlSetup, hub::Speed},
        transfer::{Direction, Recipient, Request, RequestType},
    };

    use super::*;
    use crate::backend::kmod::dwc2::{
        dma::DMA_DESCRIPTOR,
        reg::{HCINT_CHHLTD, HCINT_NAK, HCINT_XFERCOMPL},
        testutil as tu,
    };

    fn bulk_config(direction: Direction) -> ChannelConfig {
        ChannelConfig {
            device_address: 2,
            info: EndpointInfo {
                address: EndpointAddress::new(if direction == Direction::In {
                    0x81
                } else {
                    0x01
                }),
                transfer_type: EndpointType::Bulk,
                direction,
                max_packet_size: 512,
                packets_per_microframe: 1,
                interval: 0,
            },
            port_speed: Speed::High,
        }
    }

    fn test_stage(direction: Direction, descs: Vec<usize>) -> Dwc2TransferStage {
        Dwc2TransferStage {
            hcchar: hcchar(
                2,
                if direction == Direction::In { 1 } else { 2 },
                direction,
                Dwc2EpType::Bulk,
                512,
                false,
                1,
            ),
            hctsiz: hctsiz_ddma(Dwc2Pid::Data0, descs.len() as u32, 0),
            dma_addr: 0x4000,
            desc_base: 0,
            descs,
        }
    }

    #[test]
    fn control_plan_uses_dma_for_setup_data_and_status() {
        let mut data = [0u8; 18];
        let request = TransferRequest::control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetDescriptor,
                value: 0x0100,
                index: 0,
            },
            &mut data,
        );

        let plan = build_control_plan(&request, 2, 64, 0x1000, 0x2000, 0x3000)
            .expect("control IN plan builds");

        assert_eq!(plan.setup.dma_addr, 0x1000);
        assert_eq!(plan.setup.descs, vec![8]);
        assert_eq!(plan.setup.hctsiz, hctsiz_ddma(Dwc2Pid::Setup, 1, 0));
        assert_eq!(plan.setup.hcchar & HCCHAR_EPDIR, 0);

        let stage = plan.data.first().expect("control IN has a data stage");
        assert_eq!(stage.dma_addr, 0x2000);
        assert_eq!(stage.hctsiz, hctsiz_ddma(Dwc2Pid::Data1, 1, 0));
        assert_eq!(stage.hcchar & HCCHAR_EPDIR, HCCHAR_EPDIR);
        assert_eq!(stage.descs, vec![18]);

        // 带数据的 status 阶段方向反向（IN → OUT），复位时复用 setup 缓冲。
        assert_eq!(plan.status.hcchar & HCCHAR_EPDIR, 0);
        assert_eq!(plan.status.dma_addr, 0x1000);
        assert_eq!(plan.status.hctsiz, hctsiz_ddma(Dwc2Pid::Data1, 1, 0));
        assert_eq!(plan.status.descs, vec![0]);

        // 无数据的 OUT 控制传输：status 为 IN，使用独立 status 缓冲。
        let out = TransferRequest::control_out(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::SetAddress,
                value: 2,
                index: 0,
            },
            &[],
        );
        let out_plan = build_control_plan(&out, 2, 64, 0x1000, 0x2000, 0x3000)
            .expect("control OUT plan builds");
        assert!(out_plan.data.is_empty());
        assert_eq!(out_plan.status.hcchar & HCCHAR_EPDIR, HCCHAR_EPDIR);
        assert_eq!(out_plan.status.dma_addr, 0x3000);
    }

    #[test]
    fn control_data_stage_splits_at_ntd_limit_and_toggles_pid() {
        // 257 个 desc 块（每块 ≈ 131008 B，17 位 NBYTES 上限内）→ 256 desc
        // 组 + 1 desc 组两个 stage；2047 包（奇数）令第二组 PID 翻转。
        let chunk = 0x1FFC0usize; // mps=64 时 max_chunk = 0x1FFFF & ~63 … 131008
        let mut data = vec![0u8; 256 * chunk + 1];
        let request = TransferRequest::control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetDescriptor,
                value: 0x0200,
                index: 0,
            },
            &mut data,
        );

        let plan = build_control_plan(&request, 2, 64, 0x1000, 0x2000, 0x3000)
            .expect("control split plan builds");

        assert_eq!(plan.data.len(), 2);
        assert_eq!((plan.data[0].hctsiz >> 8) & 0xff, 255);
        assert_eq!((plan.data[1].hctsiz >> 8) & 0xff, 0);
        // 数据阶段从 Data1 起步；整组 256 × 2047 包为偶数 → 分组间不翻转
        //（翻转语义由 data_toggle_advances_by_packet_count 覆盖）。
        assert_eq!(plan.data[0].hctsiz >> 29, Dwc2Pid::Data1.bits());
        assert_eq!(plan.data[1].hctsiz >> 29, Dwc2Pid::Data1.bits());
        assert_eq!(
            plan.data[1].dma_addr - plan.data[0].dma_addr,
            256 * chunk as u32
        );

        // 单块与边界拆分。
        assert_eq!(split_dma_lengths(8192, 8), vec![8192]);
        assert_eq!(split_dma_lengths(1, 8), vec![1]);
        assert_eq!(split_dma_lengths(0, 8), vec![0]);
        let chunks = split_dma_lengths(0x1FFFF, 64);
        assert_eq!(chunks.iter().sum::<usize>(), 0x1FFFF);
        assert_eq!(chunks[..chunks.len() - 1], vec![0x1FFC0]);
        assert_eq!(chunks.last(), Some(&63));
    }

    #[test]
    fn data_toggle_advances_by_packet_count() {
        let mut toggle = DataToggle::data0();

        assert_eq!(toggle.pid(), Dwc2Pid::Data0);
        toggle.advance(packet_count(512, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data1);
        toggle.advance(packet_count(1024, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data1);
        toggle.advance(packet_count(1, 512));
        assert_eq!(toggle.pid(), Dwc2Pid::Data0);
        // 零长请求按 1 个包推进（DDMA 成功结算同款语义）。
        assert_eq!(packet_count(0, 512), 1);
        assert_eq!(successful_packet_count(0, 64, 64), 1);
        assert_eq!(successful_packet_count(64, 64, 64), 1);
        assert_eq!(successful_packet_count(32, 64, 64), 1);
    }

    #[test]
    fn out_stage_completion_reports_requested_length() {
        // OUT：DDMA 下无硬件回写，结算使用计划总长。
        let out = test_stage(Direction::Out, vec![31]);
        let out_desc = DmaDescriptor::new_out(0, 31, 512, true);
        assert_eq!(
            stage_actual_length(&out, core::slice::from_ref(&out_desc)),
            31
        );

        // IN：逐 desc 读回写剩余字节，实际 = 初始 − 剩余。
        let in_stage = test_stage(Direction::In, vec![31]);
        let mut in_desc = DmaDescriptor::new_in(0, 31, 512, true);
        in_desc.status.modify(DMA_DESCRIPTOR::NBYTES.val(512 - 18));
        assert_eq!(
            stage_actual_length(&in_stage, core::slice::from_ref(&in_desc)),
            18
        );

        // 短读：第一块收满（remaining 0）、第二块未处理（保留编程值）→ 只计第一块。
        // 期望长度与 desc 的 mps 必须一致（64）。
        let split = Dwc2TransferStage {
            hcchar: hcchar(2, 1, Direction::In, Dwc2EpType::Bulk, 64, false, 1),
            hctsiz: hctsiz_ddma(Dwc2Pid::Data0, 2, 0),
            dma_addr: 0,
            desc_base: 0,
            descs: vec![64, 64],
        };
        let mut first = DmaDescriptor::new_in(0, 64, 64, false);
        first.status.modify(DMA_DESCRIPTOR::NBYTES.val(0));
        let second = DmaDescriptor::new_in(64, 64, 64, false);
        assert_eq!(stage_actual_length(&split, &[first, second]), 64);
    }

    #[test]
    fn submit_programs_channel_and_waits_for_irq_before_reclaim() {
        let (_backing, regs, kernel, baselines, pool) = tu::channel_fixture(2);
        let stats = Dwc2Stats::new();
        let mut state = NonIsoChannelState::new(regs, kernel, stats.clone(), pool);
        let cfg = bulk_config(Direction::In);
        let mut data = [0u8; 512];
        let id = state
            .submit(&cfg, TransferRequest::bulk_in(&mut data))
            .unwrap();

        // 非控制端点从通道 1 起租借；只开 CHHLTD，NAK/XFERCOMPL 不产生中断
        // （NAK 由硬件重试，XFERCOMPL 由 DDMA 链尾 IOC 伴随 CHHLTD 呈现）。
        assert_eq!(regs.regs().hc[1].hcintmsk.get(), HCINT_CHHLTD);
        assert_eq!(
            regs.regs().hc[1].hcintmsk.get() & (HCINT_NAK | HCINT_XFERCOMPL),
            0
        );
        assert_eq!(regs.regs().hc[1].hcchar.get() & (1 << 31), 1 << 31);
        assert_eq!(regs.regs().hc[1].hctsiz.get() & (3 << 29), 0);
        assert!(state.reclaim(id).is_none());
        assert_eq!(stats.snapshot().transfer_busy_wait_iters, 0);

        // 模拟硬件完成：回写 desc 状态字（A 清、remaining 0）后发布 IRQ。
        state
            .active
            .as_mut()
            .unwrap()
            .descs
            .data
            .write_with_cpu(8, |dst| dst.fill(0));
        baselines.publish(1, HCINT_CHHLTD | HCINT_XFERCOMPL);

        let completion = state.reclaim(id).expect("completion must be published");
        let completion = completion.expect("512B IN transfer completes");
        assert_eq!(completion.actual_length, 512);
    }

    #[test]
    fn cancelled_request_waits_for_real_channel_halt_before_reclaim() {
        let (_backing, regs, kernel, baselines, pool) = tu::channel_fixture(2);
        let mut state = NonIsoChannelState::new(regs, kernel, Dwc2Stats::new(), pool);
        let cfg = bulk_config(Direction::In);
        let mut data = [0u8; 512];
        let id = state
            .submit(&cfg, TransferRequest::bulk_in(&mut data))
            .unwrap();

        state.cancel(id).unwrap();

        // 取消只投递 CHDIS；回收必须等真实 CHHLTD。
        assert_eq!(
            regs.regs().hc[1].hcchar.get() & ((1 << 30) | (1 << 31)),
            1 << 30
        );
        assert!(state.reclaim(id).is_none());
        baselines.publish(1, HCINT_CHHLTD);
        assert!(matches!(
            state.reclaim(id),
            Some(Err(TransferError::Cancelled))
        ));
    }

    #[test]
    fn completed_handler_publish_is_taken_exactly_once() {
        let (_backing, regs, kernel, baselines, pool) = tu::channel_fixture(2);
        let stats = Dwc2Stats::new();
        let mut state = NonIsoChannelState::new(regs, kernel, stats, pool);
        let cfg = bulk_config(Direction::In);
        let mut data = [0u8; 512];
        let id = state
            .submit(&cfg, TransferRequest::bulk_in(&mut data))
            .unwrap();

        baselines.publish(1, HCINT_CHHLTD | HCINT_XFERCOMPL);
        assert!(state.reclaim(id).expect("completion is taken once").is_ok());
        assert!(state.reclaim(id).is_none());
    }
}
