use loongArch64::ipi::{csr_mail_send, send_ipi_single};

use crate::{arch::addrspace, power::CpuOnError};

const ACTION_BOOT_CPU: u32 = 1;

pub(crate) fn cpu_on(hartid: usize, entry: usize, arg: usize) -> Result<(), CpuOnError> {
    let entry = addrspace::to_cache(entry);
    super::entry::set_secondary_boot_arg(hartid, arg)?;
    unsafe {
        core::arch::asm!("dbar 0", options(nomem, nostack));
    }
    csr_mail_send(arg as u64, hartid, 1);
    csr_mail_send(entry as u64, hartid, 0);
    send_ipi_single(hartid, ACTION_BOOT_CPU);
    Ok(())
}
