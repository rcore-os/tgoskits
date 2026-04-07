extern crate alloc;

use axplat::irq::{HandlerTable, IrqHandler, IrqIf};
use somehal::irq_handler;

/// The maximum number of IRQs.
const MAX_IRQ_COUNT: usize = 1024;

static IRQ_HANDLER_TABLE: HandlerTable<MAX_IRQ_COUNT> = HandlerTable::new();

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_raw: usize, enabled: bool) {
        somehal::irq::irq_set_enable(irq_raw.into(), enabled);
    }

    /// Registers an IRQ handler for the given IRQ.
    ///
    /// It also enables the IRQ if the registration succeeds. It returns `false`
    /// if the registration failed.
    fn register(irq_num: usize, handler: IrqHandler) -> bool {
        debug!("register handler IRQ {}", irq_num);

        if IRQ_HANDLER_TABLE.register_handler(irq_num, handler) {
            Self::set_enable(irq_num, true);
            return true;
        }
        warn!("register handler for IRQ {} failed", irq_num);
        false
    }

    /// Unregisters the IRQ handler for the given IRQ.
    ///
    /// It also disables the IRQ if the unregistration succeeds. It returns the
    /// existing handler if it is registered, `None` otherwise.
    fn unregister(irq_num: usize) -> Option<IrqHandler> {
        trace!("unregister handler IRQ {}", irq_num);
        Self::set_enable(irq_num, false);
        IRQ_HANDLER_TABLE.unregister_handler(irq_num)
    }

    /// Handles the IRQ.
    ///
    /// It is called by the common interrupt handler. It should look up in the
    /// IRQ handler table and calls the corresponding handler. If necessary, it
    /// also acknowledges the interrupt controller after handling.
    fn handle(_irq_num: usize) -> Option<usize> {
        let irq = somehal::irq::irq_handler_raw();
        Some(irq.raw())
    }

    fn send_ipi(_id: usize, _target: axplat::irq::IpiTarget) {
        #[cfg(target_arch = "aarch64")]
        {
            let mut gic = rdrive::get_one::<rdif_intc::Intc>()
                .expect("Failed to get GIC driver")
                .lock()
                .unwrap();

            if let Some(gic) = gic.typed_mut::<arm_gic_driver::v2::Gic>() {
                use arm_gic_driver::{
                    IntId,
                    v2::{SGITarget, TargetList},
                };

                match _target {
                    axplat::irq::IpiTarget::Current { cpu_id: _ } => {
                        gic.send_sgi(IntId::sgi(_id as u32), SGITarget::Current);
                    }
                    axplat::irq::IpiTarget::Other { cpu_id } => {
                        let target_list = TargetList::new(&mut [cpu_id].into_iter());
                        gic.send_sgi(IntId::sgi(_id as u32), SGITarget::TargetList(target_list));
                    }
                    axplat::irq::IpiTarget::AllExceptCurrent {
                        cpu_id: _,
                        cpu_num: _,
                    } => {
                        gic.send_sgi(IntId::sgi(_id as u32), SGITarget::AllOther);
                    }
                }
                return;
            }

            if let Some(_gic) = gic.typed_mut::<arm_gic_driver::v3::Gic>() {
                use arm_gic_driver::{
                    IntId,
                    v3::{Affinity, SGITarget},
                };

                let cpu_affinity = |cpu_idx: usize| {
                    let meta = somehal::smp::cpu_meta(cpu_idx)
                        .unwrap_or_else(|| panic!("invalid cpu idx for ipi target: {cpu_idx}"));
                    Affinity::from_mpidr(meta.cpu_id as u64)
                };

                match _target {
                    axplat::irq::IpiTarget::Current { cpu_id } => {
                        let aff = cpu_affinity(cpu_id);
                        arm_gic_driver::v3::send_sgi(
                            IntId::sgi(_id as u32),
                            SGITarget::list([aff]),
                        );
                    }
                    axplat::irq::IpiTarget::Other { cpu_id } => {
                        let aff = cpu_affinity(cpu_id);
                        arm_gic_driver::v3::send_sgi(
                            IntId::sgi(_id as u32),
                            SGITarget::list([aff]),
                        );
                    }
                    axplat::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                        let mut targets = alloc::vec::Vec::new();
                        for i in 0..cpu_num {
                            if i != cpu_id {
                                targets.push(cpu_affinity(i));
                            }
                        }
                        arm_gic_driver::v3::send_sgi(
                            IntId::sgi(_id as u32),
                            SGITarget::list(targets),
                        );
                    }
                }
                return;
            }

            panic!("no gic driver found")
        }

        #[cfg(not(target_arch = "aarch64"))]
        todo!()
    }
}

#[irq_handler]
fn somehal_handle_irq(irq: somehal::irq::IrqId) {
    if !IRQ_HANDLER_TABLE.handle(irq.raw()) {
        warn!("Unhandled IRQ {irq:?}");
    }
}
