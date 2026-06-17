//! TPU 设备 OS 适配
//!
//! 将 ioctl 命令翻译为 `Sg2002Tpu` 调用，并通过 fd 解析 Ion buffer
//! 物理/虚拟地址。

use alloc::sync::Arc;
use core::ptr::NonNull;

use sg2002_tpu::{
    ion::IonBuffer,
    tpu::{
        Sg2002Tpu,
        error::TpuError,
        types::{
            CVITPU_DMABUF_FLUSH, CVITPU_DMABUF_FLUSH_FD, CVITPU_DMABUF_INVLD,
            CVITPU_DMABUF_INVLD_FD, CVITPU_LOAD_TEE, CVITPU_PIO_MODE, CVITPU_SUBMIT_DMABUF,
            CVITPU_SUBMIT_TEE, CVITPU_UNLOAD_TEE, CVITPU_WAIT_DMABUF, CviCacheOpArg,
            CviSubmitDmaArg, CviWaitDmaArg,
        },
    },
};

use crate::{
    file::{get_file_like, ion::IonBufferFile},
    pseudofs::DeviceOps,
};

/// TPU 字符设备
pub struct TpuDevice {
    /// 硬件层
    hw: Arc<Sg2002Tpu>,
    /// 是否已注册中断
    irq_registered: bool,
}

// From SG2002 TPU DT node:
// interrupts = <0x4b 0x04 0x4c 0x04>;
// interrupt-names = "tiu_irq\0tdma_irq";
// so TDMA uses the second IRQ: 0x4c (76).
const TPU_TDMA_IRQ: usize = 76;

fn register_tpu_irq(hw: &Arc<Sg2002Tpu>) -> bool {
    let data = unsafe { NonNull::new_unchecked(Arc::as_ptr(hw) as *mut ()) };
    if ax_runtime::hal::irq::request_shared_irq(TPU_TDMA_IRQ, tpu_tdma_irq_handler, data).is_err() {
        warn!("[TPU] failed to register tdma irq {}", TPU_TDMA_IRQ);
        return false;
    }
    ax_runtime::hal::irq::set_enable(TPU_TDMA_IRQ, true);
    true
}

unsafe fn tpu_tdma_irq_handler(
    _ctx: ax_runtime::hal::irq::IrqContext,
    data: NonNull<()>,
) -> ax_runtime::hal::irq::IrqReturn {
    let hw = unsafe { &*(data.as_ptr() as *const Sg2002Tpu) };
    info!("[TPU] tdma irq {} received", TPU_TDMA_IRQ);
    if hw.handle_irq() {
        warn!("[TPU] tdma irq {} reports error status", TPU_TDMA_IRQ);
    }
    ax_runtime::hal::irq::IrqReturn::Handled
}

/// 注入给 driver core 的延时函数：按给定微秒数忙等。
///
/// driver core 在等待 TDMA 完成时以此为轮询间隔精确计时（见
/// `Sg2002Tpu::set_delay_fn`），避免空转自旋。submit 全程持有 core 的
/// `SpinNoPreempt` 锁，因此这里只能忙等、不能睡眠让出。
fn tpu_delay(usecs: u64) {
    ax_runtime::hal::time::busy_wait(core::time::Duration::from_micros(usecs));
}

impl TpuDevice {
    /// 创建 TPU 设备（使用默认物理地址）
    ///
    /// # Safety
    /// 调用者必须确保偏移计算后的虚拟地址有效。
    pub unsafe fn new() -> Self {
        let hw = Arc::new(unsafe { Sg2002Tpu::new() });
        hw.set_delay_fn(tpu_delay);
        if let Err(err) = hw.init() {
            warn!("[TPU] init failed: {:?}", err);
        }
        let irq_registered = register_tpu_irq(&hw);
        Self { hw, irq_registered }
    }

