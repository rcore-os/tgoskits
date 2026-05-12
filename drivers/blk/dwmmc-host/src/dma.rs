use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::{DArray, DeviceDma, DmaDirection, SArrayPtr};
use log::warn;
use sdmmc_protocol::{
    cmd::{CMD12, Command, DataDirection, cmd17, cmd18, cmd24, cmd25},
    error::{Error, ErrorContext, Phase},
    response::Response,
};

use crate::{
    host::{DwMmc, PendingData},
    regs::RegisterBlockVolatileFieldAccess,
};

const DESC_OWN: u32 = 1 << 31;
const DESC_CH: u32 = 1 << 4;
const DESC_FS: u32 = 1 << 3;
const DESC_LD: u32 = 1 << 2;
const DESC_DIC: u32 = 1 << 1;

const BMOD_SWR: u32 = 1 << 0;
const BMOD_FB: u32 = 1 << 1;
const BMOD_DE: u32 = 1 << 7;

const DMA_POLL_LIMIT: u32 = 8_000_000;
pub const IDMAC_DESC_ALIGN: usize = 16;
pub const IDMAC_DESC_SIZE: usize = core::mem::size_of::<IdmacDesc>();
const BLOCK_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestId(usize);

impl RequestId {
    pub const fn new(id: usize) -> Self {
        Self(id)
    }
}

impl From<RequestId> for usize {
    fn from(value: RequestId) -> Self {
        value.0
    }
}

#[derive(Default)]
pub struct AsyncRequestSlot {
    next: usize,
    active: Option<RequestId>,
}

impl AsyncRequestSlot {
    pub fn start(&mut self) -> Result<RequestId, Error> {
        if self.active.is_some() {
            return Err(Error::UnsupportedCommand);
        }
        let id = RequestId::new(self.next);
        self.next = self.next.wrapping_add(1);
        self.active = Some(id);
        Ok(id)
    }

    pub fn complete(&mut self, id: RequestId) -> Result<(), Error> {
        if self.active != Some(id) {
            return Err(Error::InvalidArgument);
        }
        self.active = None;
        Ok(())
    }
}

pub struct AsyncDmaRequest {
    inner: AsyncDmaRequestKind,
}

// `AsyncDmaRequest` owns the DMA mappings and descriptor buffer for one
// submitted transfer. Moving that ownership to another queue thread does not
// grant shared access to the mapped memory; completion still requires a
// mutable `DwMmc` reference and consumes the request.
unsafe impl Send for AsyncDmaRequest {}

enum AsyncDmaRequestKind {
    Read {
        id: RequestId,
        map: SArrayPtr<u8>,
        _desc: DArray<IdmacDesc>,
        cmd_index: u8,
        phase: Phase,
        stop_after_complete: bool,
    },
    Write {
        id: RequestId,
        _map: SArrayPtr<u8>,
        _desc: DArray<IdmacDesc>,
        cmd_index: u8,
        phase: Phase,
        stop_after_complete: bool,
    },
}

