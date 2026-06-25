#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
#[cfg(all(target_arch = "riscv64", feature = "hv"))]
use core::sync::atomic::{AtomicPtr, Ordering};

use ax_plat::irq::{IrqAffinity, IrqError, IrqIf, dispatch_irq};

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
const RISCV_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
const LOONGARCH_MAX_IRQ_COUNT: usize = 256;
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
const IRQ_ROUTE_NONE: usize = 0;
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
const IRQ_TARGET_NONE: usize = usize::MAX;
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
const IRQ_TARGET_VM_SHIFT: usize = 32;
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
const LOONGARCH_IRQ_TRACE_LIMIT: usize = 80;

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
static LOONGARCH_VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
static LOONGARCH_GUEST_IRQ_ROUTES: [AtomicUsize; LOONGARCH_MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_ROUTE_NONE) }; LOONGARCH_MAX_IRQ_COUNT];
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
static LOONGARCH_GUEST_IRQ_TARGETS: [AtomicUsize; LOONGARCH_MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_TARGET_NONE) }; LOONGARCH_MAX_IRQ_COUNT];
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
static LOONGARCH_IRQ_MISS_LOGS: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
static LOONGARCH_IRQ_INJECT_LOGS: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
pub fn register_virtual_irq_injector(injector: fn(usize) -> bool) {
    VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
}

struct IrqIfImpl;

#[impl_plat_interface]
impl IrqIf for IrqIfImpl {
    /// Enables or disables the given IRQ.
    fn set_enable(irq_raw: usize, enabled: bool) {
        somehal::irq::irq_set_enable(irq_raw.into(), enabled);
    }

    fn set_affinity(irq_raw: usize, affinity: IrqAffinity) -> Result<(), IrqError> {
        let affinity = match affinity {
            IrqAffinity::Any => somehal::irq::IrqAffinity::Any,
            IrqAffinity::Fixed(cpu) => somehal::irq::IrqAffinity::Fixed { cpu_id: cpu.0 },
        };
        somehal::irq::irq_set_affinity(irq_raw.into(), affinity).map_err(|_| IrqError::Unsupported)
    }

    /// Handles the IRQ.
    fn handle(irq_num: usize) -> Option<usize> {
        let irq_num = {
            let active = somehal::irq::begin_irq(irq_num)?;
            let irq = active.id();
            let irq_num = irq.raw();

            #[cfg(all(target_arch = "riscv64", feature = "hv"))]
            if (irq_num & RISCV_INTERRUPT_BIT == 0) && inject_virtual_irq(irq_num) {
                return Some(irq_num);
            }

            let outcome = dispatch_irq(irq_num);
            if !outcome.handled {
                #[cfg(all(target_arch = "loongarch64", feature = "hv"))]
                if inject_loongarch_virtual_irq(irq_num) {
                    return Some(irq_num);
                }

                if outcome.called == 0 {
                    warn!("Unhandled IRQ {irq:?}");
                } else {
                    debug!("Spurious IRQ {irq:?}");
                }
            }
            irq_num
        };
        Some(irq_num)
    }

    fn send_ipi(id: usize, target: ax_plat::irq::IpiTarget) {
        let target = match target {
            ax_plat::irq::IpiTarget::Current { cpu_id } => {
                somehal::irq::IpiTarget::Current { cpu_id }
            }
            ax_plat::irq::IpiTarget::Other { cpu_id } => somehal::irq::IpiTarget::Other { cpu_id },
            ax_plat::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num } => {
                somehal::irq::IpiTarget::AllExceptCurrent { cpu_id, cpu_num }
            }
        };
        somehal::irq::send_ipi(id.into(), target);
    }
}

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
#[impl_plat_interface]
impl ax_plat::irq::LoongArchHvIrqIf for IrqIfImpl {
    fn register_virtual_irq_injector(injector: fn(usize, usize, usize, usize)) {
        LOONGARCH_VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
        debug!("LoongArch dynamic platform virtual IRQ injector registered");
    }