    /// 使用指定的虚拟地址创建 TPU 设备
    ///
    /// # Safety
    /// 调用者必须确保虚拟地址有效。
    #[allow(dead_code)]
    pub unsafe fn from_vaddr(tdma_vaddr: *mut u8, tiu_vaddr: *mut u8) -> Self {
        let hw = Arc::new(unsafe { Sg2002Tpu::from_vaddr(tdma_vaddr, tiu_vaddr) });
        hw.set_delay_fn(tpu_delay);
        if let Err(err) = hw.init() {
            warn!("[TPU] init failed: {:?}", err);
        }
        let irq_registered = register_tpu_irq(&hw);
        Self { hw, irq_registered }
    }

    /// 提交 DMA buffer 任务
    fn submit_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        // 从用户空间读取参数
        let submit_arg = unsafe { &*(arg as *const CviSubmitDmaArg) };

        debug!(
            "[TPU] submit dmabuf: fd={}, seq_no={}",
            submit_arg.fd, submit_arg.seq_no
        );
        if !self.irq_registered {
            warn!(
                "[TPU] tdma irq {} not registered, execution may timeout",
                TPU_TDMA_IRQ
            );
        }

        // 从文件描述符获取 IonBufferFile
        let fd = submit_arg.fd;
        let file = get_file_like(fd).map_err(|_| {
            error!("[TPU] Failed to get file for fd={}", fd);
            TpuError::InvalidDmabuf
        })?;

        // 尝试转换为 IonBufferFile (使用 downcast_arc)
        let ion_file: Arc<IonBufferFile> = file.downcast_arc::<IonBufferFile>().map_err(|_| {
            error!("[TPU] fd={} is not an IonBufferFile", fd);
            TpuError::InvalidDmabuf
        })?;

        // 获取底层 Ion buffer（fd 持有强引用，保证生命周期）
        let buffer = ion_file.buffer();
        debug!(
            "[TPU] dmabuf info: handle={}, size={}, paddr=0x{:x}",
            buffer.handle.as_u32(),
            buffer.size,
            buffer.dma_info.bus_addr.as_u64()
        );

        let dmabuf_vaddr = buffer.dma_info.cpu_addr.as_ptr() as usize;
        let dmabuf_paddr = buffer.dma_info.bus_addr.as_u64();

        let (irq_before, poll_before) = self.hw.irq_stats();

        self.hw
            .submit_dmabuf(submit_arg.fd, submit_arg.seq_no, dmabuf_vaddr, dmabuf_paddr)?;

        let (irq_hits, poll_hits) = self.hw.irq_stats();
        let irq_delta = irq_hits.saturating_sub(irq_before);
        let poll_delta = poll_hits.saturating_sub(poll_before);
        info!(
            "[TPU] irq stats after submit: delta_irq={}, delta_poll_fallback={}, total_irq={}, \
             total_poll_fallback={}",
            irq_delta, poll_delta, irq_hits, poll_hits
        );