impl AsyncDmaRequest {
    pub fn id(&self) -> RequestId {
        match &self.inner {
            AsyncDmaRequestKind::Read { id, .. } | AsyncDmaRequestKind::Write { id, .. } => *id,
        }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdmacDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

pub struct IdmacRead<'a, F> {
    pub buffer_dma: u64,
    pub desc_dma: u64,
    pub desc_count: usize,
    pub desc_virt: *mut u8,
    pub flush_desc: &'a mut F,
}

impl IdmacDesc {
    pub fn chained(buffer_dma: u32, len: u32, next_desc_dma: u32, first: bool, last: bool) -> Self {
        let mut des0 = DESC_OWN | DESC_CH | DESC_DIC;
        if first {
            des0 |= DESC_FS;
        }
        if last {
            des0 |= DESC_LD;
        }
        Self {
            des0,
            des1: len,
            des2: buffer_dma,
            des3: next_desc_dma,
        }
    }
}

impl DwMmc {
    pub(crate) fn try_idmac_read_transfer(
        &mut self,
        cmd: &Command,
        buf: &mut [u8],
        block_size: u32,
        expected_block_count: u32,
    ) -> Result<Response, Error> {
        if block_size as usize != BLOCK_SIZE || buf.is_empty() {
            return Err(Error::UnsupportedCommand);
        }
        let dma = self.dma.clone().ok_or(Error::UnsupportedCommand)?;
        let size = NonZeroUsize::new(buf.len()).ok_or(Error::InvalidArgument)?;
        let block_count = dma_read_block_count(size)?;
        if block_count != expected_block_count {
            return Err(Error::InvalidArgument);
        }
        let map = dma
            .map_single_array(buf, BLOCK_SIZE, DmaDirection::FromDevice)
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let mut desc = dma
            .array_zero_with_align::<IdmacDesc>(
                block_count as usize,
                IDMAC_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;

        let response =
            self.idmac_transfer_mapped(cmd, block_count, map.dma_addr().as_u64(), &mut desc)?;
        map.prepare_read_all();
        Ok(response)
    }

    pub(crate) fn try_idmac_write_transfer(
        &mut self,
        cmd: &Command,
        buf: &[u8],
        block_size: u32,
        expected_block_count: u32,
    ) -> Result<Response, Error> {
        if block_size as usize != BLOCK_SIZE || buf.is_empty() {
            return Err(Error::UnsupportedCommand);
        }
        let dma = self.dma.clone().ok_or(Error::UnsupportedCommand)?;
        let size = NonZeroUsize::new(buf.len()).ok_or(Error::InvalidArgument)?;
        let block_count = dma_write_block_count(size)?;
        if block_count != expected_block_count {
            return Err(Error::InvalidArgument);
        }
        let map = dma
            .map_single_array(buf, BLOCK_SIZE, DmaDirection::ToDevice)
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        map.confirm_write_all();

        let mut desc = dma
            .array_zero_with_align::<IdmacDesc>(
                block_count as usize,
                IDMAC_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;

        self.idmac_transfer_mapped(cmd, block_count, map.dma_addr().as_u64(), &mut desc)
    }

    pub fn dma_read_blocks_into(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
    ) -> Result<(), Error> {
        let mut slot = AsyncRequestSlot::default();
        let request = self.submit_dma_read_blocks(start_block, buffer, size, dma, &mut slot)?;
        self.wait_async_dma_request(request, &mut slot)?;
        Ok(())
    }

    pub fn dma_write_blocks_from(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
    ) -> Result<(), Error> {
        let mut slot = AsyncRequestSlot::default();
        let request = self.submit_dma_write_blocks(start_block, buffer, size, dma, &mut slot)?;
        self.wait_async_dma_request(request, &mut slot)
    }

    pub fn submit_dma_read_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut AsyncRequestSlot,
    ) -> Result<AsyncDmaRequest, Error> {
        let id = slot.start()?;
        match self.build_dma_read_request(start_block, buffer, size, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub fn submit_dma_write_blocks(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        slot: &mut AsyncRequestSlot,
    ) -> Result<AsyncDmaRequest, Error> {
        let id = slot.start()?;
        match self.build_dma_write_request(start_block, buffer, size, dma, id) {
            Ok(request) => Ok(request),
            Err(err) => {
                let _ = slot.complete(id);
                Err(err)
            }
        }
    }

    pub fn poll_async_dma_request(
        &mut self,
        request: &mut Option<AsyncDmaRequest>,
        id: RequestId,
        slot: &mut AsyncRequestSlot,
    ) -> Result<(), Error> {
        let Some(active) = request.as_ref() else {
            return Err(Error::InvalidArgument);
        };
        if active.id() != id {
            return Err(Error::InvalidArgument);
        }

        let (cmd_index, phase) = match &active.inner {
            AsyncDmaRequestKind::Read {
                cmd_index, phase, ..
            }
            | AsyncDmaRequestKind::Write {
                cmd_index, phase, ..
            } => (*cmd_index, *phase),
        };

        match self.poll_dma_complete(cmd_index, phase) {
            Ok(PollResult::Pending) => Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index))),
            Ok(PollResult::Complete) => {
                let active = request.take().ok_or(Error::InvalidArgument)?;
                self.finish_async_dma_request(active)?;
                slot.complete(id)
            }
            Err(err) => {
                self.abort_async_dma_request(request, id, slot, phase);
                Err(err)
            }
        }
    }

