use core::sync::atomic::{AtomicBool, Ordering};

use ax_lazyinit::OnceLock;
use dma_api::{DeviceDma, DmaDeviceInfo, DmaDomainId, DmaError, DmaOp};

pub type DmaDeviceResolver = fn(DmaDeviceInfo) -> Result<DeviceDma, DmaError>;

static DMA_OP: OnceLock<&'static dyn DmaOp> = OnceLock::new();
static DMA_DEVICE_RESOLVER: OnceLock<DmaDeviceResolver> = OnceLock::new();
static DMA_READY: AtomicBool = AtomicBool::new(false);

/// Installs the OS-owned resolver for device-scoped DMA capabilities.
pub fn install_dma_device_resolver(resolver: DmaDeviceResolver) {
    DMA_DEVICE_RESOLVER.call_once(|| resolver);
}

/// Resolves DMA metadata through the OS backend, retaining its bound domain.
///
/// The direct backend fallback is kept for standalone filesystem tests, which
/// install only a `DmaOp`. It never interprets translated metadata as direct.
pub fn dma_device(info: DmaDeviceInfo) -> Result<DeviceDma, DmaError> {
    if let Some(resolver) = DMA_DEVICE_RESOLVER.get() {
        return resolver(info);
    }
    if !matches!(info.domain(), DmaDomainId::Direct) {
        return Err(DmaError::DomainMismatch {
            requested: info.domain(),
            backend: DmaDomainId::Direct,
        });
    }
    let op = dma_op().ok_or(DmaError::MappingFailed)?;
    if op.domain_id() != DmaDomainId::Direct {
        return Err(DmaError::DomainMismatch {
            requested: info.domain(),
            backend: op.domain_id(),
        });
    }
    Ok(DeviceDma::new(info, op))
}

pub fn install_dma_op(op: &'static dyn DmaOp) {
    DMA_OP.call_once(|| op);
    DMA_READY.store(true, Ordering::Release);
}

pub fn dma_op() -> Option<&'static dyn DmaOp> {
    DMA_OP.get().copied()
}

pub fn has_dma_op() -> bool {
    DMA_READY.load(Ordering::Acquire)
}
