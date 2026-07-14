use crate::{
    context_frame::LoongArchContextFrame,
    registers::CRMD,
    trap::{get_badv, get_guest_pc},
    types::LoongArchAccessFlags,
};

const GUEST_RAM_START: usize = 0x0008_0000;
const GUEST_HIGH_RAM_START: usize = 0x8000_0000;
const GUEST_HIGH_RAM_END: usize = 0xb000_0000;
const QEMU_VIRT_MMIO_START: usize = 0x1000_0000;
const QEMU_VIRT_MMIO_END: usize = 0x8000_0000;
const GUEST_RAM_END: usize = QEMU_VIRT_MMIO_START;

pub(crate) fn direct_map_guest_addr_to_gpa(addr: usize) -> usize {
    if matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) {
        addr & 0x0000_ffff_ffff_ffff
    } else if matches!(addr >> 44, 0x8..=0xa) {
        addr & 0x0000_0fff_ffff_ffff
    } else if (0xffff_8000_0000..0xffff_c000_0000).contains(&addr) {
        let gpa = addr - 0xffff_8000_0000;
        if is_known_guest_physical_addr(gpa) {
            gpa
        } else {
            addr
        }
    } else {
        addr
    }
}

pub(crate) fn get_refill_access_flags(ctx: &LoongArchContextFrame) -> LoongArchAccessFlags {
    let badv = direct_map_guest_addr_to_gpa(get_badv(ctx));
    let pc_gpa = direct_map_guest_addr_to_gpa(get_guest_pc(ctx));
    if badv == pc_gpa {
        LoongArchAccessFlags::EXECUTE
    } else {
        LoongArchAccessFlags::READ | LoongArchAccessFlags::WRITE
    }
}

fn guest_paging_enabled(ctx: &LoongArchContextFrame) -> bool {
    ctx.gcsr_crmd & CRMD::PG::SET.value != 0
}

fn is_guest_direct_mapped_va(addr: usize) -> bool {
    matches!(addr >> 48, 0x8000 | 0x9000 | 0xa000) || matches!(addr >> 44, 0x8..=0xa)
}

fn is_known_guest_physical_addr(addr: usize) -> bool {
    (GUEST_RAM_START..GUEST_RAM_END).contains(&addr)
        || (GUEST_HIGH_RAM_START..GUEST_HIGH_RAM_END).contains(&addr)
        || (QEMU_VIRT_MMIO_START..QEMU_VIRT_MMIO_END).contains(&addr)
}

pub(crate) fn should_inject_guest_virtual_fault(
    ctx: &LoongArchContextFrame,
    badv: usize,
    from_tlb_refill: bool,
) -> bool {
    let is_direct = is_guest_direct_mapped_va(badv);
    let known_physical = is_known_guest_physical_addr(badv);

    if from_tlb_refill {
        if ctx.gcsr_tlbrentry == 0 || is_direct {
            false
        } else {
            !known_physical
        }
    } else if !guest_paging_enabled(ctx) || is_direct || ctx.gcsr_eentry == 0 {
        false
    } else {
        !known_physical
    }
}
