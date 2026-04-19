use ax_errno::AxResult;
use axvcpu::AxArchPerCpu;

use crate::registers::{
    CSR_EENTRY, csr_read, csr_write, gcsr_eentry_read, gcsr_eentry_write, gstat_read, gstat_write,
};

#[cfg(target_arch = "loongarch64")]
unsafe extern "C" {
    static _exception_vectors: u8;
}

#[cfg(not(target_arch = "loongarch64"))]
#[unsafe(no_mangle)]
static _exception_vectors: u8 = 0;

#[repr(C)]
#[repr(align(4096))]
pub struct LoongArchPerCpu {
    pub cpu_id: usize,
    pub original_eentry: usize,
    pub original_gstat: usize,
    pub original_gcsr_eentry: usize,
    pub enabled: bool,
}

impl AxArchPerCpu for LoongArchPerCpu {
    fn new(cpu_id: usize) -> AxResult<Self> {
        Ok(Self {
            cpu_id,
            original_eentry: 0,
            original_gstat: 0,
            original_gcsr_eentry: 0,
            enabled: false,
        })
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn hardware_enable(&mut self) -> AxResult {
        self.original_eentry = unsafe { csr_read::<CSR_EENTRY>() };
        self.original_gstat = gstat_read();
        self.original_gcsr_eentry = gcsr_eentry_read();

        unsafe {
            gcsr_eentry_write(core::ptr::addr_of!(_exception_vectors) as usize);
        }
        self.enabled = true;

        log::debug!(
            "LoongArch virtualization enabled for CPU {}, GCSR_EENTRY={:#x}",
            self.cpu_id,
            core::ptr::addr_of!(_exception_vectors) as usize
        );
        Ok(())
    }

    fn hardware_disable(&mut self) -> AxResult {
        unsafe {
            gstat_write(self.original_gstat);
            gcsr_eentry_write(self.original_gcsr_eentry);
            csr_write::<CSR_EENTRY>(self.original_eentry);
        }
        self.enabled = false;

        log::debug!("LoongArch virtualization disabled for CPU {}", self.cpu_id);
        Ok(())
    }

    fn max_guest_page_table_levels(&self) -> usize {
        4
    }
}
