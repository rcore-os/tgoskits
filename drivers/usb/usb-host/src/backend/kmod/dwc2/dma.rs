use core::ptr::NonNull;

use dma_api::{ContiguousArray, DmaDirection};
use mbarrier::mb;
use tock_registers::{LocalRegisterCopy, register_bitfields};
use usb_if::{endpoint::TransferBuffer, err::TransferError, transfer::Direction};

use crate::backend::kmod::{
    Kernel,
    dwc2::{DWC2_DMA_ALIGN, stats::Dwc2Stats},
};

// 描述符状态字位字段
register_bitfields![u32,
    pub DMA_DESCRIPTOR [
        /// HOST_DMA_A（BIT31）：活动位。CPU 置位；硬件完成传输后改写整个状态字。
        A OFFSET(31) NUMBITS(1) [],
        /// HOST_DMA_STS（bits 28-29）：传输状态。hw.h 仅定义 PKTERR；
        /// XFERERR/BABBLE 见 Synopsys databook，本驱动不读取。
        STS OFFSET(28) NUMBITS(2) [
            /// 包错误（HOST_DMA_STS_PKTERR）。
            PKTERR = 1,
        ],
        /// HOST_DMA_EOL（BIT26）：链尾指示。
        EOL OFFSET(26) NUMBITS(1) [],
        /// HOST_DMA_IOC（BIT25）：完成中断，链尾/批尾置位。
        IOC OFFSET(25) NUMBITS(1) [],
        /// HOST_DMA_SUP（BIT24）：SETUP 对齐（仅 SETUP 阶段，见 active_setup）。
        SUP OFFSET(24) NUMBITS(1) [],
        /// HOST_DMA_ALT_QTD（BIT23）：备用 QTD 指示（非 ISO 队列用，本驱动不读取）。
        ALT_QTD OFFSET(23) NUMBITS(1) [],
        /// HOST_DMA_QTD_OFFSET（bits 17-22）：QTD 偏移（非 ISO 队列用，本驱动不读取）。
        QTD_OFFSET OFFSET(17) NUMBITS(6) [],
        /// HOST_DMA_ISOC_NBYTES（bits 0-11）：ISO 硬件写回的 12 位剩余字节。
        /// 与 NBYTES 同基址、宽度更窄。
        ISOC_NBYTES OFFSET(0) NUMBITS(12) [],
        /// HOST_DMA_NBYTES（bits 0-16，SHIFT=0）：硬件写回的剩余字节数。
        /// 非 ISO 用 17 位；读 ISO 值也无害（12 位值落在低 12 位）。
        NBYTES OFFSET(0) NUMBITS(17) [],
    ]
];

/// DWC2 DMA 描述符 — 8 字节，硬件按此格式读写。
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct DmaDescriptor {
    pub(crate) status: LocalRegisterCopy<u32, DMA_DESCRIPTOR::Register>,
    pub(crate) paddr: u32, // DMA 传输缓冲区地址（物理地址）
}

/// desc 编程的 NBYTES 值（与结算镜像共用，Linux `qh->n_bytes[]` 语义）：
/// IN 向上取整到 mps 整数倍（0 长 → 1 个包）；OUT 原值。两侧必须一致。
pub(crate) fn initial_len(len: usize, mps: u32, is_in: bool) -> usize {
    let cap = (DmaDescriptor::NBYTES_LIMIT - mps.saturating_sub(1)) as usize;
    let len = len.min(cap);
    if is_in {
        if len > 0 && mps > 0 {
            len.div_ceil(mps as usize) * mps as usize
        } else if mps > 0 {
            mps as usize
        } else {
            0
        }
    } else {
        len
    }
}

impl DmaDescriptor {
    pub(crate) const SIZE: usize = 8;
    pub(crate) const NBYTES_LIMIT: u32 = 0x1FFFF;
    /// ISO 描述符的 12 位 NBYTES 上限（HOST_DMA_ISOC_NBYTES）。
    pub(crate) const ISO_NBYTES_LIMIT: u32 = 0xfff;

