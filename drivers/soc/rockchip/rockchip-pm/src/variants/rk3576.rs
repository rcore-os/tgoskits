//! RK3576 power domain definitions.

use crate::variants::{DomainMap, PowerDomain, RockchipDomainInfo, RockchipPmuInfo};

define_power_domains! {
    NPU = 0,
    NPUTOP = 1,
    NPU0 = 2,
    NPU1 = 3,
    GPU = 4,
    NVM = 5,
    SDGMAC = 6,
    USB = 7,
    PHP = 8,
    SUBPHP = 9,
    AUDIO = 10,
    VEPU0 = 11,
    VEPU1 = 12,
    VPU = 13,
    VDEC = 14,
    VI = 15,
    VO0 = 16,
    VO1 = 17,
    VOP = 18,
}

pub fn pmu_info() -> RockchipPmuInfo {
    RockchipPmuInfo {
        pwr_offset: 0x210,
        status_offset: 0x230,
        chain_status_offset: 0x248,
        mem_status_offset: 0x250,
        mem_pwr_offset: 0x300,
        req_offset: 0x110,
        idle_offset: 0x128,
        ack_offset: 0x120,
        repair_status_offset: 0x570,
        clk_ungate_offset: 0x140,
        domains: domains(),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn domain(
    name: &'static str,
    pwr_offset: u32,
    pwr_mask: i32,
    status_mask: i32,
    repair_status_mask: i32,
    req_offset: u32,
    req_mask: i32,
    idle_mask: i32,
    clk_ungate_mask: i32,
    active_wakeup: bool,
) -> RockchipDomainInfo {
    RockchipDomainInfo {
        name,
        pwr_offset,
        pwr_w_mask: ((pwr_mask as u32) << 16) as i32,
        pwr_mask,
        status_mask,
        mem_status_mask: repair_status_mask,
        repair_status_mask,
        req_offset,
        req_w_mask: ((req_mask as u32) << 16) as i32,
        req_mask,
        idle_mask,
        ack_mask: idle_mask,
        clk_ungate_mask,
        active_wakeup,
        ..Default::default()
    }
}

fn domains() -> DomainMap {
    map! {
        NPU     => domain("npu",    0x0, bit!(0),  bit!(0), 0,        0x0, 0,        0,        0,      false),
        NVM     => domain("nvm",    0x0, bit!(6),  0,       bit!(6),  0x4, bit!(2),  bit!(18), bit!(2),false),
        SDGMAC  => domain("sdgmac", 0x0, bit!(7),  0,       bit!(7),  0x4, bit!(1),  bit!(17), 0x6,    false),
        AUDIO   => domain("audio",  0x0, bit!(8),  0,       bit!(8),  0x4, bit!(0),  bit!(16), bit!(0),false),
        PHP     => domain("php",    0x0, bit!(9),  0,       bit!(9),  0x0, bit!(15), bit!(15), bit!(15),false),
        SUBPHP  => domain("subphp", 0x0, bit!(10), 0,       bit!(10), 0x0, 0,        0,        0,      false),
        VOP     => domain("vop",    0x0, bit!(11), 0,       bit!(11), 0x0, 0x6000,   0x6000,   0x6000, false),
        VO1     => domain("vo1",    0x0, bit!(14), 0,       bit!(14), 0x0, bit!(12), bit!(12), 0x7000, false),
        VO0     => domain("vo0",    0x0, bit!(15), 0,       bit!(15), 0x0, bit!(11), bit!(11), 0x6800, false),
        USB     => domain("usb",    0x4, bit!(0),  0,       bit!(16), 0x0, bit!(10), bit!(10), 0x6400, true),
        VI      => domain("vi",     0x4, bit!(1),  0,       bit!(17), 0x0, bit!(9),  bit!(9),  bit!(9),false),
        VEPU0   => domain("vepu0",  0x4, bit!(2),  0,       bit!(18), 0x0, bit!(7),  bit!(7),  0x280,  false),
        VEPU1   => domain("vepu1",  0x4, bit!(3),  0,       bit!(19), 0x0, bit!(8),  bit!(8),  bit!(8),false),
        VDEC    => domain("vdec",   0x4, bit!(4),  0,       bit!(20), 0x0, bit!(6),  bit!(6),  bit!(6),false),
        VPU     => domain("vpu",    0x4, bit!(5),  0,       bit!(21), 0x0, bit!(5),  bit!(5),  bit!(5),false),
        NPUTOP  => domain("nputop", 0x4, bit!(6),  0,       bit!(22), 0x0, 0x18,     0x18,     0x18,   false),
        NPU0    => domain("npu0",   0x4, bit!(7),  0,       bit!(23), 0x0, bit!(1),  bit!(1),  0x1a,   false),
        NPU1    => domain("npu1",   0x4, bit!(8),  0,       bit!(24), 0x0, bit!(2),  bit!(2),  0x1c,   false),
        GPU     => domain("gpu",    0x4, bit!(9),  0,       bit!(25), 0x0, bit!(0),  bit!(0),  bit!(0),false),
    }
}
