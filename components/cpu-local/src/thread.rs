use core::{
    mem::{offset_of, size_of},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{CpuAreaRef, ThreadSwitchError};

/// Stable opaque identity of one runtime-owned execution context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CurrentContext(usize);

impl CurrentContext {
    /// Converts a non-null opaque execution-context handle.
    pub const fn from_raw(raw: usize) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// Returns the opaque scalar representation.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CpuBindingEpoch(usize);

#[derive(Clone, Copy, Debug)]
pub(crate) struct CurrentCpuBinding {
    pub(crate) area: CpuAreaRef,
    pub(crate) epoch: CpuBindingEpoch,
}

const CPU_PHASE_MASK: usize = 0b11;
const CPU_UNBOUND: usize = 0b00;
const CPU_BINDING: usize = 0b01;
const CPU_BOUND: usize = 0b10;
const CPU_UNBINDING: usize = 0b11;

const fn current_thread_reserved_size() -> usize {
    64 - 5 * size_of::<usize>()
}

/// Pinned scheduler/architecture header for one execution context.
///
/// CPU binding uses a four-phase publication word:
/// `Unbound -> Binding -> Bound -> Unbinding -> next Unbound`. The epoch is
/// retained solely to reject a stale incoming switch tail.
#[repr(C, align(64))]
pub struct CurrentThreadHeader {
    context: usize,
    cpu_area: AtomicUsize,
    binding_epoch: AtomicUsize,
    architecture_state: [AtomicUsize; 2],
    reserved: [u8; current_thread_reserved_size()],
}

impl CurrentThreadHeader {
    /// Creates an unbound header before placing it in stable pinned storage.
    pub const fn new(context: CurrentContext) -> Self {
        Self {
            context: context.0,
            cpu_area: AtomicUsize::new(0),
            binding_epoch: AtomicUsize::new(CPU_UNBOUND),
            architecture_state: [const { AtomicUsize::new(0) }; 2],
            reserved: [0; current_thread_reserved_size()],
        }
    }

    pub(crate) const fn boot(area_base: usize) -> Self {
        Self {
            context: 0,
            cpu_area: AtomicUsize::new(area_base),
            binding_epoch: AtomicUsize::new(CPU_BOUND),
            architecture_state: [const { AtomicUsize::new(0) }; 2],
            reserved: [0; current_thread_reserved_size()],
        }
    }

    /// Returns the immutable runtime context identity, if this is a task.
    pub const fn current_context(&self) -> Option<CurrentContext> {
        CurrentContext::from_raw(self.context)
    }

    /// Returns the stable CPU area while this header is fully bound.
    pub fn cpu_area(&self) -> Option<CpuAreaRef> {
        self.cpu_binding().map(|binding| binding.area)
    }

    /// Returns the raw bound area base used by architecture trap entry.
    pub fn cpu_area_base(&self) -> Option<usize> {
        self.cpu_binding().map(|binding| binding.area.base())
    }

    pub(crate) unsafe fn bind_cpu(
        self: Pin<&Self>,
        area: CpuAreaRef,
    ) -> Result<CpuBindingEpoch, ThreadSwitchError> {
        let this = self.get_ref();
        let unbound = this.binding_epoch.load(Ordering::Acquire);
        if unbound & CPU_PHASE_MASK != CPU_UNBOUND {
            return Err(ThreadSwitchError::NextThreadAlreadyBound);
        }
        this.binding_epoch
            .compare_exchange(
                unbound,
                unbound | CPU_BINDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| ThreadSwitchError::NextThreadAlreadyBound)?;
        this.cpu_area.store(area.base(), Ordering::Relaxed);
        let bound = (unbound & !CPU_PHASE_MASK) | CPU_BOUND;
        this.binding_epoch.store(bound, Ordering::Release);
        Ok(CpuBindingEpoch(bound))
    }

    pub(crate) unsafe fn unbind_cpu(
        self: Pin<&Self>,
        expected: CpuBindingEpoch,
    ) -> Result<(), ThreadSwitchError> {
        if expected.0 & CPU_PHASE_MASK != CPU_BOUND {
            return Err(ThreadSwitchError::StalePreviousBinding);
        }
        let this = self.get_ref();
        let unbinding = (expected.0 & !CPU_PHASE_MASK) | CPU_UNBINDING;
        this.binding_epoch
            .compare_exchange(expected.0, unbinding, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ThreadSwitchError::StalePreviousBinding)?;
        this.cpu_area.store(0, Ordering::Relaxed);
        let next_unbound = (expected.0 & !CPU_PHASE_MASK).wrapping_add(4);
        this.binding_epoch.store(next_unbound, Ordering::Release);
        Ok(())
    }

    pub(crate) fn cpu_binding(&self) -> Option<CurrentCpuBinding> {
        let (area_base, epoch) = self.raw_cpu_binding()?;
        // SAFETY: only bind_cpu can publish this field, and it accepts an
        // already validated shutdown-lifetime CpuAreaRef.
        let area = unsafe { CpuAreaRef::from_initialized_base(area_base) }.ok()?;
        Some(CurrentCpuBinding { area, epoch })
    }

    pub(crate) fn raw_cpu_binding(&self) -> Option<(usize, CpuBindingEpoch)> {
        loop {
            let before = self.binding_epoch.load(Ordering::Acquire);
            if before & CPU_PHASE_MASK != CPU_BOUND {
                return None;
            }
            let area_base = self.cpu_area.load(Ordering::Relaxed);
            let after = self.binding_epoch.load(Ordering::Acquire);
            if before == after {
                return Some((area_base, CpuBindingEpoch(after)));
            }
            core::hint::spin_loop();
        }
    }

    /// Returns the stable pointer installed in the current-thread register.
    pub fn as_non_null(self: Pin<&Self>) -> NonNull<Self> {
        NonNull::from(self.get_ref())
    }
}

/// Byte offset of the current header's bound CPU-area base.
pub const CURRENT_THREAD_CPU_BASE_OFFSET: usize = offset_of!(CurrentThreadHeader, cpu_area);
/// Byte offset of architecture-owned task trap state.
pub const CURRENT_THREAD_ARCH_STATE_OFFSET: usize =
    offset_of!(CurrentThreadHeader, architecture_state);
/// Reserved bytes available to architecture-owned task trap state.
pub const CURRENT_THREAD_ARCH_STATE_SIZE: usize = 2 * size_of::<usize>();

const _: () = {
    assert!(size_of::<CurrentThreadHeader>() == 64);
    assert!(core::mem::align_of::<CurrentThreadHeader>() == 64);
};