    /// ISO 传输：每个 interval 一个描述符，环形列表、不设 EOL。
    /// NBYTES 截断到 12 位 ISO 上限（HOST_DMA_ISOC_NBYTES），0 长 → 1 字节。
    pub(crate) fn new_iso(paddr: u32, len: u32, last: bool) -> Self {
        let nbytes = if len == 0 {
            1
        } else {
            len.min(Self::ISO_NBYTES_LIMIT)
        };
        let mut status = LocalRegisterCopy::new(0);
        status.write(DMA_DESCRIPTOR::NBYTES.val(nbytes) + DMA_DESCRIPTOR::A::SET);

        if last {
            status.modify(DMA_DESCRIPTOR::IOC::SET);
        }

        Self { status, paddr }
    }

    /// IN 传输：NBYTES 向上取整到 mps 整数倍（0 长 → 1 个包），不拆包且不
    /// 溢出 17 位。链尾（`last`）置 IOC + EOL。
    pub(crate) fn new_in(paddr: u32, len: u32, mps: u32, last: bool) -> Self {
        let len = initial_len(len as usize, mps, true) as u32;
        let mut status = LocalRegisterCopy::new(0);
        status.write(DMA_DESCRIPTOR::NBYTES.val(len) + DMA_DESCRIPTOR::A::SET);
        if last {
            status.modify(DMA_DESCRIPTOR::IOC::SET + DMA_DESCRIPTOR::EOL::SET);
        }

        Self { status, paddr }
    }

    /// OUT 传输：不需要整包对齐，只截断到字段上限。链尾（`last`）置 IOC + EOL。
    pub(crate) fn new_out(paddr: u32, len: u32, mps: u32, last: bool) -> Self {
        let len = initial_len(len as usize, mps, false) as u32;
        let mut status = LocalRegisterCopy::new(0);
        status.write(DMA_DESCRIPTOR::NBYTES.val(len) + DMA_DESCRIPTOR::A::SET);
        if last {
            status.modify(DMA_DESCRIPTOR::IOC::SET + DMA_DESCRIPTOR::EOL::SET);
        }

        Self { status, paddr }
    }

    /// SETUP 传输：置 SUP，单描述符 A|IOC|EOL
    pub(crate) fn new_setup(paddr: u32, len: u32) -> Self {
        let mut status = LocalRegisterCopy::new(0);
        status.write(
            DMA_DESCRIPTOR::NBYTES.val(len)
                + DMA_DESCRIPTOR::A::SET
                + DMA_DESCRIPTOR::SUP::SET
                + DMA_DESCRIPTOR::IOC::SET
                + DMA_DESCRIPTOR::EOL::SET,
        );

        Self { status, paddr }
    }

    /// 硬件写回的剩余字节数（非 ISO 用 17 位，ISO 用 12 位）。
    pub(crate) fn remaining(&self) -> u32 {
        self.status.read(DMA_DESCRIPTOR::NBYTES)
    }
    pub(crate) fn iso_remaining(&self) -> u32 {
        self.status.read(DMA_DESCRIPTOR::ISOC_NBYTES)
    }

    /// 描述符是否仍为活动（A 位由 CPU 置位，硬件服务后随状态字回写清零）。
    pub(crate) fn is_active(&self) -> bool {
        self.status.read(DMA_DESCRIPTOR::A) != 0
    }

    /// ISO 描述符是否被硬件标记为包错误（HOST_DMA_STS_PKTERR，即 XactErr /
    /// 无法在调度窗口内完成全部事务）。
    pub(crate) fn iso_status_error(&self) -> bool {
        self.status.read_as_enum(DMA_DESCRIPTOR::STS) == Some(DMA_DESCRIPTOR::STS::Value::PKTERR)
    }
}

/// DWC2 DMA 描述符数组，连续分配、对齐、可 flush。
pub(crate) struct DmaDescriptors {
    pub(crate) data: ContiguousArray<u8>,
    pub(crate) dma_addr: u32, // data 字段的物理地址
}

