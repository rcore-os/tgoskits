#![allow(dead_code)]

/// Maximum ID for Software Generated Interrupts (SGI)
pub(crate) const SGI_ID_MAX: usize = 16;
/// Maximum ID for Private Peripheral Interrupts (PPI), range: 16-31
pub(crate) const PPI_ID_MAX: usize = 32;
/// Maximum ID for Shared Peripheral Interrupts (SPI)
pub(crate) const SPI_ID_MAX: usize = 512;
/// Number of GICH List Registers
pub(crate) const GICH_LR_NUM: usize = 4;

/* GIC Distributor Register Offsets */
/// Control Register
pub(crate) const VGICD_CTLR: usize = 0x0000;
/// Type Register
pub(crate) const VGICD_TYPER: usize = 0x0004;
/// Implementer Identification Register
pub(crate) const VGICD_IIDR: usize = 0x0008;
/// Interrupt Group Registers
pub(crate) const VGICD_IGROUPR_X: usize = 0x0080;
/// Interrupt Set-Enable Registers
pub(crate) const VGICD_ISENABLER_X: usize = 0x0100;
/// Interrupt Clear-Enable Registers
pub(crate) const VGICD_ICENABLER_X: usize = 0x0180;
/// Interrupt Set-Pending Registers
pub(crate) const VGICD_ISPENDER_X: usize = 0x0200;
/// Interrupt Clear-Pending Registers
pub(crate) const VGICD_ICPENDER_X: usize = 0x0280;
/// Interrupt Set-Active Registers
pub(crate) const VGICD_ISACTIVER_X: usize = 0x0300;
/// Interrupt Clear-Active Registers
pub(crate) const VGICD_ICACTIVER_X: usize = 0x0380;
/// Interrupt Priority Registers
pub(crate) const VGICD_IPRIORITYR_X: usize = 0x0400;
/// Interrupt Target Registers
pub(crate) const VGICD_ITARGETSR_X: usize = 0x0800;
/// Interrupt Configuration Registers
pub(crate) const VGICD_ICFGR_X: usize = 0x0c00;
/// Private Peripheral Interrupt Status Register
pub(crate) const VGICD_PPISR: usize = 0x0d00;
/// Shared Peripheral Interrupt Status Registers
pub(crate) const VGICD_SPISR_X: usize = 0x0d04;
/// Non-Secure Access Control Registers
pub(crate) const VGICD_NSACR_X: usize = 0x0e00;
/// Software Generated Interrupt Register
pub(crate) const VGICD_SGIR: usize = 0x0f00;
/// SGI Clear-Pending Registers
pub(crate) const VGICD_CPENDSGIR_X: usize = 0x0f10;
/// SGI Set-Pending Registers
pub(crate) const VGICD_SPENDSGIR_X: usize = 0x0f20;

/* GIC CPU Interface Register Offsets */
/// CPU Interface Control Register
pub(crate) const VGICC_CTRL: usize = 0x0000;
/// Priority Mask Register
pub(crate) const VGICC_PMR: usize = 0x0004;
/// Binary Point Register
pub(crate) const VGICC_BPR: usize = 0x0008;
/// Interrupt Acknowledge Register
pub(crate) const VGICC_IAR: usize = 0x000c;
/// End Of Interrupt Register
pub(crate) const VGICC_EOIR: usize = 0x0010;
/// Running Priority Register
pub(crate) const VGICC_RPR: usize = 0x0014;
/// Highest Priority Pending Interrupt Register
pub(crate) const VGICC_HPPIR: usize = 0x0018;
/// Aliased Binary Point Register
pub(crate) const VGICC_ABPR: usize = 0x001c;
/// Aliased Interrupt Acknowledge Register
pub(crate) const VGICC_AIAR: usize = 0x0020;
/// Aliased End Of Interrupt Register
pub(crate) const VGICC_AEOIR: usize = 0x0024;
/// Aliased Highest Priority Pending Interrupt Register
pub(crate) const VGICC_AHPPIR: usize = 0x0028;
/// Active Priorities Registers
pub(crate) const VGICC_APR_X: usize = 0x00d0;
/// Non-secure Active Priorities Registers
pub(crate) const VGICC_NSAPR_X: usize = 0x00e0;
/// CPU Interface Identification Register
pub(crate) const VGICC_IIDR: usize = 0x00fc;
/// Deactivate Interrupt Register
pub(crate) const VGICC_DIR: usize = 0x1000;

/* GIC Virtual Interface Control Register Offsets */
/// Hypervisor Control Register
pub(crate) const VGICH_HCR: usize = 0x0000;
/// VGIC Type Register
pub(crate) const VGICH_VTR: usize = 0x0004;
/// Virtual Machine Control Register
pub(crate) const VGICH_VMCR: usize = 0x0008;
/// Maintenance Interrupt Status Register
pub(crate) const VGICH_MISR: usize = 0x0010;
/// End of Interrupt Status Register 0
pub(crate) const VGICH_EISR0: usize = 0x0020;
/// End of Interrupt Status Register 1
pub(crate) const VGICH_EISR1: usize = 0x0024;
/// Empty List Register Status Register 0
pub(crate) const VGICH_ELSR0: usize = 0x0030;
/// Empty List Register Status Register 1
pub(crate) const VGICH_ELSR1: usize = 0x0034;
/// Active Priorities Register
pub(crate) const VGICH_APR: usize = 0x00f0;
/// List Registers
pub(crate) const VGICH_LR_X: usize = 0x0100;
