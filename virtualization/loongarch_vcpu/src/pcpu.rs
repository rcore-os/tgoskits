use ax_errno::AxResult;
use axvm_types::VmArchPerCpuOps;

use crate::registers::{
    CSR_EENTRY, csr_read, csr_write, gcsr_eentry_read, gstat_read, gstat_write,
};

#[repr(C)]
#[repr(align(4096))]
pub struct LoongArchPerCpu {
    pub cpu_id: usize,
    pub original_eentry: usize,
    pub original_gstat: usize,
    pub original_gcsr_eentry: usize,
    pub enabled: bool,
}

impl VmArchPerCpuOps for LoongArchPerCpu {
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
        self.enabled = true;

        log::debug!(
            "LoongArch virtualization enabled for CPU {}, host CSR.EENTRY={:#x}, guest \
             GCSR.EENTRY={:#x}",
            self.cpu_id,
            self.original_eentry,
            self.original_gcsr_eentry
        );
        Ok(())
    }

    fn hardware_disable(&mut self) -> AxResult {
        unsafe {
            gstat_write(self.original_gstat);
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
