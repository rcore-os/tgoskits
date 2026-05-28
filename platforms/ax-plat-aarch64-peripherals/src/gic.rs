//! ARM Generic Interrupt Controller (GIC), dispatcher over v2 and v3.
//!
//! The backend is selected at compile time by the `gic-v3` Cargo
//! feature on this crate (forwarded from
//! `ax-plat-aarch64-qemu-virt`'s `gic-v3` feature, in turn from
//! `starryos`'s `gic-v3` feature and the `aarch64-gic-v3` feature
//! on `ax-feat` / `ax-hal`). Default builds stay on GICv2 to match
//! the QEMU TCG `-cpu cortex-a72` configuration used by CI. Enabling
//! `gic-v3` switches to GICv3, which is required to boot under Apple
//! HVF on Apple Silicon.
//!
//! Why this matters for HVF: Apple's Hypervisor framework cannot
//! always populate decoded instruction syndromes (ESR.ISV) for MMIO
//! traps, and QEMU's HVF backend asserts on ISV=0. The GICv2 CPU
//! interface (GICC) is MMIO and reliably triggers that assert during
//! `init_gicc()`. GICv3 moves the CPU interface to system registers
//! (`ICC_IAR1_EL1`, `ICC_EOIR1_EL1`, `ICC_SGI1R_EL1`, ...), which
//! HVF handles natively without trap-and-emulate. The distributor
//! (GICD) and redistributor (GICR) stay MMIO in both versions, but
//! those accesses are plain u32 LDR/STR that HVF decodes correctly.
//!
//! Runtime detection between v2 and v3 is not feasible here; see the
//! `init_gic_v3` docs for why each candidate probe fails under at
//! least one of TCG-v2 / HVF-v3.

use arm_gic_driver::v2::{
    Ack as V2Ack, Gic as GicV2, IntId, SGITarget as V2SGITarget, TargetList, TrapOp as V2TrapOp,
    Trigger, VirtAddr as DriverVirtAddr,
};
#[cfg(feature = "gic-v3")]
use arm_gic_driver::v3::{Affinity, Gic as GicV3, SGITarget as V3SGITarget};
use ax_kspin::SpinNoIrq;
use ax_lazyinit::LazyInit;
use ax_plat::irq::{IpiTarget, dispatch_irq};

/// Per-backend handle. Stored inside [`GIC`] so every ops call can
/// match over the variant and forward to the right driver.
enum Backend {
    V2 {
        gic: GicV2,
        /// v2's per-CPU ack/eoi are done via MMIO on the GICC block.
        /// Cached here so `handle_irq` doesn't need to reach through
        /// `self.gic.cpu_interface()` on every IRQ.
        trap: V2TrapOp,
    },
    #[cfg(feature = "gic-v3")]
    V3 { gic: GicV3 },
}

static GIC: LazyInit<SpinNoIrq<Backend>> = LazyInit::new();

/// Enables or disables the given IRQ.
pub fn set_enable(irq: usize, enabled: bool) {
    trace!("GIC set enable: {irq} {enabled}");
    let intid = unsafe { IntId::raw(irq as u32) };
    let mut guard = GIC.lock();
    match &mut *guard {
        Backend::V2 { gic, .. } => {
            gic.set_irq_enable(intid, enabled);
            if !intid.is_private() {
                gic.set_cfg(intid, Trigger::Edge);
            }
        }
        #[cfg(feature = "gic-v3")]
        Backend::V3 { gic } => {
            gic.set_irq_enable(intid, enabled);
            // v3 SPI trigger config also stays on the distributor.
            if !intid.is_private() {
                gic.set_cfg(intid, Trigger::Edge);
            }
        }
    }
}

enum ActiveIrq {
    V2(V2Ack),
    #[cfg(feature = "gic-v3")]
    V3(IntId),
}

/// Handles the IRQ.
pub fn handle_irq(_irq: usize) -> Option<usize> {
    // Ack + dispatch + EOI. v2 goes through GICC MMIO via the cached
    // TrapOp; v3 uses `ICC_IAR1_EL1` / `ICC_EOIR1_EL1` system regs.
    let (irq, active_irq) = {
        let guard = GIC.lock();
        match &*guard {
            Backend::V2 { trap, .. } => {
                let ack = trap.ack();
                if ack.is_special() {
                    return None;
                }
                let irq = match ack {
                    V2Ack::Other(intid) => intid,
                    V2Ack::SGI { intid, cpu_id: _ } => intid,
                }
                .to_u32() as usize;
                (irq, ActiveIrq::V2(ack))
            }
            #[cfg(feature = "gic-v3")]
            Backend::V3 { gic } => {
                // Group 1 Non-secure is what QEMU's virt machine uses.
                let ack = gic.cpu_interface().ack1();
                // v3 special interrupt ids: 0x3FF = spurious, 1020–1023
                // = special. IntId::is_special covers those.
                if ack.is_special() {
                    return None;
                }
                (ack.to_u32() as usize, ActiveIrq::V3(ack))
            }
        }
    };

    trace!("IRQ {irq}");
    if !dispatch_irq(irq).handled {
        debug!("Unhandled IRQ {irq}");
    }

    // EOI and (if two-step mode) deactivate.
    let guard = GIC.lock();
    match active_irq {
        ActiveIrq::V2(ack) => match &*guard {
            Backend::V2 { trap, .. } => {
                trap.eoi(ack);
                if trap.eoi_mode_ns() {
                    trap.dir(ack);
                }
            }
            #[cfg(feature = "gic-v3")]
            Backend::V3 { .. } => unreachable!("GIC backend changed while handling an IRQ"),
        },
        #[cfg(feature = "gic-v3")]
        ActiveIrq::V3(ack) => match &*guard {
            Backend::V3 { gic } => {
                let cpu = gic.cpu_interface();
                cpu.eoi1(ack);
                if cpu.eoi_mode() {
                    cpu.dir(ack);
                }
            }
            Backend::V2 { .. } => unreachable!("GIC backend changed while handling an IRQ"),
        },
    }

    Some(irq)
}