    fn register_guest_irq_route(
        physical_irq: usize,
        vm_id: usize,
        vcpu_id: usize,
        guest_vector: usize,
    ) {
        if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
            warn!("LoongArch guest IRQ route ignored: physical IRQ {physical_irq} out of range");
            return;
        }

        let target = (vm_id << IRQ_TARGET_VM_SHIFT) | vcpu_id;
        let previous_target = LOONGARCH_GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
        if previous_target != IRQ_TARGET_NONE && previous_target != target {
            warn!(
                "LoongArch guest IRQ route conflict: physical_irq={} is already routed to encoded \
                 target {:#x}, ignoring VM[{}] VCpu[{}]",
                physical_irq, previous_target, vm_id, vcpu_id
            );
            return;
        }

        LOONGARCH_GUEST_IRQ_TARGETS[physical_irq].store(target, Ordering::Release);
        LOONGARCH_GUEST_IRQ_ROUTES[physical_irq].store(guest_vector + 1, Ordering::Release);
        somehal::irq::irq_set_enable(physical_irq.into(), true);
        debug!(
            "LoongArch dynamic guest IRQ route: physical_irq={}, target=VM[{}] VCpu[{}], \
             guest_vector={}",
            physical_irq, vm_id, vcpu_id, guest_vector
        );
    }

    fn unregister_guest_irq_routes(vm_id: usize) {
        for physical_irq in 0..LOONGARCH_MAX_IRQ_COUNT {
            let target = LOONGARCH_GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
            if target == IRQ_TARGET_NONE || (target >> IRQ_TARGET_VM_SHIFT) != vm_id {
                continue;
            }

            LOONGARCH_GUEST_IRQ_ROUTES[physical_irq].store(IRQ_ROUTE_NONE, Ordering::Release);
            LOONGARCH_GUEST_IRQ_TARGETS[physical_irq].store(IRQ_TARGET_NONE, Ordering::Release);
            somehal::irq::irq_set_enable(physical_irq.into(), false);
            debug!("LoongArch dynamic guest IRQ route removed: physical_irq={physical_irq}");
        }
    }
}

#[cfg(all(target_arch = "riscv64", feature = "hv"))]
fn inject_virtual_irq(irq: usize) -> bool {
    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("virtual IRQ injector is not registered");
        return false;
    }
    unsafe { core::mem::transmute::<*mut (), fn(usize) -> bool>(injector)(irq) }
}

#[cfg(all(target_arch = "loongarch64", feature = "hv"))]
fn inject_loongarch_virtual_irq(physical_irq: usize) -> bool {
    if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
        if LOONGARCH_IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} out of range",
                physical_irq
            );
        }
        return false;
    }

    let encoded_vector = LOONGARCH_GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire);
    if encoded_vector == IRQ_ROUTE_NONE {
        if LOONGARCH_IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} has no route",
                physical_irq
            );
        }
        return false;
    }

    let encoded_target = LOONGARCH_GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
    if encoded_target == IRQ_TARGET_NONE {
        if LOONGARCH_IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} has no target",
                physical_irq
            );
        }
        return false;
    }

    let injector = LOONGARCH_VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("LoongArch virtual IRQ injector is not registered");
        return false;
    }

    let guest_vector = encoded_vector - 1;
    let vm_id = encoded_target >> IRQ_TARGET_VM_SHIFT;
    let vcpu_id = encoded_target & ((1usize << IRQ_TARGET_VM_SHIFT) - 1);
    if LOONGARCH_IRQ_INJECT_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
        trace!(
            "LoongArch guest IRQ inject: physical_irq={} -> VM[{}] VCpu[{}] guest_vector={}",
            physical_irq, vm_id, vcpu_id, guest_vector
        );
    }
    unsafe {
        core::mem::transmute::<*mut (), fn(usize, usize, usize, usize)>(injector)(
            vm_id,
            vcpu_id,
            guest_vector,
            physical_irq,
        );
    }
    true
}
