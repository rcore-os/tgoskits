use crate::variants::{
    _macros::domain_m_o_r, DomainMap, PowerDomain, RockchipDomainInfo, RockchipPmuInfo,
};

define_power_domains! {
    /// GPU domain
    GPU = 12,
    /// NPU main domain
    NPU = 8,
    /// VCODEC domain
    VCODEC = 13,
    /// NPU top domain
    NPUTOP = 9,
    /// NPU core 1
    NPU1 = 10,
    /// NPU core 2
    NPU2 = 11,
    /// VENC0 domain
    VENC0 = 16,
    /// VENC1 domain
    VENC1 = 17,
    /// RKVDEC0 domain
    RKVDEC0 = 14,
    /// RKVDEC1 domain
    RKVDEC1 = 15,
    /// VDPU domain
    VDPU = 21,
    /// RGA30 domain
    RGA30 = 22,
    /// AV1 decoder domain
    AV1 = 23,
    /// VI (Video Input) domain
    VI = 27,
    /// FEC domain
    FEC = 29,
    /// ISP1 domain
    ISP1 = 28,
    /// RGA31 domain
    RGA31 = 30,
    /// VOP domain (display)
    VOP = 24,
    /// VO0 display sub-domain
    VO0 = 25,
    /// VO1 display sub-domain
    VO1 = 26,
    /// AUDIO domain
    AUDIO = 38,
    /// PHP domain
    PHP = 32,
    /// GMAC domain
    GMAC = 33,
    /// PCIE domain
    PCIE = 34,
    /// NVM aggregate domain
    NVM = 35,
    /// NVM0 sub-domain
    NVM0 = 36,
    /// SDIO domain
    SDIO = 37,
    /// USB domain
    USB = 31,
    /// SDMMC domain
    SDMMC = 40,
}

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
        GPU     => domain_info("gpu",     0x0, bit!(0), 0,       0x0, 0,        bit!(1),  0x0, bit!(0), bit!(0), false),
        NPU     => domain_info("npu",     0x0, bit!(1), bit!(1), 0x0, 0,        0,        0x0, 0,       0,       false),
        VCODEC  => domain_info("vcodec",  0x0, bit!(2), bit!(2), 0x0, 0,        0,        0x0, 0,       0,       false),
        NPUTOP  => domain_info("nputop",  0x0, bit!(3), 0,       0x0, bit!(11), bit!(2),  0x0, bit!(1), bit!(1), false),
        NPU1    => domain_info("npu1",    0x0, bit!(4), 0,       0x0, bit!(12), bit!(3),  0x0, bit!(2), bit!(2), false),
        NPU2    => domain_info("npu2",    0x0, bit!(5), 0,       0x0, bit!(13), bit!(4),  0x0, bit!(3), bit!(3), false),
        VENC0   => domain_info("venc0",   0x0, bit!(6), 0,       0x0, bit!(14), bit!(5),  0x0, bit!(4), bit!(4), false),
        VENC1   => domain_info("venc1",   0x0, bit!(7), 0,       0x0, bit!(15), bit!(6),  0x0, bit!(5), bit!(5), false),
        RKVDEC0 => domain_info("rkvdec0", 0x0, bit!(8), 0,       0x0, bit!(16), bit!(7),  0x0, bit!(6), bit!(6), false),
        RKVDEC1 => domain_info("rkvdec1", 0x0, bit!(9), 0,       0x0, bit!(17), bit!(8),  0x0, bit!(7), bit!(7), false),
        VDPU    => domain_info("vdpu",    0x0, bit!(10),0,       0x0, bit!(18), bit!(9),  0x0, bit!(8), bit!(8), false),
        RGA30   => domain_info("rga30",   0x0, bit!(11),0,       0x0, bit!(19), bit!(10), 0x0, 0,       0,       false),
        AV1     => domain_info("av1",     0x0, bit!(12),0,       0x0, bit!(20), bit!(11), 0x0, bit!(9), bit!(9), false),
        VI      => domain_info("vi",      0x0, bit!(13),0,       0x0, bit!(21), bit!(12), 0x0, bit!(10),bit!(10),false),
        FEC     => domain_info("fec",     0x0, bit!(14),0,       0x0, bit!(22), bit!(13), 0x0, 0,       0,       false),
        ISP1    => domain_info("isp1",    0x0, bit!(15),0,       0x0, bit!(23), bit!(14), 0x0, bit!(11),bit!(11),false),
        RGA31   => domain_info("rga31",   0x4, bit!(0), 0,       0x0, bit!(24), bit!(15), 0x0, bit!(12),bit!(12),false),
        VOP     => domain_info("vop",     0x4, bit!(1), 0,       0x0, bit!(25), bit!(16), 0x0, bit!(13)|bit!(14), bit!(13)|bit!(14), false),
        VO0     => domain_info("vo0",     0x4, bit!(2), 0,       0x0, bit!(26), bit!(17), 0x0, bit!(15),bit!(15),false),
        VO1     => domain_info("vo1",     0x4, bit!(3), 0,       0x0, bit!(27), bit!(18), 0x4, bit!(0), bit!(16),false),
        AUDIO   => domain_info("audio",   0x4, bit!(4), 0,       0x0, bit!(28), bit!(19), 0x4, bit!(1), bit!(17),false),
        PHP     => domain_info("php",     0x4, bit!(5), 0,       0x0, bit!(29), bit!(20), 0x4, bit!(5), bit!(21),false),
        GMAC    => domain_info("gmac",    0x4, bit!(6), 0,       0x0, bit!(30), bit!(21), 0x0, 0,       0,       false),
        PCIE    => domain_info("pcie",    0x4, bit!(7), 0,       0x0, bit!(31), bit!(22), 0x0, 0,       0,       true),
        NVM     => domain_info("nvm",     0x4, bit!(8), bit!(24),0x4, 0,        0,        0x4, bit!(2), bit!(18),false),
        NVM0    => domain_info("nvm0",    0x4, bit!(9), 0,       0x4, bit!(1),  bit!(23), 0x0, 0,       0,       false),
        SDIO    => domain_info("sdio",    0x4, bit!(10),0,       0x4, bit!(2),  bit!(24), 0x4, bit!(3), bit!(19),false),
        USB     => domain_info("usb",     0x4, bit!(11),0,       0x4, bit!(3),  bit!(25), 0x4, bit!(4), bit!(20),true),
        SDMMC   => domain_info("sdmmc",   0x4, bit!(13),0,       0x4, bit!(5),  bit!(26), 0x0, 0,       0,       false),
    }
}
