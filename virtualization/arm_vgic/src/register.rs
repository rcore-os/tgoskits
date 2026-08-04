//! GICv3 register offsets used by the checked MMIO model.

pub(crate) const GICD_CTLR: u64 = 0x0000;
pub(crate) const GICD_TYPER: u64 = 0x0004;
pub(crate) const GICD_IIDR: u64 = 0x0008;
pub(crate) const GICD_IGROUPR: u64 = 0x0080;
pub(crate) const GICD_ISENABLER: u64 = 0x0100;
pub(crate) const GICD_ICENABLER: u64 = 0x0180;
pub(crate) const GICD_ISPENDR: u64 = 0x0200;
pub(crate) const GICD_ICPENDR: u64 = 0x0280;
pub(crate) const GICD_ISACTIVER: u64 = 0x0300;
pub(crate) const GICD_ICACTIVER: u64 = 0x0380;
pub(crate) const GICD_IPRIORITYR: u64 = 0x0400;
pub(crate) const GICD_ITARGETSR: u64 = 0x0800;
pub(crate) const GICD_ICFGR: u64 = 0x0c00;
pub(crate) const GICD_SGIR: u64 = 0x0f00;
pub(crate) const GICD_CPENDSGIR: u64 = 0x0f10;
pub(crate) const GICD_SPENDSGIR: u64 = 0x0f20;
pub(crate) const GICD_IROUTER: u64 = 0x6000;

pub(crate) const GICC_CTLR: u64 = 0x0000;
pub(crate) const GICC_PMR: u64 = 0x0004;
pub(crate) const GICC_BPR: u64 = 0x0008;
pub(crate) const GICC_IAR: u64 = 0x000c;
pub(crate) const GICC_EOIR: u64 = 0x0010;
pub(crate) const GICC_RPR: u64 = 0x0014;
pub(crate) const GICC_HPPIR: u64 = 0x0018;
pub(crate) const GICC_ABPR: u64 = 0x001c;
pub(crate) const GICC_AIAR: u64 = 0x0020;
pub(crate) const GICC_AEOIR: u64 = 0x0024;
pub(crate) const GICC_AHPPIR: u64 = 0x0028;
pub(crate) const GICC_APR: u64 = 0x00d0;
pub(crate) const GICC_IIDR: u64 = 0x00fc;
pub(crate) const GICC_DIR: u64 = 0x1000;

const GIC_COMPONENT_ID_START: u64 = 0xffd0;
const GIC_COMPONENT_ID_END: u64 = 0x1_0000;
const GICV3_ARCHITECTURE_REVISION: u8 = 3;

pub(crate) const GICR_CTLR: u64 = 0x0000;
pub(crate) const GICR_IIDR: u64 = 0x0004;
pub(crate) const GICR_TYPER: u64 = 0x0008;
pub(crate) const GICR_WAKER: u64 = 0x0014;
pub(crate) const GICR_PROPBASER: u64 = 0x0070;
pub(crate) const GICR_PENDBASER: u64 = 0x0078;
pub(crate) const GICR_SYNCR: u64 = 0x00c0;
pub(crate) const GICR_SGI_BASE: u64 = 0x1_0000;

pub(crate) const GITS_CTLR: u64 = 0x0000;
pub(crate) const GITS_IIDR: u64 = 0x0004;
pub(crate) const GITS_TYPER: u64 = 0x0008;
pub(crate) const GITS_CBASER: u64 = 0x0080;
pub(crate) const GITS_CWRITER: u64 = 0x0088;
pub(crate) const GITS_CREADR: u64 = 0x0090;
pub(crate) const GITS_BASER: u64 = 0x0100;
pub(crate) const GITS_BASER_COUNT: usize = 8;

#[derive(Clone, Copy)]
pub(crate) enum GicComponent {
    Distributor,
    Redistributor,
    Its,
}

pub(crate) fn component_id(offset: u64, component: GicComponent) -> Option<u64> {
    if !(GIC_COMPONENT_ID_START..GIC_COMPONENT_ID_END).contains(&offset)
        || !offset.is_multiple_of(4)
    {
        return None;
    }

    let index = ((offset - GIC_COMPONENT_ID_START) / 4) as usize;
    let mut value = COMPONENT_IDS[index];
    if index == 4 {
        value = component.peripheral_id();
    } else if index == 6 {
        value |= GICV3_ARCHITECTURE_REVISION << 4;
    }
    Some(u64::from(value))
}

impl GicComponent {
    const fn peripheral_id(self) -> u8 {
        match self {
            Self::Distributor => 0x92,
            Self::Redistributor => 0x93,
            Self::Its => 0x94,
        }
    }
}

// ARM's GIC-500-compatible CoreSight identity. PIDR0 is replaced with the
// concrete component identity and PIDR2 gains the GIC architecture revision.
const COMPONENT_IDS: [u8; 12] = [
    0x44, 0x00, 0x00, 0x00, 0x00, 0xb4, 0x0b, 0x00, 0x0d, 0xf0, 0x05, 0xb1,
];