    pub fn dma_read_blocks<F>(
        &mut self,
        start_block: u32,
        block_count: u32,
        request: IdmacRead<'_, F>,
    ) -> Result<(), Error>
    where
        F: FnMut(*mut u8, usize),
    {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let byte_count = block_count.checked_mul(512).ok_or(Error::InvalidArgument)?;
        let transfer_end = request
            .buffer_dma
            .checked_add(byte_count as u64)
            .ok_or(Error::InvalidArgument)?;
        let desc_bytes = (block_count as usize)
            .checked_mul(IDMAC_DESC_SIZE)
            .ok_or(Error::InvalidArgument)?;
        let desc_end = request
            .desc_dma
            .checked_add(desc_bytes as u64)
            .ok_or(Error::InvalidArgument)?;
        if transfer_end > u32::MAX as u64 + 1
            || desc_end > u32::MAX as u64 + 1
            || request.desc_count < block_count as usize
            || request.desc_virt.is_null()
        {
            return Err(Error::InvalidArgument);
        }

        unsafe {
            let descs = request.desc_virt as *mut IdmacDesc;
            for index in 0..block_count as usize {
                let last = index + 1 == block_count as usize;
                let next = if last {
                    0
                } else {
                    (request.desc_dma as u32) + ((index + 1) * IDMAC_DESC_SIZE) as u32
                };
                descs.add(index).write_volatile(IdmacDesc::chained(
                    (request.buffer_dma as u32) + (index * 512) as u32,
                    512,
                    next,
                    index == 0,
                    last,
                ));
            }
        }
        (request.flush_desc)(request.desc_virt, desc_bytes);

        self.clear_all_int_status();
        self.program_data_phase(512, block_count);
        self.reset_dma()?;

        self.regs.dbaddr().write(request.desc_dma as u32);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.data_irq_enabled)
        });
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);

        self.pending_data = Some(PendingData {
            direction: DataDirection::Read,
            block_size: 512,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        if let Err(err) = self.issue_command(&cmd) {
            self.disable_idmac();
            self.recover_after_idmac_read_error();
            self.clear_all_int_status();
            return Err(err);
        }

        let result = self.wait_dma_read_complete(cmd.cmd);
        if result.is_ok() && block_count > 1 {
            let _ = self.issue_command(&CMD12);
        }
        self.disable_idmac();
        if result.is_err() {
            self.recover_after_idmac_read_error();
        }
        self.clear_all_int_status();
        result
    }

    fn idmac_transfer_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut dma_api::DArray<IdmacDesc>,
    ) -> Result<Response, Error> {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let direction = cmd.data_direction();
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => return Err(Error::InvalidArgument),
        };
        let byte_count = block_count
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(Error::InvalidArgument)?;
        let transfer_end = buffer_dma
            .checked_add(byte_count as u64)
            .ok_or(Error::InvalidArgument)?;
        let desc_bytes = (block_count as usize)
            .checked_mul(IDMAC_DESC_SIZE)
            .ok_or(Error::InvalidArgument)?;
        let desc_dma = desc.dma_addr().as_u64();
        let desc_end = desc_dma
            .checked_add(desc_bytes as u64)
            .ok_or(Error::InvalidArgument)?;
        if transfer_end > u32::MAX as u64 + 1
            || desc_end > u32::MAX as u64 + 1
            || desc.len() < block_count as usize
        {
            return Err(Error::InvalidArgument);
        }

        desc.write_with(block_count as usize, |descs| {
            for (index, desc) in descs.iter_mut().enumerate() {
                let last = index + 1 == block_count as usize;
                let next = if last {
                    0
                } else {
                    (desc_dma as u32) + ((index + 1) * IDMAC_DESC_SIZE) as u32
                };
                *desc = IdmacDesc::chained(
                    (buffer_dma as u32) + (index * BLOCK_SIZE) as u32,
                    BLOCK_SIZE as u32,
                    next,
                    index == 0,
                    last,
                );
            }
        });

        self.clear_all_int_status();
        self.program_data_phase(BLOCK_SIZE as u32, block_count);
        self.reset_dma_for_phase(phase)?;

        self.regs.dbaddr().write(desc_dma as u32);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.data_irq_enabled)
        });
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        let response = match self.issue_command(cmd) {
            Ok(response) => response,
            Err(err) => {
                self.disable_idmac();
                self.recover_after_idmac_error(phase);
                self.clear_all_int_status();
                return Err(err);
            }
        };

        let result = self.wait_dma_complete(cmd.cmd, phase);
        self.disable_idmac();
        if result.is_err() {
            self.recover_after_idmac_error(phase);
        }
        self.clear_all_int_status();
        result.map(|_| response)
    }

    fn build_dma_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<AsyncDmaRequest, Error> {
        let block_count = dma_read_block_count(size)?;
        let map = dma
            .map_single_array(
                unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size.get()) },
                BLOCK_SIZE,
                DmaDirection::FromDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let mut desc = dma
            .array_zero_with_align::<IdmacDesc>(
                block_count as usize,
                IDMAC_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataRead))?;
        let cmd = if block_count == 1 {
            cmd17(start_block)
        } else {
            cmd18(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, map.dma_addr().as_u64(), &mut desc)?;
        Ok(AsyncDmaRequest {
            inner: AsyncDmaRequestKind::Read {
                id,
                map,
                _desc: desc,
                cmd_index: cmd.cmd,
                phase: Phase::DataRead,
                stop_after_complete: block_count > 1,
            },
        })
    }

    fn build_dma_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: &DeviceDma,
        id: RequestId,
    ) -> Result<AsyncDmaRequest, Error> {
        let block_count = dma_write_block_count(size)?;
        let map = dma
            .map_single_array(
                unsafe { core::slice::from_raw_parts(buffer.as_ptr(), size.get()) },
                BLOCK_SIZE,
                DmaDirection::ToDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        map.confirm_write_all();
        let mut desc = dma
            .array_zero_with_align::<IdmacDesc>(
                block_count as usize,
                IDMAC_DESC_ALIGN,
                DmaDirection::ToDevice,
            )
            .map_err(|err| map_dma_error(err, Phase::DataWrite))?;
        let cmd = if block_count == 1 {
            cmd24(start_block)
        } else {
            cmd25(start_block)
        };
        self.submit_idmac_transfer_mapped(&cmd, block_count, map.dma_addr().as_u64(), &mut desc)?;
        Ok(AsyncDmaRequest {
            inner: AsyncDmaRequestKind::Write {
                id,
                _map: map,
                _desc: desc,
                cmd_index: cmd.cmd,
                phase: Phase::DataWrite,
                stop_after_complete: block_count > 1,
            },
        })
    }

    fn submit_idmac_transfer_mapped(
        &mut self,
        cmd: &Command,
        block_count: u32,
        buffer_dma: u64,
        desc: &mut DArray<IdmacDesc>,
    ) -> Result<Response, Error> {
        if block_count == 0 {
            return Err(Error::InvalidArgument);
        }
        let direction = cmd.data_direction();
        let phase = match direction {
            DataDirection::Read => Phase::DataRead,
            DataDirection::Write => Phase::DataWrite,
            DataDirection::None => return Err(Error::InvalidArgument),
        };
        let byte_count = block_count
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(Error::InvalidArgument)?;
        let transfer_end = buffer_dma
            .checked_add(byte_count as u64)
            .ok_or(Error::InvalidArgument)?;
        let desc_bytes = (block_count as usize)
            .checked_mul(IDMAC_DESC_SIZE)
            .ok_or(Error::InvalidArgument)?;
        let desc_dma = desc.dma_addr().as_u64();
        let desc_end = desc_dma
            .checked_add(desc_bytes as u64)
            .ok_or(Error::InvalidArgument)?;
        if transfer_end > u32::MAX as u64 + 1
            || desc_end > u32::MAX as u64 + 1
            || desc.len() < block_count as usize
        {
            return Err(Error::InvalidArgument);
        }

        desc.write_with(block_count as usize, |descs| {
            for (index, desc) in descs.iter_mut().enumerate() {
                let last = index + 1 == block_count as usize;
                let next = if last {
                    0
                } else {
                    (desc_dma as u32) + ((index + 1) * IDMAC_DESC_SIZE) as u32
                };
                *desc = IdmacDesc::chained(
                    (buffer_dma as u32) + (index * BLOCK_SIZE) as u32,
                    BLOCK_SIZE as u32,
                    next,
                    index == 0,
                    last,
                );
            }
        });

        self.clear_all_int_status();
        self.program_data_phase(BLOCK_SIZE as u32, block_count);
        self.reset_dma_for_phase(phase)?;

        self.regs.dbaddr().write(desc_dma as u32);
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(true)
                .with_dma_enable(true)
                .with_int_enable(self.data_irq_enabled)
        });
        self.regs.bmod().write(BMOD_FB | BMOD_DE);
        self.regs.pldmnd().write(1);

        self.pending_data = Some(PendingData {
            direction,
            block_size: BLOCK_SIZE as u32,
            block_count,
        });
        self.data_blocks_remaining = block_count;
        match self.issue_command(cmd) {
            Ok(response) => Ok(response),
            Err(err) => {
                self.disable_idmac();
                self.recover_after_idmac_error(phase);
                self.clear_all_int_status();
                Err(err)
            }
        }
    }

    fn wait_async_dma_request(
        &mut self,
        request: AsyncDmaRequest,
        slot: &mut AsyncRequestSlot,
    ) -> Result<(), Error> {
        let id = request.id();
        let mut request = Some(request);
        loop {
            match self.poll_async_dma_request(&mut request, id, slot) {
                Err(Error::Timeout(_)) => core::hint::spin_loop(),
                result => return result,
            }
        }
    }

    fn finish_async_dma_request(&mut self, request: AsyncDmaRequest) -> Result<(), Error> {
        match request.inner {
            AsyncDmaRequestKind::Read {
                map,
                stop_after_complete,
                phase,
                ..
            } => {
                map.prepare_read_all();
                if stop_after_complete {
                    let _ = self.issue_command(&CMD12);
                }
                self.disable_idmac();
                self.clear_all_int_status();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                if matches!(phase, Phase::DataRead) {
                    self.data_cmd_index = 0;
                }
            }
            AsyncDmaRequestKind::Write {
                stop_after_complete,
                ..
            } => {
                if stop_after_complete {
                    let _ = self.issue_command(&CMD12);
                }
                self.disable_idmac();
                self.clear_all_int_status();
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
            }
        }
        Ok(())
    }

    fn abort_async_dma_request(
        &mut self,
        request: &mut Option<AsyncDmaRequest>,
        id: RequestId,
        slot: &mut AsyncRequestSlot,
        phase: Phase,
    ) {
        let _ = request.take();
        self.disable_idmac();
        self.recover_after_idmac_error(phase);
        self.clear_all_int_status();
        let _ = slot.complete(id);
    }

    fn disable_idmac(&self) {
        self.regs.ctrl().update(|r| {
            r.with_use_internal_dmac(false)
                .with_dma_enable(false)
                .with_int_enable(false)
        });
        self.regs.bmod().write(0);
    }

    fn recover_after_idmac_read_error(&mut self) {
        self.recover_after_idmac_error(Phase::DataRead);
    }

    fn recover_after_idmac_error(&mut self, phase: Phase) {
        let status = self.regs.status().read().into_bits();
        let rintsts = self.regs.rintsts().read();
        warn!(
            "dwmmc: IDMAC {:?} error state rintsts={:#010x} status={:#010x} tcbcnt={} tbbcnt={}",
            phase,
            rintsts.into_bits(),
            status,
            self.regs.tcbcnt().read(),
            self.regs.tbbcnt().read()
        );

        self.regs.ctrl().update(|r| r.with_abort_read_data(true));
        let _ = self.regs.ctrl().read();
        let _ = self.reset_fifo();
        let _ = self.reset_dma();
        self.regs.ctrl().update(|r| r.with_abort_read_data(false));
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
    }

    fn reset_dma(&self) -> Result<(), Error> {
        self.reset_dma_for_phase(Phase::DataRead)
    }

    fn reset_dma_for_phase(&self, phase: Phase) -> Result<(), Error> {
        self.regs.ctrl().update(|r| r.with_dma_reset(true));
        for _ in 0..DMA_POLL_LIMIT {
            if !self.regs.ctrl().read().dma_reset() {
                self.regs.bmod().write(BMOD_SWR);
                for _ in 0..DMA_POLL_LIMIT {
                    if self.regs.bmod().read() & BMOD_SWR == 0 {
                        return Ok(());
                    }
                    core::hint::spin_loop();
                }
                break;
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::new(phase)))
    }

    fn wait_dma_read_complete(&mut self, cmd_index: u8) -> Result<(), Error> {
        self.wait_dma_complete(cmd_index, Phase::DataRead)
    }

    fn wait_dma_complete(&mut self, cmd_index: u8, phase: Phase) -> Result<(), Error> {
        for _ in 0..DMA_POLL_LIMIT {
            match self.poll_dma_complete(cmd_index, phase)? {
                PollResult::Pending => core::hint::spin_loop(),
                PollResult::Complete => return Ok(()),
            }
        }
        Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index)))
    }

    fn poll_dma_complete(&mut self, cmd_index: u8, phase: Phase) -> Result<PollResult, Error> {
        let raw_status = self.take_data_irq_status();
        let rintsts = crate::regs::RIntSts::from_bits(raw_status);
        if rintsts.error() {
            return Err(self.translate_int_error(rintsts, phase, cmd_index));
        }
        if rintsts.data_transfer_over() {
            return Ok(PollResult::Complete);
        }
        Ok(PollResult::Pending)
    }

    fn take_data_irq_status(&mut self) -> u32 {
        let raw_status = self.regs.rintsts().read().into_bits();
        if raw_status != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(raw_status));
        }
        let status = self.irq_pending_status | raw_status;
        self.irq_pending_status &= !(crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_INT_ERROR_MASK);
        status
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollResult {
    Pending,
    Complete,
}

