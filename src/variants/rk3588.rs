use crate::variants::{_macros::domain_m_o_r, DomainMap, PD, RockchipDomainInfo, RockchipPmuInfo};

define_pd!(PD_NPU, 8);
define_pd!(PD_NPUTOP, 9);
define_pd!(PD_NPU1, 10);
define_pd!(PD_NPU2, 11);

pub fn pmu_info() -> RockchipPmuInfo {
    RockchipPmuInfo {
        pwr_offset: 0x14c,
        status_offset: 0x180,
        req_offset: 0x10c,
        idle_offset: 0x120,
        ack_offset: 0x118,
        mem_pwr_offset: 0x1a0,
        chain_status_offset: 0x1f0,
        mem_status_offset: 0x1f8,
        repair_status_offset: 0x290,
        domains: domains(),
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn domain_info(
    name: &'static str,
    pwr_offset: u32,
    pwr: i32,
    status: i32,
    mem_offset: u32,
    mem_status: i32,
    repair_status: i32,
    req_offset: u32,
    req: i32,
    idle: i32,
    wakeup: bool,
) -> RockchipDomainInfo {
    domain_m_o_r(
        name,
        pwr_offset,
        pwr,
        status,
        mem_offset,
        mem_status,
        repair_status,
        req_offset,
        req,
        idle,
        idle,
        wakeup,
        false,
    )
}

fn domains() -> DomainMap {
    map! {
        PD_NPU    => domain_info("npu",    0x0, bit!(1), bit!(1), 0x0, 0,        0,       0x0, 0,       0,       false),
        PD_NPUTOP => domain_info("nputop", 0x0, bit!(3), 0,       0x0, bit!(11), bit!(2), 0x0, bit!(1), bit!(1), false),
        PD_NPU1   => domain_info("npu1",   0x0, bit!(4), 0,       0x0, bit!(12), bit!(3), 0x0, bit!(2), bit!(2), false),
        PD_NPU2   => domain_info("npu2",   0x0, bit!(5), 0,       0x0, bit!(13), bit!(4), 0x0, bit!(3), bit!(3), false),
    }
}
