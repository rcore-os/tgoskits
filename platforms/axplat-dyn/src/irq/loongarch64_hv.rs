use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use ax_plat::irq::LoongArchHvIrqIf;

use super::IrqIfImpl;

const LOONGARCH_MAX_IRQ_COUNT: usize = 256;
const IRQ_ROUTE_NONE: usize = 0;
const IRQ_TARGET_NONE: usize = usize::MAX;
const IRQ_TARGET_VM_SHIFT: usize = 32;
const LOONGARCH_IRQ_TRACE_LIMIT: usize = 80;

static VIRTUAL_IRQ_INJECTOR: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
static GUEST_IRQ_ROUTES: [AtomicUsize; LOONGARCH_MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_ROUTE_NONE) }; LOONGARCH_MAX_IRQ_COUNT];
static GUEST_IRQ_TARGETS: [AtomicUsize; LOONGARCH_MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(IRQ_TARGET_NONE) }; LOONGARCH_MAX_IRQ_COUNT];
static IRQ_MISS_LOGS: AtomicUsize = AtomicUsize::new(0);
static IRQ_INJECT_LOGS: AtomicUsize = AtomicUsize::new(0);

#[impl_plat_interface]
impl LoongArchHvIrqIf for IrqIfImpl {
    fn register_virtual_irq_injector(injector: fn(usize, usize, usize, usize)) {
        VIRTUAL_IRQ_INJECTOR.store(injector as *mut (), Ordering::Release);
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
        let previous_target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
        if previous_target != IRQ_TARGET_NONE && previous_target != target {
            warn!(
                "LoongArch guest IRQ route conflict: physical_irq={} is already routed to encoded \
                 target {:#x}, ignoring VM[{}] VCpu[{}]",
                physical_irq, previous_target, vm_id, vcpu_id
            );
            return;
        }

        GUEST_IRQ_TARGETS[physical_irq].store(target, Ordering::Release);
        GUEST_IRQ_ROUTES[physical_irq].store(guest_vector + 1, Ordering::Release);
        somehal::irq::irq_set_enable(physical_irq.into(), true);
        debug!(
            "LoongArch dynamic guest IRQ route: physical_irq={}, target=VM[{}] VCpu[{}], \
             guest_vector={}",
            physical_irq, vm_id, vcpu_id, guest_vector
        );
    }

    fn unregister_guest_irq_routes(vm_id: usize) {
        for physical_irq in 0..LOONGARCH_MAX_IRQ_COUNT {
            let target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
            if target == IRQ_TARGET_NONE || (target >> IRQ_TARGET_VM_SHIFT) != vm_id {
                continue;
            }

            GUEST_IRQ_ROUTES[physical_irq].store(IRQ_ROUTE_NONE, Ordering::Release);
            GUEST_IRQ_TARGETS[physical_irq].store(IRQ_TARGET_NONE, Ordering::Release);
            somehal::irq::irq_set_enable(physical_irq.into(), false);
            debug!("LoongArch dynamic guest IRQ route removed: physical_irq={physical_irq}");
        }
    }
}

pub(super) fn inject_virtual_irq(physical_irq: usize) -> bool {
    if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
        if IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} out of range",
                physical_irq
            );
        }
        return false;
    }

    let encoded_vector = GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire);
    if encoded_vector == IRQ_ROUTE_NONE {
        if IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} has no route",
                physical_irq
            );
        }
        return false;
    }

    let encoded_target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
    if encoded_target == IRQ_TARGET_NONE {
        if IRQ_MISS_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
            trace!(
                "LoongArch guest IRQ route miss: physical_irq={} has no target",
                physical_irq
            );
        }
        return false;
    }

    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("LoongArch virtual IRQ injector is not registered");
        return false;
    }

    let guest_vector = encoded_vector - 1;
    let vm_id = encoded_target >> IRQ_TARGET_VM_SHIFT;
    let vcpu_id = encoded_target & ((1usize << IRQ_TARGET_VM_SHIFT) - 1);
    if IRQ_INJECT_LOGS.fetch_add(1, Ordering::Relaxed) < LOONGARCH_IRQ_TRACE_LIMIT {
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