impl DmaDescriptors {
    pub(crate) fn new(kernel: &Kernel, n: usize, align: usize) -> Result<Self, anyhow::Error> {
        let data = kernel
            .contiguous_array_zero_with_align::<u8>(
                n * DmaDescriptor::SIZE,
                align,
                DmaDirection::Bidirectional,
            )
            .map_err(|err| anyhow!("DWC2 desc array alloc: {err}"))?;

        let dma_addr = u32::try_from(data.dma_addr().as_u64())
            .map_err(|_| anyhow!("DWC2 desc array DMA above 32-bit mask"))?;
        Ok(Self { data, dma_addr })
    }

    pub(crate) fn write_descs(&self, start: usize, descs: &[DmaDescriptor]) {
        let ptr = (self.data.as_ptr().as_ptr() as usize + start * DmaDescriptor::SIZE) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(
                descs.as_ptr() as *const u8,
                ptr,
                descs.len() * DmaDescriptor::SIZE,
            )
        }
        let byte_start = start * DmaDescriptor::SIZE;
        let byte_end = byte_start + descs.len() * DmaDescriptor::SIZE;
        self.data.prepare_for_device(byte_start..byte_end);
        mb();
    }

    /// 读回 `start` 起的 `count` 个 desc（先 invalidate 整段再读，硬件可能
    /// 已回写状态字）。返回的切片在下次 sync 前有效。
    pub(crate) fn read_descs(&self, start: usize, count: usize) -> &[DmaDescriptor] {
        let byte_start = start * DmaDescriptor::SIZE;
        let byte_end = byte_start + count * DmaDescriptor::SIZE;
        self.data.complete_for_cpu(byte_start..byte_end);
        let base = self.data.as_ptr().as_ptr() as *const DmaDescriptor;
        unsafe { core::slice::from_raw_parts(base.add(start), count) }
    }

    /// 清空第 `index` 个 desc（status=0、buf=0，A 位清零）并 flush 落盘
    pub(crate) fn clear(&self, index: usize) {
        let ptr = (self.data.as_ptr().as_ptr() as usize + index * DmaDescriptor::SIZE) as *mut u64;
        unsafe { core::ptr::write_volatile(ptr, 0u64) }

        let byte_start = index * DmaDescriptor::SIZE;
        self.data
            .prepare_for_device(byte_start..byte_start + DmaDescriptor::SIZE);
    }

    /// 清空全部 desc（A=0）并 flush 落盘
    pub(crate) fn clear_all(&self) {
        let bytes = self.data.bytes_len();
        unsafe { core::ptr::write_bytes(self.data.as_ptr().as_ptr(), 0, bytes) }

        self.data.prepare_for_device(0..bytes);
    }

    /// DMA 地址（物理地址），用于编程 DMA 控制寄存器。
    pub(crate) fn dma_addr(&self) -> u32 {
        self.dma_addr
    }
}

#[derive(Default)]
pub(crate) struct Dwc2DmaBufferPool {
    cached: Option<ContiguousArray<u8>>,
}

impl Dwc2DmaBufferPool {
    pub(crate) fn take(
        &mut self,
        kernel: &Kernel,
        len: usize,
        stats: &Dwc2Stats,
    ) -> Result<Option<ContiguousArray<u8>>, TransferError> {
        let len = len.max(1);
        if self
            .cached
            .as_ref()
            .is_some_and(|buffer| buffer.len() >= len)
        {
            return Ok(self.cached.take());
        }

        let buffer = kernel
            .contiguous_array_zero_with_align::<u8>(
                len,
                DWC2_DMA_ALIGN,
                DmaDirection::Bidirectional,
            )
            .map_err(|err| {
                TransferError::Other(anyhow!("DWC2 coherent DMA allocation failed: {err}"))
            })?;
        stats.record_dma_alloc();
        Ok(Some(buffer))
    }