fn dma_read_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    let len = size.get();
    if !len.is_multiple_of(BLOCK_SIZE) {
        return Err(Error::Misaligned);
    }
    let blocks = len / BLOCK_SIZE;
    u32::try_from(blocks).map_err(|_| Error::InvalidArgument)
}

fn dma_write_block_count(size: NonZeroUsize) -> Result<u32, Error> {
    dma_read_block_count(size)
}

fn map_dma_error(err: dma_api::DmaError, phase: Phase) -> Error {
    match err {
        dma_api::DmaError::NoMemory => Error::BusError(ErrorContext::new(phase)),
        dma_api::DmaError::LayoutError(_)
        | dma_api::DmaError::DmaMaskNotMatch { .. }
        | dma_api::DmaError::AlignMismatch { .. }
        | dma_api::DmaError::NullPointer
        | dma_api::DmaError::ZeroSizedBuffer => Error::InvalidArgument,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_descriptor_sets_owned_chained_first_read_buffer() {
        let desc = IdmacDesc::chained(0x1234_5000, 512, 0x2000, true, false);

        assert_eq!(desc.des0, DESC_OWN | DESC_CH | DESC_FS | DESC_DIC);
        assert_eq!(desc.des1, 512);
        assert_eq!(desc.des2, 0x1234_5000);
        assert_eq!(desc.des3, 0x2000);
    }

    #[test]
    fn last_descriptor_sets_last_and_terminates_chain() {
        let desc = IdmacDesc::chained(0x1234_5200, 512, 0, false, true);

        assert_eq!(desc.des0, DESC_OWN | DESC_CH | DESC_LD | DESC_DIC);
        assert_eq!(desc.des1, 512);
        assert_eq!(desc.des2, 0x1234_5200);
        assert_eq!(desc.des3, 0);
    }

    #[test]
    fn dma_read_plan_rejects_non_block_sized_buffers() {
        let size = NonZeroUsize::new(513).unwrap();

        assert_eq!(dma_read_block_count(size), Err(Error::Misaligned));
    }

    #[test]
    fn dma_read_plan_reports_block_count() {
        let size = NonZeroUsize::new(1024).unwrap();

        assert_eq!(dma_read_block_count(size), Ok(2));
    }

    #[test]
    fn dma_write_plan_rejects_non_block_sized_buffers() {
        let size = NonZeroUsize::new(513).unwrap();

        assert_eq!(dma_write_block_count(size), Err(Error::Misaligned));
    }

    #[test]
    fn async_request_slot_rejects_second_request_until_completed() {
        let mut slot = AsyncRequestSlot::default();
        let first = slot.start().unwrap();

        assert_eq!(slot.start(), Err(Error::UnsupportedCommand));
        assert_eq!(
            slot.complete(RequestId::new(usize::from(first) + 1)),
            Err(Error::InvalidArgument)
        );
        assert_eq!(slot.complete(first), Ok(()));
        assert!(slot.start().is_ok());
    }

    #[test]
    fn async_dma_request_can_cross_queue_thread_boundary() {
        fn assert_send<T: Send>() {}

        assert_send::<AsyncDmaRequest>();
        assert_send::<AsyncRequestSlot>();
    }
}