/// Sends an inter-processor interrupt (IPI) to the specified target.
///
/// v2 targets use an 8-bit CPU bitmap (TargetList); v3 uses affinity
/// routing through ICC_SGI1R_EL1 with per-CPU MPIDR decomposition.
pub fn send_ipi(irq_num: usize, target: IpiTarget) {
    let guard = GIC.lock();
    match &*guard {
        Backend::V2 { gic, .. } => {
            let sgi = IntId::sgi(irq_num as u32);
            let v2_target = match target {
                IpiTarget::Current { cpu_id: _ } => V2SGITarget::Current,
                IpiTarget::Other { cpu_id } => {
                    V2SGITarget::TargetList(TargetList::new(&mut core::iter::once(cpu_id)))
                }
                IpiTarget::AllExceptCurrent { .. } => V2SGITarget::AllOther,
            };
            gic.send_sgi(sgi, v2_target);
        }
        #[cfg(feature = "gic-v3")]
        Backend::V3 { gic } => {
            let sgi = IntId::sgi(irq_num as u32);
            let v3_target = match target {
                // `SGITarget::current()` builds a single-CPU List with
                // this CPU's MPIDR affinity — sends the SGI to just us.
                IpiTarget::Current { cpu_id: _ } => V3SGITarget::current(),
                IpiTarget::Other { cpu_id } => {
                    // Target one specific CPU by its logical id. On
                    // QEMU's single-socket virt machine the mapping is
                    // aff3:aff2:aff1 = 0 and aff0 = cpu_id (0..=N-1).
                    V3SGITarget::list([Affinity {
                        aff3: 0,
                        aff2: 0,
                        aff1: 0,
                        aff0: cpu_id as u8,
                    }])
                }
                // GICv3 `ICC_SGI1R_EL1.IRM=1` routes the SGI to every
                // PE *except the sender* — our `AllExceptCurrent`
                // semantic.
                IpiTarget::AllExceptCurrent { .. } => V3SGITarget::All,
            };
            gic.cpu_interface().send_sgi(sgi, v3_target);
        }
    }
}

/// Initializes the GICv2 distributor and MMIO CPU interface.
pub fn init_gic(gicd_base: ax_plat::mem::VirtAddr, gicc_base: ax_plat::mem::VirtAddr) {
    info!("Initialize GICv2 (MMIO CPU interface)...");
    let gicd = DriverVirtAddr::new(gicd_base.into());
    let gicc = DriverVirtAddr::new(gicc_base.into());
    // SAFETY: platform code supplies mapped Device-memory addresses
    // for the interrupt controller and this kernel owns the device.
    let mut gic = unsafe { GicV2::new(gicd, gicc, None) };
    gic.init();
    let cpu = gic.cpu_interface();
    let trap = cpu.trap_operations();
    GIC.init_once(SpinNoIrq::new(Backend::V2 { gic, trap }));
}

/// Initializes the GICv3 distributor and redistributor.
///
/// GICv3 is selected at compile time by enabling the `gic-v3`
/// feature on this crate. We do not runtime-probe the backend
/// because neither `ID_AA64PFR0_EL1.GIC` nor GICD PIDR2 probing
/// works cleanly under both QEMU TCG GICv2 and Apple HVF GICv3.
#[cfg(feature = "gic-v3")]
pub fn init_gic_v3(gicd_base: ax_plat::mem::VirtAddr, gicr_base: ax_plat::mem::VirtAddr) {
    info!("Initialize GICv3 (system-register CPU interface)...");
    let gicd = DriverVirtAddr::new(gicd_base.into());
    let gicr = DriverVirtAddr::new(gicr_base.into());
    // SAFETY: platform code supplies mapped Device-memory addresses
    // for the interrupt controller and this kernel owns the device.
    let mut gic = unsafe { GicV3::new(gicd, gicr) };
    gic.init();
    GIC.init_once(SpinNoIrq::new(Backend::V3 { gic }));
}

/// Initializes the CPU-side state. Must be called per-CPU.
pub fn init_gicc() {
    let mut guard = GIC.lock();
    match &mut *guard {
        Backend::V2 { gic, .. } => {
            let mut cpu = gic.cpu_interface();
            cpu.init_current_cpu();
            cpu.set_eoi_mode_ns(false);
        }
        #[cfg(feature = "gic-v3")]
        Backend::V3 { gic } => {
            let mut cpu = gic.cpu_interface();
            if let Err(e) = cpu.init_current_cpu() {
                warn!("GICv3 CPU interface init failed: {e}");
            }
            cpu.set_eoi_mode(false);
        }
    }
}

/// Default implementation of [`ax_plat::irq::IrqIf`] using the GIC.
#[macro_export]
macro_rules! irq_if_impl {
    ($name:ident) => {
        struct $name;

        #[impl_plat_interface]
        impl ax_plat::irq::IrqIf for $name {
            /// Enables or disables the given IRQ.
            fn set_enable(irq: usize, enabled: bool) {
                $crate::gic::set_enable(irq, enabled);
            }

            /// Handles the IRQ.
            fn handle(irq: usize) -> Option<usize> {
                $crate::gic::handle_irq(irq)
            }

            /// Sends an inter-processor interrupt (IPI) to the specified target CPU or all CPUs.
            fn send_ipi(irq_num: usize, target: ax_plat::irq::IpiTarget) {
                $crate::gic::send_ipi(irq_num, target);
            }
        }
    };
}