    pub(crate) fn reclaim(&mut self, buffer: Dwc2DmaBuffer) {
        if let Some(coherent) = buffer.coherent {
            self.cached = Some(coherent);
        }
    }
}

pub(crate) struct Dwc2DmaBuffer {
    direction: Direction,
    request_buffer: Option<(NonNull<u8>, usize)>,
    coherent: Option<ContiguousArray<u8>>,
    len: usize,
}

impl Dwc2DmaBuffer {
    /// 构建传输的 DMA 缓冲：分配 `alloc_len` 字节 coherent 内存，OUT 拷贝
    /// 调用方数据、IN 清零，记录调用方缓冲供完成时拷回。
    ///
    /// `alloc_len` 为分配长度，由调用方决定：non-ISO IN 需按 `initial_len`
    /// 取整（与 desc NBYTES 取整镜像，防 DMA 越界），其余为请求数据长度；
    /// ISO 为各包长度精确之和（desc 按精确长度编程）。OUT 时须保证
    /// `alloc_len >= buffer.len`（拷贝数据长度取调用方缓冲长度）。
    /// `buffer` 为 None（零长请求）时仍分配但无调用方缓冲。
    pub(crate) fn new(
        kernel: &Kernel,
        pool: &mut Dwc2DmaBufferPool,
        stats: &Dwc2Stats,
        buffer: Option<TransferBuffer>,
        direction: Direction,
        alloc_len: usize,
    ) -> Result<Self, TransferError> {
        let request_buffer = buffer
            .filter(|buffer| buffer.len > 0)
            .map(|buffer| (buffer.ptr, buffer.len));
        let len = request_buffer.map_or(0, |(_, len)| len);
        let alloc_len = alloc_len.max(1);
        let coherent = {
            let mut coherent = pool.take(kernel, alloc_len, stats)?.ok_or_else(|| {
                TransferError::Other(anyhow!("DWC2 DMA buffer pool returned no buffer"))
            })?;
            if let Some((ptr, len)) = request_buffer {
                match direction {
                    Direction::Out => {
                        let data =
                            unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) };
                        coherent.write_with_cpu(len, |dst| dst.copy_from_slice(data));
                        stats.record_bounce_to_device(len);
                    }
                    Direction::In => {
                        coherent.write_with_cpu(alloc_len, |dst| dst.fill(0));
                        stats.record_bounce_from_device(len);
                    }
                }
            } else if matches!(direction, Direction::In) {
                coherent.write_with_cpu(alloc_len, |dst| dst.fill(0));
            }
            // CPU 写（OUT 载荷 / IN 清零）flush 到内存后才能交给 DMA 侧：
            // IN 短包未覆盖区域在完成时 invalidate 后必须已是内存中的 0。
            coherent.prepare_for_device(0..alloc_len);
            Some(coherent)
        };

        Ok(Self {
            direction,
            request_buffer,
            coherent,
            len,
        })
    }

    pub(crate) fn buffer_len(&self) -> usize {
        self.len
    }

    pub(crate) fn dma_addr(&self) -> u64 {
        self.coherent
            .as_ref()
            .map_or(0, |buffer| buffer.dma_addr().as_u64())
    }

    pub(crate) fn copy_in_to_request(&self, actual: usize) -> Result<(), TransferError> {
        if !matches!(self.direction, Direction::In) || actual == 0 {
            return Ok(());
        }
        let Some((ptr, len)) = self.request_buffer else {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer completed without a request buffer"
            )));
        };
        let Some(coherent) = self.coherent.as_ref() else {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer completed without a DMA buffer"
            )));
        };
        if actual > len || actual > coherent.len() {
            return Err(TransferError::Other(anyhow!(
                "DWC2 IN transfer actual length {actual} exceeds buffer len {len}"
            )));
        }

        // Cache 纪律（IN，DMA → CPU）：先 invalidate 丢弃旧值、从内存拿到
        // DMA 写入的数据，再拷贝回调用方 buffer。漏掉会读到陈旧数据。
        coherent.complete_for_cpu(0..actual);
        let dst = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), actual) };
        coherent.read_with_cpu(actual, |src| dst.copy_from_slice(src));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    const A: u32 = 1 << 31;
    const SUP: u32 = 1 << 24;
    const IOC: u32 = 1 << 25;
    const EOL: u32 = 1 << 26;
    const NBYTES_MASK: u32 = 0x1FFFF;

    fn status(d: &DmaDescriptor) -> u32 {
        d.status.get()
    }

    #[test]
    fn initial_len_rounds_in_to_packet_boundary() {
        assert_eq!(initial_len(100, 64, true), 128);
        assert_eq!(initial_len(0, 64, true), 64);
        assert_eq!(initial_len(512, 64, true), 512);
        assert_eq!(initial_len(0, 0, true), 0);
    }

    #[test]
    fn initial_len_passes_out_through_and_caps() {
        assert_eq!(initial_len(100, 64, false), 100);
        assert_eq!(initial_len(0, 64, false), 0);
        // 17 位上限：cap = 0x1FFFF - (mps - 1)，IN 取整后不溢出。
        assert_eq!(initial_len(0x1FFFF, 64, true), 0x1FFC0);
        assert_eq!(initial_len(0x1FFFF, 64, false), 0x1FFC0);
    }

    #[test]
    fn setup_desc_sets_a_sup_ioc_eol() {
        let d = DmaDescriptor::new_setup(0x1000, 8);
        assert_eq!(status(&d) & NBYTES_MASK, 8);
        assert_eq!(status(&d) & (A | SUP | IOC | EOL), A | SUP | IOC | EOL);
    }

    #[test]
    fn in_desc_rounds_and_marks_chain_tail() {
        let tail = DmaDescriptor::new_in(0x1000, 100, 64, true);
        assert_eq!(status(&tail) & NBYTES_MASK, 128);
        assert_eq!(status(&tail) & (IOC | EOL), IOC | EOL);

        let head = DmaDescriptor::new_in(0x1000, 100, 64, false);
        assert_eq!(status(&head) & (IOC | EOL), 0);
        assert_eq!(status(&head) & A, A);
    }

    #[test]
    fn out_desc_caps_nbytes() {
        let d = DmaDescriptor::new_out(0x1000, 0x1FFFF, 64, true);
        assert_eq!(status(&d) & NBYTES_MASK, 0x1FFC0);
        assert_eq!(status(&d) & (IOC | EOL), IOC | EOL);
    }

    #[test]
    fn iso_desc_sets_a_ioc_and_caps_nbytes() {
        let last = DmaDescriptor::new_iso(0x1000, 512, true);
        assert_eq!(status(&last) & NBYTES_MASK, 512);
        assert_eq!(status(&last) & (IOC | EOL), IOC);
        assert_eq!(status(&last) & A, A);

        // 0 长 → 1 字节；超过 12 位 ISO 上限截断。
        let zero = DmaDescriptor::new_iso(0x1000, 0, false);
        assert_eq!(status(&zero) & NBYTES_MASK, 1);
        let big = DmaDescriptor::new_iso(0x1000, DmaDescriptor::ISO_NBYTES_LIMIT + 100, false);
        assert_eq!(status(&big) & NBYTES_MASK, DmaDescriptor::ISO_NBYTES_LIMIT);
        // 非链尾不置 IOC。
        assert_eq!(status(&big) & IOC, 0);
    }

    #[test]
    fn iso_remaining_and_pkterr_readback() {
        let mut d = DmaDescriptor::new_iso(0x1000, 512, true);
        assert!(!d.iso_status_error());
        d.status.modify(DMA_DESCRIPTOR::ISOC_NBYTES.val(64));
        assert_eq!(d.iso_remaining(), 64);
        d.status.modify(DMA_DESCRIPTOR::STS.val(1));
        assert!(d.iso_status_error());
    }

    #[test]
    fn iso_desc_a_bit_readback() {
        let mut d = DmaDescriptor::new_iso(0x1000, 512, true);
        assert!(d.is_active());
        // 硬件服务后回写状态字会清掉 A 位（Linux DDMA 结算同款语义）。
        d.status.modify(DMA_DESCRIPTOR::A::CLEAR);
        assert!(!d.is_active());
    }

    #[test]
    fn remaining_reads_written_back_nbytes() {
        let mut d = DmaDescriptor::new_in(0x1000, 100, 64, true);
        d.status.modify(DMA_DESCRIPTOR::NBYTES.val(28));
        assert_eq!(d.remaining(), 28);
    }

    #[test]
    fn dma_buffer_bounces_in_data_through_coherent_dma_memory() {
        let kernel = crate::backend::kmod::dwc2::testutil::test_kernel();
        let mut pool = Dwc2DmaBufferPool::default();
        let stats = Dwc2Stats::new();
        let mut data = [0u8; 4];
        let buffer = Some(TransferBuffer::from_mut_slice(&mut data).unwrap());
        let dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, buffer, Direction::In, 4)
            .expect("IN bounce buffer builds");

        assert_eq!(dma.buffer_len(), 4);
        assert_ne!(dma.dma_addr(), data.as_ptr() as u64);
        let mut dma = dma;
        dma.coherent
            .as_mut()
            .unwrap()
            .write_with_cpu(4, |dst| dst.copy_from_slice(&[1, 2, 3, 4]));
        dma.copy_in_to_request(3).expect("partial completion copy");

        assert_eq!(data, [1, 2, 3, 0]);
    }

    #[test]
    fn dma_buffer_pool_reuses_existing_dma_allocation() {
        let kernel = crate::backend::kmod::dwc2::testutil::test_kernel();
        let stats = Dwc2Stats::new();
        let mut pool = Dwc2DmaBufferPool::default();
        let mut first = [0u8; 64];
        let first_buffer = Some(TransferBuffer::from_mut_slice(&mut first).unwrap());
        let first_dma =
            Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, first_buffer, Direction::In, 64)
                .expect("first buffer builds");
        let first_addr = first_dma.dma_addr();
        pool.reclaim(first_dma);

        let mut smaller = [0u8; 32];
        let smaller_buffer = Some(TransferBuffer::from_mut_slice(&mut smaller).unwrap());
        let smaller_dma = Dwc2DmaBuffer::new(
            &kernel,
            &mut pool,
            &stats,
            smaller_buffer,
            Direction::In,
            32,
        )
        .expect("smaller buffer reuses cache");
        assert_eq!(smaller_dma.dma_addr(), first_addr);
        pool.reclaim(smaller_dma);

        let mut larger = [0u8; 128];
        let larger_buffer = Some(TransferBuffer::from_mut_slice(&mut larger).unwrap());
        let larger_dma = Dwc2DmaBuffer::new(
            &kernel,
            &mut pool,
            &stats,
            larger_buffer,
            Direction::In,
            128,
        )
        .expect("larger buffer allocates");
        assert_eq!(larger_dma.buffer_len(), 128);
        pool.reclaim(larger_dma);

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.dma_allocs, 2);
        assert_eq!(snapshot.bounce_from_device_bytes, 64 + 32 + 128);
    }

    #[test]
    fn out_dma_buffer_copies_request_data_for_device() {
        let kernel = crate::backend::kmod::dwc2::testutil::test_kernel();
        let mut pool = Dwc2DmaBufferPool::default();
        let stats = Dwc2Stats::new();
        let data = [1u8, 2, 3, 4];
        let buffer = Some(TransferBuffer::from_slice(&data).unwrap());
        let dma = Dwc2DmaBuffer::new(&kernel, &mut pool, &stats, buffer, Direction::Out, 4)
            .expect("OUT bounce buffer builds");

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.bounce_to_device_bytes, 4);
        dma.coherent
            .as_ref()
            .unwrap()
            .read_with_cpu(4, |src| assert_eq!(src, &[1, 2, 3, 4]));
    }
}
