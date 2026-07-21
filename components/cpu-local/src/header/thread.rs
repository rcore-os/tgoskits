use core::{
    cell::UnsafeCell,
    fmt,
    mem::{offset_of, size_of},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use crate::{CpuBindingV1, CpuIndex, image_register_mode};

/// Generation-bearing scheduler thread identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(C)]
pub struct ThreadIdentity {
    slot: u32,
    generation: u32,
}

impl ThreadIdentity {
    const UNBOUND: Self = Self {
        slot: CpuIndex::INVALID_RAW,
        generation: 0,
    };

    /// Constructs an identity from scheduler-owned parts.
    pub const fn from_parts(slot: u32, generation: u32) -> Option<Self> {
        if slot == CpuIndex::INVALID_RAW || generation == 0 {
            None
        } else {
            Some(Self { slot, generation })
        }
    }

    /// Returns the scheduler registry slot.
    pub const fn slot(self) -> u32 {
        self.slot
    }

    /// Returns the slot reuse generation.
    pub const fn generation(self) -> u32 {
        self.generation
    }

    const fn boot(cpu_index: CpuIndex) -> Self {
        Self {
            slot: cpu_index.as_u32(),
            generation: u32::MAX,
        }
    }

    const fn is_unbound(self) -> bool {
        self.slot == CpuIndex::INVALID_RAW && self.generation == 0
    }
}

/// Stable identity of one runtime-owned architecture context.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct ContextIdentity(usize);

impl ContextIdentity {
    /// Converts a non-null opaque execution-context handle.
    pub const fn from_raw(raw: usize) -> Option<Self> {
        if raw == 0 { None } else { Some(Self(raw)) }
    }

    /// Returns the opaque scalar representation.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Stable bound phase returned to the scheduler switch tail.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct CpuBindingEpoch(usize);

impl CpuBindingEpoch {
    /// Returns the raw phase/generation word.
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

/// Coherent acquire snapshot of a task-owned header's live CPU binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CurrentCpuBinding {
    area_base: usize,
    cpu_index: CpuIndex,
    epoch: CpuBindingEpoch,
}

impl CurrentCpuBinding {
    /// Returns the direct CPU-area base.
    pub const fn area_base(self) -> usize {
        self.area_base
    }

    /// Returns the logical CPU index.
    pub const fn cpu_index(self) -> CpuIndex {
        self.cpu_index
    }

    /// Returns the exact stable-bound phase word.
    pub const fn epoch(self) -> CpuBindingEpoch {
        self.epoch
    }
}

const THREAD_UNBOUND: u32 = 0;
const THREAD_INITIALIZING: u32 = 1;
const THREAD_BOUND: u32 = 2;

const CPU_PHASE_MASK: usize = 0b11;
const CPU_UNBOUND: usize = 0b00;
const CPU_BINDING: usize = 0b01;
const CPU_BOUND: usize = 0b10;
const CPU_UNBINDING: usize = 0b11;

const fn current_thread_reserved_size() -> usize {
    if size_of::<usize>() == 8 { 0 } else { 28 }
}

pub(super) const fn cpu_header_reserved_size() -> usize {
    if size_of::<usize>() == 8 { 24 } else { 40 }
}

/// Pinned scheduler/architecture identity header for one execution context.
///
/// Stacks, kernel TLS, address spaces, and `TaskContext` remain in the
/// runtime-owned object containing this header. Thread identity is initialized
/// exactly once after pinning. CPU binding uses a four-phase publication word:
/// `Unbound -> Binding -> Bound -> Unbinding -> next Unbound`.
#[repr(C, align(64))]
pub struct CurrentThreadHeader {
    thread_identity: UnsafeCell<ThreadIdentity>,
    context_identity: ContextIdentity,
    thread_state: AtomicU32,
    cpu_base: AtomicUsize,
    cpu_index: AtomicU32,
    binding_epoch: AtomicUsize,
    trap_scratch0: UnsafeCell<usize>,
    trap_scratch1: UnsafeCell<usize>,
    reserved: [u8; current_thread_reserved_size()],
}

// SAFETY: `thread_identity` is written only by the successful one-time state
// owner and published by `thread_state` Release. Every reader first observes
// THREAD_BOUND with Acquire. Later binding fields are atomic. The two trap
// scratch words are owned only by assembly on the CPU currently bound to this
// header and are never exposed through safe Rust.
unsafe impl Sync for CurrentThreadHeader {}

impl CurrentThreadHeader {
    pub(super) const fn empty() -> Self {
        Self {
            thread_identity: UnsafeCell::new(ThreadIdentity::UNBOUND),
            context_identity: ContextIdentity(0),
            thread_state: AtomicU32::new(THREAD_UNBOUND),
            cpu_base: AtomicUsize::new(0),
            cpu_index: AtomicU32::new(CpuIndex::INVALID_RAW),
            binding_epoch: AtomicUsize::new(CPU_UNBOUND),
            trap_scratch0: UnsafeCell::new(0),
            trap_scratch1: UnsafeCell::new(0),
            reserved: [0; current_thread_reserved_size()],
        }
    }

    /// Creates an unbound header before placing it in pinned runtime storage.
    pub const fn new(context_identity: ContextIdentity) -> Self {
        Self {
            context_identity,
            ..Self::empty()
        }
    }

    pub(super) const fn boot(cpu_index: CpuIndex, area_base: usize) -> Self {
        Self {
            thread_identity: UnsafeCell::new(ThreadIdentity::boot(cpu_index)),
            // The permanent boot header is CPU-owned and has no runtime task.
            // Keep the opaque context null so bootstrap callers cannot mistake
            // it for an `Arc`-backed scheduler object.
            context_identity: ContextIdentity(0),
            thread_state: AtomicU32::new(THREAD_BOUND),
            cpu_base: AtomicUsize::new(area_base),
            cpu_index: AtomicU32::new(cpu_index.as_u32()),
            binding_epoch: AtomicUsize::new(CPU_BOUND),
            ..Self::empty()
        }
    }

    /// Publishes the scheduler identity exactly once after the header is pinned.
    ///
    /// A failed call performs no write to the identity storage.
    pub fn bind_thread(
        self: Pin<&Self>,
        thread_identity: ThreadIdentity,
    ) -> Result<(), CurrentThreadError> {
        if thread_identity.is_unbound() {
            return Err(CurrentThreadError::InvalidThreadIdentity);
        }
        let this = self.get_ref();
        this.thread_state
            .compare_exchange(
                THREAD_UNBOUND,
                THREAD_INITIALIZING,
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .map_err(|_| CurrentThreadError::ThreadAlreadyBound)?;
        // SAFETY: the successful state transition grants this caller the only
        // write to the storage, and no reader accepts THREAD_INITIALIZING.
        unsafe { this.thread_identity.get().write(thread_identity) };
        this.thread_state.store(THREAD_BOUND, Ordering::Release);
        Ok(())
    }

    /// Acquires the generation-bearing identity after one-time publication.
    pub fn thread_identity(&self) -> Option<ThreadIdentity> {
        if self.thread_state.load(Ordering::Acquire) != THREAD_BOUND {
            return None;
        }
        // SAFETY: THREAD_BOUND Acquire observes the one-time initialized value,
        // which remains immutable for the pinned header lifetime.
        Some(unsafe { *self.thread_identity.get() })
    }

    /// Returns the immutable runtime context identity.
    ///
    /// The permanent CPU-owned boot header returns the null identity because
    /// it does not correspond to a runtime-owned execution context.
    pub const fn context_identity(&self) -> ContextIdentity {
        self.context_identity
    }

    /// Validates immutable context and scheduler identities.
    pub fn validate_identity(
        &self,
        context_identity: ContextIdentity,
        thread_identity: ThreadIdentity,
    ) -> Result<(), CurrentThreadError> {
        if self.context_identity != context_identity {
            return Err(CurrentThreadError::ContextIdentityMismatch);
        }
        if self.thread_identity() != Some(thread_identity) {
            return Err(CurrentThreadError::ThreadIdentityMismatch);
        }
        Ok(())
    }

    /// Publishes a CPU binding with a four-phase Release/Acquire protocol.
    ///
    /// # Safety
    ///
    /// Only the scheduler may call this while the context is not on a CPU.
    /// The header must remain pinned, and local IRQs must stay disabled until
    /// the CPU current slot and LinuxCurrent register publish this header.
    pub unsafe fn bind_cpu(
        self: Pin<&Self>,
        binding: CpuBindingV1,
    ) -> Result<CpuBindingEpoch, CurrentThreadError> {
        if self.thread_identity().is_none() {
            return Err(CurrentThreadError::ThreadUnbound);
        }
        let Some(validated) = binding.validated() else {
            return Err(CurrentThreadError::InvalidCpuBinding);
        };
        if validated.register_mode() != Some(image_register_mode()) {
            return Err(CurrentThreadError::InvalidCpuBinding);
        }
        let cpu_index = validated
            .cpu_index()
            .ok_or(CurrentThreadError::InvalidCpuBinding)?;

        let this = self.get_ref();
        let unbound = this.binding_epoch.load(Ordering::Acquire);
        if unbound & CPU_PHASE_MASK != CPU_UNBOUND {
            return Err(CurrentThreadError::CpuAlreadyBound);
        }
        this.binding_epoch
            .compare_exchange(
                unbound,
                unbound | CPU_BINDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| CurrentThreadError::CpuAlreadyBound)?;
        this.cpu_base.store(binding.area_base, Ordering::Relaxed);
        this.cpu_index.store(cpu_index.as_u32(), Ordering::Relaxed);
        let bound = (unbound & !CPU_PHASE_MASK) | CPU_BOUND;
        this.binding_epoch.store(bound, Ordering::Release);
        Ok(CpuBindingEpoch(bound))
    }

    /// Withdraws the CPU binding after all current-register publication ends.
    ///
    /// # Safety
    ///
    /// Only the switch tail owning the exact epoch returned by [`Self::bind_cpu`]
    /// may call this. The header must no longer be current on any CPU.
    pub unsafe fn unbind_cpu(
        self: Pin<&Self>,
        expected: CpuBindingEpoch,
    ) -> Result<(), CurrentThreadError> {
        if expected.0 & CPU_PHASE_MASK != CPU_BOUND {
            return Err(CurrentThreadError::BindingEpochMismatch);
        }
        let this = self.get_ref();
        let unbinding = (expected.0 & !CPU_PHASE_MASK) | CPU_UNBINDING;
        this.binding_epoch
            .compare_exchange(expected.0, unbinding, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CurrentThreadError::BindingEpochMismatch)?;
        this.cpu_base.store(0, Ordering::Relaxed);
        this.cpu_index
            .store(CpuIndex::INVALID_RAW, Ordering::Relaxed);
        let next_unbound = (expected.0 & !CPU_PHASE_MASK).wrapping_add(4);
        this.binding_epoch.store(next_unbound, Ordering::Release);
        Ok(())
    }

    /// Acquires a coherent stable-bound CPU snapshot.
    pub fn cpu_binding(&self) -> Option<CurrentCpuBinding> {
        loop {
            let before = self.binding_epoch.load(Ordering::Acquire);
            if before & CPU_PHASE_MASK != CPU_BOUND {
                return None;
            }
            let area_base = self.cpu_base.load(Ordering::Relaxed);
            let cpu_index = self.cpu_index.load(Ordering::Relaxed);
            let after = self.binding_epoch.load(Ordering::Acquire);
            if before == after {
                return Some(CurrentCpuBinding {
                    area_base,
                    cpu_index: CpuIndex::from_u32(cpu_index)?,
                    epoch: CpuBindingEpoch(after),
                });
            }
            core::hint::spin_loop();
        }
    }

    /// Returns the stable pointer installed in the current-thread register.
    pub fn as_non_null(self: Pin<&Self>) -> NonNull<Self> {
        NonNull::from(self.get_ref())
    }
}

/// Rejected current-thread identity or CPU publication transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CurrentThreadError {
    /// Reserved scheduler identity.
    InvalidThreadIdentity,
    /// Thread identity already initialized.
    ThreadAlreadyBound,
    /// Thread identity not initialized.
    ThreadUnbound,
    /// Runtime context identity mismatch.
    ContextIdentityMismatch,
    /// Scheduler identity mismatch.
    ThreadIdentityMismatch,
    /// Malformed platform CPU binding.
    InvalidCpuBinding,
    /// Header is already bound or in transition.
    CpuAlreadyBound,
    /// Stale or non-bound switch-tail epoch.
    BindingEpochMismatch,
    /// Header belongs to another CPU.
    CpuBindingMismatch,
    /// Current-thread pointer is null or misaligned.
    InvalidCurrentThread,
}

impl fmt::Display for CurrentThreadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidThreadIdentity => "thread identity is invalid",
            Self::ThreadAlreadyBound => "thread identity is already bound",
            Self::ThreadUnbound => "thread identity is not bound",
            Self::ContextIdentityMismatch => "runtime context identity mismatch",
            Self::ThreadIdentityMismatch => "scheduler thread identity mismatch",
            Self::InvalidCpuBinding => "platform CPU binding is invalid",
            Self::CpuAlreadyBound => "current-thread header is already CPU-bound",
            Self::BindingEpochMismatch => "CPU binding epoch mismatch",
            Self::CpuBindingMismatch => "current-thread header belongs to another CPU",
            Self::InvalidCurrentThread => "current-thread pointer is invalid",
        })
    }
}

impl core::error::Error for CurrentThreadError {}

/// Byte offset of the current header's first architecture entry scratch word.
pub const CURRENT_THREAD_TRAP_SCRATCH0_OFFSET: usize =
    offset_of!(CurrentThreadHeader, trap_scratch0);
/// Byte offset of the current header's second architecture entry scratch word.
pub const CURRENT_THREAD_TRAP_SCRATCH1_OFFSET: usize =
    offset_of!(CurrentThreadHeader, trap_scratch1);
/// Byte offset of the current header's bound CPU-area base.
pub const CURRENT_THREAD_CPU_BASE_OFFSET: usize = offset_of!(CurrentThreadHeader, cpu_base);