        Ok(0)
    }

    /// 等待 DMA buffer 完成
    fn wait_dmabuf(&self, arg: usize) -> Result<usize, TpuError> {
        let wait_arg = unsafe { &mut *(arg as *mut CviWaitDmaArg) };

        match self.hw.wait_dmabuf(wait_arg.seq_no) {
            Ok(ret) => {
                wait_arg.ret = ret;
                Ok(0)
            }
            Err(e) => {
                wait_arg.ret = -1;
                Err(e)
            }
        }
    }

    /// 刷新 DMA buffer 缓存 (通过物理地址)
    fn cache_flush(&self, arg: usize) -> Result<usize, TpuError> {
        let flush_arg = unsafe { &*(arg as *const CviCacheOpArg) };
        self.hw.cache_flush_paddr(flush_arg.paddr, flush_arg.size)?;
        Ok(0)
    }

    /// 无效化 DMA buffer 缓存 (通过物理地址)
    fn cache_invalidate(&self, arg: usize) -> Result<usize, TpuError> {
        let invalidate_arg = unsafe { &*(arg as *const CviCacheOpArg) };
        self.hw
            .cache_invalidate_paddr(invalidate_arg.paddr, invalidate_arg.size)?;
        Ok(0)
    }

    /// 刷新 DMA buffer 缓存 (通过 fd)
    fn dmabuf_flush_fd(&self, arg: usize) -> Result<usize, TpuError> {
        let fd = arg as i32;
        debug!("TPU dmabuf flush fd: {}", fd);
        let buffer = self.lookup_ion_buffer(fd)?;
        let paddr = buffer.dma_info.bus_addr.as_u64();
        let size = buffer.size as u64;
        self.hw.cache_flush_paddr(paddr, size)?;
        debug!("Flushed buffer: paddr=0x{:x}, size={}", paddr, size);
        Ok(0)
    }

    /// 无效化 DMA buffer 缓存 (通过 fd)
    fn dmabuf_invld_fd(&self, arg: usize) -> Result<usize, TpuError> {
        let fd = arg as i32;
        debug!("TPU dmabuf invalidate fd: {}", fd);
        let buffer = self.lookup_ion_buffer(fd)?;
        let paddr = buffer.dma_info.bus_addr.as_u64();
        let size = buffer.size as u64;
        self.hw.cache_invalidate_paddr(paddr, size)?;
        Ok(0)
    }

    /// 把用户传入的 fd 解析为底层 [`sg2002_tpu::ion::IonBuffer`]。
    ///
    /// fd（由 `add_file_like` 分配的文件描述符）与 Ion 内部 handle（来自
    /// `IonHandle` 的全局递增计数）属于两个独立的编号空间，不能直接互相替代。
    /// 因此这里走和 `submit_dmabuf` 一致的路径：fd → `IonBufferFile` →
    /// 持有的 `Arc<IonBuffer>`。
    fn lookup_ion_buffer(&self, fd: i32) -> Result<Arc<IonBuffer>, TpuError> {
        let file = get_file_like(fd).map_err(|err| {
            error!("[TPU] failed to get file for fd={}: {:?}", fd, err);
            TpuError::InvalidDmabuf
        })?;
        let ion_file: Arc<IonBufferFile> = file.downcast_arc::<IonBufferFile>().map_err(|_| {
            error!("[TPU] fd={} is not an IonBufferFile", fd);
            TpuError::InvalidDmabuf
        })?;
        Ok(ion_file.buffer().clone())
    }
}

impl DeviceOps for TpuDevice {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> axfs_ng_vfs::VfsResult<usize> {
        Ok(0)
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> axfs_ng_vfs::VfsResult<usize> {
        debug!("TPU ioctl: cmd=0x{:x}, arg=0x{:x}", cmd, arg);

        let result = match cmd {
            CVITPU_SUBMIT_DMABUF => self.submit_dmabuf(arg),
            CVITPU_DMABUF_FLUSH_FD => self.dmabuf_flush_fd(arg),
            CVITPU_DMABUF_INVLD_FD => self.dmabuf_invld_fd(arg),
            CVITPU_DMABUF_FLUSH => self.cache_flush(arg),
            CVITPU_DMABUF_INVLD => self.cache_invalidate(arg),
            CVITPU_WAIT_DMABUF => self.wait_dmabuf(arg),
            CVITPU_PIO_MODE => {
                warn!("TPU PIO mode not implemented");
                Ok(0)
            }
            CVITPU_LOAD_TEE | CVITPU_SUBMIT_TEE | CVITPU_UNLOAD_TEE => {
                warn!("TPU TEE operations not supported");
                Err(TpuError::NotInitialized)
            }
            _ => {
                warn!("Unknown TPU ioctl command: 0x{:x}", cmd);
                Err(TpuError::NotInitialized)
            }
        };

        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                error!("TPU ioctl error: {:?}", e);
                Err(ax_errno::AxError::Unsupported)
            }
        }
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}
