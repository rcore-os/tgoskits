use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use ax_plat::irq::{IrqId, IrqSource, LoongArchHvIrqIf};

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
static GUEST_IRQ_IN_FLIGHT: [AtomicUsize; LOONGARCH_MAX_IRQ_COUNT] =
    [const { AtomicUsize::new(0) }; LOONGARCH_MAX_IRQ_COUNT];
static IRQ_MISS_LOGS: AtomicUsize = AtomicUsize::new(0);
static IRQ_INJECT_LOGS: AtomicUsize = AtomicUsize::new(0);

struct InFlightGuestIrq<'a>(&'a AtomicUsize);

impl Drop for InFlightGuestIrq<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

fn resolve_physical_irq(physical_irq: usize) -> Result<IrqId, ax_plat::irq::IrqError> {
    let gsi = u32::try_from(physical_irq).map_err(|_| ax_plat::irq::IrqError::InvalidIrq)?;
    somehal::irq::resolve_irq_source(IrqSource::AcpiGsi(gsi))
}

fn set_physical_irq_enabled(
    physical_irq: usize,
    enabled: bool,
) -> Result<(), ax_plat::irq::IrqError> {
    let irq = resolve_physical_irq(physical_irq)?;
    somehal::irq::irq_set_enable(irq, enabled)
}

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
    ) -> Result<(), ax_plat::irq::IrqError> {
        if physical_irq >= LOONGARCH_MAX_IRQ_COUNT {
            return Err(ax_plat::irq::IrqError::InvalidIrq);
        }

        if vm_id > (usize::MAX >> IRQ_TARGET_VM_SHIFT) || vcpu_id >= (1usize << IRQ_TARGET_VM_SHIFT)
        {
            return Err(ax_plat::irq::IrqError::InvalidIrq);
        }
        let target = (vm_id << IRQ_TARGET_VM_SHIFT) | vcpu_id;
        if target == IRQ_TARGET_NONE {
            return Err(ax_plat::irq::IrqError::InvalidIrq);
        }
        let previous_target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
        let encoded_vector = guest_vector
            .checked_add(1)
            .ok_or(ax_plat::irq::IrqError::InvalidIrq)?;
        let previous_vector = GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire);
        if previous_target == target && previous_vector == encoded_vector {
            return Ok(());
        }
        if previous_target != IRQ_TARGET_NONE {
            return Err(ax_plat::irq::IrqError::Busy);
        }

        GUEST_IRQ_TARGETS[physical_irq].store(target, Ordering::Release);
        GUEST_IRQ_ROUTES[physical_irq].store(encoded_vector, Ordering::Release);
        if let Err(error) = set_physical_irq_enabled(physical_irq, true) {
            let _ = set_physical_irq_enabled(physical_irq, false);
            GUEST_IRQ_ROUTES[physical_irq].store(IRQ_ROUTE_NONE, Ordering::Release);
            return Err(error);
        }
        debug!(
            "LoongArch dynamic guest IRQ route: physical_irq={}, target=VM[{}] VCpu[{}], \
             guest_vector={}",
            physical_irq, vm_id, vcpu_id, guest_vector
        );
        Ok(())
    }

    fn begin_guest_irq_route_revocation(vm_id: usize) -> Result<(), ax_plat::irq::IrqError> {
        for physical_irq in 0..LOONGARCH_MAX_IRQ_COUNT {
            let target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
            if target == IRQ_TARGET_NONE || (target >> IRQ_TARGET_VM_SHIFT) != vm_id {
                continue;
            }

            set_physical_irq_enabled(physical_irq, false)?;
            GUEST_IRQ_ROUTES[physical_irq].store(IRQ_ROUTE_NONE, Ordering::Release);
            debug!("LoongArch guest IRQ route is draining: physical_irq={physical_irq}");
        }
        Ok(())
    }

    fn poll_guest_irq_route_revocation(vm_id: usize) -> Result<bool, ax_plat::irq::IrqError> {
        let mut drained = true;
        for physical_irq in 0..LOONGARCH_MAX_IRQ_COUNT {
            let target = GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire);
            if target == IRQ_TARGET_NONE || (target >> IRQ_TARGET_VM_SHIFT) != vm_id {
                continue;
            }
            if GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire) != IRQ_ROUTE_NONE
                || GUEST_IRQ_IN_FLIGHT[physical_irq].load(Ordering::Acquire) != 0
            {
                drained = false;
                continue;
            }
            if GUEST_IRQ_TARGETS[physical_irq]
                .compare_exchange(target, IRQ_TARGET_NONE, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                drained = false;
                continue;
            }
            debug!("LoongArch guest IRQ route drained: physical_irq={physical_irq}");
        }
        Ok(drained)
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

    let encoded_vector = GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire);
    if encoded_vector == IRQ_ROUTE_NONE {
        // A target without a route is in the mask-and-drain phase. Consume a
        // stale controller claim instead of exposing guest-owned hardware to
        // the host IRQ framework.
        return true;
    }

    GUEST_IRQ_IN_FLIGHT[physical_irq].fetch_add(1, Ordering::AcqRel);
    let _in_flight = InFlightGuestIrq(&GUEST_IRQ_IN_FLIGHT[physical_irq]);
    if GUEST_IRQ_TARGETS[physical_irq].load(Ordering::Acquire) != encoded_target
        || GUEST_IRQ_ROUTES[physical_irq].load(Ordering::Acquire) != encoded_vector
    {
        return true;
    }

    let injector = VIRTUAL_IRQ_INJECTOR.load(Ordering::Acquire);
    if injector.is_null() {
        warn!("LoongArch virtual IRQ injector is not registered");
        return true;
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
