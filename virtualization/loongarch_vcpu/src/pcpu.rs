use crate::{
    registers::{
        CSR_EENTRY, csr_read, csr_write, gcsr_eentry_read, gintc_set_hwi_passthrough, gstat_read,
        gstat_write, read_gintc, write_gintc,
    },
    types::LoongArchVcpuResult,
};

/// Per-CPU LoongArch virtualization state.
///
/// This object stores register snapshots rather than a hardware page, so it
/// keeps ordinary C alignment and can be embedded in the fixed CPU-area ABI.
#[repr(C)]
pub struct LoongArchPerCpu {
    pub cpu_id: usize,
    pub original_eentry: usize,
    pub original_gstat: usize,
    pub original_gcsr_eentry: usize,
    pub original_gintc: usize,
    pub enabled: bool,
}

impl LoongArchPerCpu {
    pub fn new(cpu_id: usize) -> LoongArchVcpuResult<Self> {
        Ok(Self {
            cpu_id,
            original_eentry: 0,
            original_gstat: 0,
            original_gcsr_eentry: 0,
            original_gintc: 0,
            enabled: false,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn hardware_enable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> LoongArchVcpuResult {
        self.original_eentry = unsafe { csr_read::<CSR_EENTRY>() };
        self.original_gstat = gstat_read();
        self.original_gcsr_eentry = gcsr_eentry_read();
        self.original_gintc = read_gintc();
        unsafe { gintc_set_hwi_passthrough(0) };
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

    pub fn hardware_disable(&mut self, _cpu_pin: &ax_cpu_local::CpuPin) -> LoongArchVcpuResult {
        unsafe {
            gstat_write(self.original_gstat);
            write_gintc(self.original_gintc);
            csr_write::<CSR_EENTRY>(self.original_eentry);
        }
        self.enabled = false;

        log::debug!("LoongArch virtualization disabled for CPU {}", self.cpu_id);
        Ok(())
    }

    pub const fn max_guest_page_table_levels(&self) -> usize {
        4
    }
}
