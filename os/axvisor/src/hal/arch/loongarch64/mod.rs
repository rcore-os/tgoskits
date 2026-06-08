#![allow(unsafe_op_in_unsafe_fn)]

mod api;
pub mod cache;

const CSR_GSTAT: u16 = 0x50;

const GSTAT_PGM: usize = 1 << 1;
const GSTAT_GIDBITS_MASK: usize = 0x3f << 4;
const GSTAT_GIDBITS_SHIFT: usize = 4;
const GSTAT_GID_MASK: usize = 0xff << 16;
const GSTAT_GID_SHIFT: usize = 16;

#[inline(always)]
unsafe fn csr_read<const CSR_NUM: u16>() -> usize {
    let value: usize;
    core::arch::asm!("csrrd {}, {}", out(reg) value, const CSR_NUM);
    value
}

#[inline(always)]
unsafe fn cpucfg_read(reg: u32) -> u32 {
    let value: u32;
    core::arch::asm!("cpucfg {}, {}", out(reg) value, in(reg) reg);
    value
}

fn has_virtualization_support() -> bool {
    let cpucfg2 = unsafe { cpucfg_read(2) };
    (cpucfg2 & (1 << 10)) != 0
}

#[inline(always)]
fn read_gstat() -> usize {
    unsafe { csr_read::<CSR_GSTAT>() }
}

fn get_gidbits() -> usize {
    (read_gstat() & GSTAT_GIDBITS_MASK) >> GSTAT_GIDBITS_SHIFT
}

fn max_gid() -> usize {
    let gidbits = get_gidbits();
    if gidbits == 0 { 0 } else { (1 << gidbits) - 1 }
}

fn current_gid() -> usize {
    (read_gstat() & GSTAT_GID_MASK) >> GSTAT_GID_SHIFT
}

pub fn hardware_check() {
    let gstat = read_gstat();
    info!("CSR.GSTAT = 0x{gstat:x}");

    if has_virtualization_support() {
        info!("LoongArch virtualization extensions supported (CPUCFG.2.LVZ[10]=1)");
    } else {
        warn!("LoongArch virtualization extensions not supported (CPUCFG.2.LVZ[10]=0)");
    }

    if (gstat & GSTAT_PGM) != 0 {
        info!("Guest mode currently enabled (PGM=1)");
    } else {
        info!("Guest mode currently disabled (PGM=0)");
    }

    let gidbits = get_gidbits();
    info!(
        "GIDBITS value: {gidbits} (supports {} GIDs)",
        1usize << gidbits
    );
    info!("Maximum GID value: {}", max_gid());
    info!("Current GID value: {}", current_gid());
}

pub fn inject_interrupt(vector: usize) {
    let vm = crate::vmm::vm_list::get_vm_by_id(axvisor_api::vmm::current_vm_id()).unwrap();
    if let Err(e) = vm.router().inject(axbus::IrqMessage::Legacy {
        line: axbus::IrqLine(vector as u32),
    }) {
        warn!("inject_interrupt({vector}) failed: {e}");
    }
}
