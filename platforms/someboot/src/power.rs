use core::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    ArchTrait, DCacheOp,
    arch::Arch,
    mem::{__kimage_va, cpu_area_phys_to_virt, dcache_range, virt_to_phys},
    smp::PerCpuMeta,
};

const CPU_ALIVE_TIMEOUT_SECONDS: usize = 10;
static CPU_BOOT_LOCK: AtomicBool = AtomicBool::new(false);

pub fn shutdown() -> ! {
    crate::arch::Arch::shutdown()
}

pub fn reset() -> ! {
    crate::arch::Arch::reset()
}

pub fn cpu_on(cpu_idx: usize) -> Result<(), CpuOnError> {
    let _guard = CpuBootGuard::lock();
    let entry = secondary_entry_addr();
    debug!("Secondary entry address: {entry:#x}");
    let arg = crate::smp::cpu_meta_addr(cpu_idx).ok_or(CpuOnError::InvalidParameters)?;
    debug!("Secondary entry argument (cpu meta address): {arg:#x}");

    let meta = unsafe { &*(cpu_area_phys_to_virt(arg) as *const PerCpuMeta) };

    debug!("Power on CPU {meta:#x?}");
    let kimg = crate::mem::kimage_range();
    let kimg_start = __kimage_va(kimg.start);
    let size = kimg.end - kimg.start;
    dcache_range(DCacheOp::Clean, kimg_start, size);

    crate::smp::prepare_secondary_boot(cpu_idx).map_err(|error| {
        CpuOnError::Other(anyhow::anyhow!(
            "cannot kick logical CPU {cpu_idx} (hardware ID {:#x}): {error}",
            meta.cpu_id
        ))
    })?;
    Arch::kick_secondary_cpu(meta.cpu_id, entry, arg)?;
    wait_for_secondary_alive(cpu_idx, meta.cpu_id)
}

fn wait_for_secondary_alive(cpu_idx: usize, hardware_id: usize) -> Result<(), CpuOnError> {
    let start = Arch::systimer_tick();
    let timeout_ticks = Arch::systimer_freq().saturating_mul(CPU_ALIVE_TIMEOUT_SECONDS);

    loop {
        if crate::smp::try_release_secondary(cpu_idx) {
            return Ok(());
        }
        if Arch::systimer_tick().wrapping_sub(start) >= timeout_ticks {
            return Err(CpuOnError::AliveTimeout {
                logical_cpu: cpu_idx,
                hardware_id,
            });
        }
        spin_loop();
    }
}

struct CpuBootGuard;

impl CpuBootGuard {
    fn lock() -> Self {
        while CPU_BOOT_LOCK
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        Self
    }
}

impl Drop for CpuBootGuard {
    fn drop(&mut self) {
        CPU_BOOT_LOCK.store(false, Ordering::Release);
    }
}

/// secondary entry address
/// arg0 is stack top
fn secondary_entry_addr() -> usize {
    let ptr = crate::arch::Arch::secondary_entry_fn_address() as *const u8;
    virt_to_phys(ptr)
}

#[derive(thiserror::Error, Debug)]
pub enum CpuOnError {
    #[error("CPU on is not supported")]
    NotSupported,
    #[error("CPU is already on")]
    AlreadyOn,
    #[error("Invalid parameters")]
    InvalidParameters,
    #[error(
        "logical CPU {logical_cpu} (hardware ID {hardware_id:#x}) did not reach the alive \
         synchronization point"
    )]
    AliveTimeout {
        logical_cpu: usize,
        hardware_id: usize,
    },
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}
