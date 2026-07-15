use core::{
    cell::UnsafeCell,
    fmt,
    mem::{align_of, offset_of, size_of},
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
};

use crate::{
    CPU_LOCAL_ABI_VERSION, CpuBindingV1, CpuIndex, HostLevelV1, RegisterModeV1, image_register_mode,
};

/// Default nonzero identity cookie for compatibility CPU-area layouts.
pub const CPU_AREA_DEFAULT_COOKIE: usize = 0x4158_4350;

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

    const fn boot(cpu_index: CpuIndex) -> Self {
        Self(usize::MAX - cpu_index.as_usize())
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

const fn cpu_header_reserved_size() -> usize {
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
    const fn empty() -> Self {
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

    const fn boot(cpu_index: CpuIndex, area_base: usize) -> Self {
        Self {
            thread_identity: UnsafeCell::new(ThreadIdentity::boot(cpu_index)),
            context_identity: ContextIdentity::boot(cpu_index),
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

/// CPU-local scalar state used by trap entry and scheduler publication.
#[repr(C, align(64))]
pub struct CpuRuntimeAnchor {
    current_thread: AtomicUsize,
    kernel_stack_pointer: AtomicUsize,
    user_trap_frame: AtomicUsize,
    scratch0: AtomicUsize,
    scratch1: AtomicUsize,
    reserved: [u8; 64 - 5 * size_of::<usize>()],
}

impl CpuRuntimeAnchor {
    const fn empty() -> Self {
        Self {
            current_thread: AtomicUsize::new(0),
            kernel_stack_pointer: AtomicUsize::new(0),
            user_trap_frame: AtomicUsize::new(0),
            scratch0: AtomicUsize::new(0),
            scratch1: AtomicUsize::new(0),
            reserved: [0; 64 - 5 * size_of::<usize>()],
        }
    }

    const fn for_boot_thread(boot_thread: usize) -> Self {
        Self {
            current_thread: AtomicUsize::new(boot_thread),
            ..Self::empty()
        }
    }

    /// Acquires the pinned current-thread pointer.
    pub fn current_thread_raw(&self) -> usize {
        self.current_thread.load(Ordering::Acquire)
    }

    pub(crate) const fn current_thread_slot(&self) -> &AtomicUsize {
        &self.current_thread
    }

    /// Publishes a validated pinned current-thread pointer.
    ///
    /// # Safety
    ///
    /// The pointer must remain pinned and bound to this CPU until withdrawn by
    /// the serialized IRQ-disabled scheduler switch path.
    pub unsafe fn publish_current_thread_raw(&self, current_thread: usize) {
        self.current_thread.store(current_thread, Ordering::Release);
    }

    /// Loads the kernel continuation stack used by trap entry.
    pub fn kernel_stack_pointer(&self) -> usize {
        self.kernel_stack_pointer.load(Ordering::Acquire)
    }

    /// Publishes the kernel continuation stack while trap entry is excluded.
    ///
    /// # Safety
    ///
    /// Local traps must not consume the value until publication completes.
    pub unsafe fn set_kernel_stack_pointer(&self, value: usize) {
        self.kernel_stack_pointer.store(value, Ordering::Release);
    }

    /// Loads the current user trap-frame pointer.
    pub fn user_trap_frame(&self) -> usize {
        self.user_trap_frame.load(Ordering::Acquire)
    }

    /// Publishes the user trap-frame pointer while trap entry is excluded.
    ///
    /// # Safety
    ///
    /// `value` must remain valid until a later serialized replacement.
    pub unsafe fn set_user_trap_frame(&self, value: usize) {
        self.user_trap_frame.store(value, Ordering::Release);
    }

    /// Loads scratch word zero.
    pub fn scratch0(&self) -> usize {
        self.scratch0.load(Ordering::Acquire)
    }

    /// Stores scratch word zero in a trap-excluded section.
    ///
    /// # Safety
    ///
    /// Architecture entry must not concurrently own the scratch word.
    pub unsafe fn set_scratch0(&self, value: usize) {
        self.scratch0.store(value, Ordering::Release);
    }

    /// Loads scratch word one.
    pub fn scratch1(&self) -> usize {
        self.scratch1.load(Ordering::Acquire)
    }

    /// Stores scratch word one in a trap-excluded section.
    ///
    /// # Safety
    ///
    /// Architecture entry must not concurrently own the scratch word.
    pub unsafe fn set_scratch1(&self, value: usize) {
        self.scratch1.store(value, Ordering::Release);
    }
}

/// Compatibility name for architecture code using the original line-1 API.
pub type CpuEntryScratch = CpuRuntimeAnchor;

/// Permanent current header used before the scheduler publishes a task.
#[repr(transparent)]
pub struct BootThreadHeader(CurrentThreadHeader);

impl BootThreadHeader {
    const fn empty() -> Self {
        Self(CurrentThreadHeader::empty())
    }

    const fn for_cpu(cpu_index: CpuIndex, area_base: usize) -> Self {
        Self(CurrentThreadHeader::boot(cpu_index, area_base))
    }

    /// Returns the permanent pinned header.
    pub const fn header(&self) -> &CurrentThreadHeader {
        &self.0
    }
}

/// Value-only facts that construct one final CPU-area prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CpuAreaInitV2 {
    /// CPU-local ABI version.
    pub abi_version: u16,
    /// Final-image register ownership mode.
    pub register_mode: u8,
    /// Privileged host level.
    pub host_level: u8,
    /// Logical CPU index.
    pub cpu_index: u32,
    /// Frozen layout generation.
    pub generation: u32,
    /// Direct runtime base of the area.
    pub area_base: usize,
    /// Permanent boot current-thread header pointer.
    pub boot_thread: usize,
    /// Frozen layout identity cookie.
    pub cookie: usize,
}

impl CpuAreaInitV2 {
    /// Constructs typed initialization facts.
    pub const fn new(
        register_mode: RegisterModeV1,
        host_level: HostLevelV1,
        cpu_index: CpuIndex,
        generation: u32,
        area_base: usize,
        boot_thread: usize,
        cookie: usize,
    ) -> Self {
        Self {
            abi_version: CPU_LOCAL_ABI_VERSION,
            register_mode: register_mode.as_u8(),
            host_level: host_level.as_u8(),
            cpu_index: cpu_index.as_u32(),
            generation,
            area_base,
            boot_thread,
            cookie,
        }
    }

    /// Converts a validated frozen platform binding to area-init facts.
    pub const fn from_binding(binding: CpuBindingV1) -> Option<Self> {
        if binding.validated().is_none() {
            return None;
        }
        Some(Self {
            abi_version: binding.abi_version,
            register_mode: binding.register_mode,
            host_level: binding.host_level,
            cpu_index: binding.cpu_index,
            generation: binding.generation,
            area_base: binding.area_base,
            boot_thread: binding.boot_thread,
            cookie: binding.cookie,
        })
    }

    /// Converts these frozen facts to the platform ABI binding.
    pub const fn binding(self) -> CpuBindingV1 {
        CpuBindingV1 {
            abi_version: self.abi_version,
            register_mode: self.register_mode,
            host_level: self.host_level,
            cpu_index: self.cpu_index,
            generation: self.generation,
            area_base: self.area_base,
            boot_thread: self.boot_thread,
            cookie: self.cookie,
        }
    }

    fn validate(self) -> Result<CpuIndex, CpuAreaInitError> {
        let binding = self.binding();
        let cpu_index = binding.cpu_index().ok_or(CpuAreaInitError::CpuIndex)?;
        if binding.abi_version != CPU_LOCAL_ABI_VERSION {
            return Err(CpuAreaInitError::AbiVersion);
        }
        if binding.register_mode().is_none() {
            return Err(CpuAreaInitError::RegisterMode);
        }
        if binding.host_level().is_none() {
            return Err(CpuAreaInitError::HostLevel);
        }
        if binding.area_base == 0
            || !binding
                .area_base
                .is_multiple_of(align_of::<CpuAreaPrefixV2>())
        {
            return Err(CpuAreaInitError::AreaBase);
        }
        if binding.boot_thread
            != binding
                .area_base
                .checked_add(CPU_AREA_BOOT_THREAD_OFFSET)
                .ok_or(CpuAreaInitError::AddressOverflow)?
        {
            return Err(CpuAreaInitError::BootThread);
        }
        if binding.generation == 0 {
            return Err(CpuAreaInitError::Generation);
        }
        if binding.cookie == 0 {
            return Err(CpuAreaInitError::Cookie);
        }
        Ok(cpu_index)
    }
}

/// Immutable identity of one initialized CPU-local area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C, align(64))]
pub struct CpuAreaHeader {
    abi_version: u16,
    register_mode: u8,
    host_level: u8,
    cpu_index: u32,
    generation: u32,
    self_base: usize,
    boot_thread: usize,
    cookie: usize,
    reserved: [u8; cpu_header_reserved_size()],
}

impl CpuAreaHeader {
    const TEMPLATE: Self = Self {
        abi_version: CPU_LOCAL_ABI_VERSION,
        register_mode: 0,
        host_level: 0,
        cpu_index: CpuIndex::INVALID_RAW,
        generation: 0,
        self_base: 0,
        boot_thread: 0,
        cookie: 0,
        reserved: [0; cpu_header_reserved_size()],
    };

    const fn from_init(init: CpuAreaInitV2) -> Self {
        Self {
            abi_version: init.abi_version,
            register_mode: init.register_mode,
            host_level: init.host_level,
            cpu_index: init.cpu_index,
            generation: init.generation,
            self_base: init.area_base,
            boot_thread: init.boot_thread,
            cookie: init.cookie,
            reserved: [0; cpu_header_reserved_size()],
        }
    }

    /// Returns the complete frozen platform binding.
    pub const fn binding(&self) -> CpuBindingV1 {
        CpuBindingV1 {
            abi_version: self.abi_version,
            register_mode: self.register_mode,
            host_level: self.host_level,
            cpu_index: self.cpu_index,
            generation: self.generation,
            area_base: self.self_base,
            boot_thread: self.boot_thread,
            cookie: self.cookie,
        }
    }

    /// Returns the CPU index recorded in this header.
    pub const fn cpu_index(&self) -> Option<CpuIndex> {
        CpuIndex::from_u32(self.cpu_index)
    }

    /// Returns the direct runtime area base.
    pub const fn self_base(&self) -> usize {
        self.self_base
    }

    /// Returns the frozen layout generation.
    pub const fn generation(&self) -> u32 {
        self.generation
    }

    /// Returns the frozen layout cookie.
    pub const fn cookie(&self) -> usize {
        self.cookie
    }

    /// Returns the CPU-area ABI version.
    pub const fn abi_version(&self) -> u16 {
        self.abi_version
    }

    /// Returns the final-image register mode.
    pub const fn register_mode(&self) -> Option<RegisterModeV1> {
        RegisterModeV1::try_from_raw(self.register_mode)
    }

    /// Returns the privileged host level.
    pub const fn host_level(&self) -> Option<HostLevelV1> {
        HostLevelV1::try_from_raw(self.host_level)
    }

    /// Returns whether this is the uninitialized template header.
    pub const fn is_unbound(&self) -> bool {
        self.self_base == 0 && self.generation == 0 && self.cookie == 0
    }

    /// Validates every frozen scalar fact.
    pub fn validate_init(&self, init: CpuAreaInitV2) -> Result<(), CpuAreaHeaderError> {
        if self.abi_version != init.abi_version {
            return Err(CpuAreaHeaderError::AbiVersion);
        }
        if self.cpu_index != init.cpu_index {
            return Err(CpuAreaHeaderError::CpuIndex);
        }
        if self.self_base != init.area_base {
            return Err(CpuAreaHeaderError::Anchor);
        }
        if self.boot_thread != init.boot_thread {
            return Err(CpuAreaHeaderError::BootThread);
        }
        if self.generation != init.generation {
            return Err(CpuAreaHeaderError::Generation);
        }
        if self.cookie != init.cookie {
            return Err(CpuAreaHeaderError::Cookie);
        }
        if self.register_mode != init.register_mode {
            return Err(CpuAreaHeaderError::RegisterMode);
        }
        if self.host_level != init.host_level {
            return Err(CpuAreaHeaderError::HostLevel);
        }
        Ok(())
    }
}

/// Fixed three-cache-line prefix at offset zero of every CPU-local area.
#[repr(C, align(64))]
pub struct CpuAreaPrefixV2 {
    header: CpuAreaHeader,
    runtime: CpuRuntimeAnchor,
    boot_thread: BootThreadHeader,
}

/// Compatibility name for consumers accepting the current prefix ABI.
pub type CpuAreaPrefix = CpuAreaPrefixV2;

impl CpuAreaPrefixV2 {
    /// Creates the linker template replaced by final-high typed initialization.
    pub const fn template() -> Self {
        Self {
            header: CpuAreaHeader::TEMPLATE,
            runtime: CpuRuntimeAnchor::empty(),
            boot_thread: BootThreadHeader::empty(),
        }
    }

    /// Constructs the only final prefix image written to one offline area.
    pub fn initialize(init: CpuAreaInitV2) -> Result<Self, CpuAreaInitError> {
        let cpu_index = init.validate()?;
        Ok(Self {
            header: CpuAreaHeader::from_init(init),
            runtime: CpuRuntimeAnchor::for_boot_thread(init.boot_thread),
            boot_thread: BootThreadHeader::for_cpu(cpu_index, init.area_base),
        })
    }

    /// Compatibility constructor for supervisor-mode fixtures.
    pub fn for_area(cpu_index: CpuIndex, area_base: usize, generation: u32, cookie: usize) -> Self {
        Self::initialize(CpuAreaInitV2::new(
            image_register_mode(),
            HostLevelV1::Supervisor,
            cpu_index,
            generation,
            area_base,
            area_base + CPU_AREA_BOOT_THREAD_OFFSET,
            cookie,
        ))
        .expect("CPU-area fixture requires validated initialization facts")
    }

    /// Returns immutable CPU-area identity.
    pub const fn header(&self) -> &CpuAreaHeader {
        &self.header
    }

    /// Returns CPU runtime/trap state.
    pub const fn runtime_anchor(&self) -> &CpuRuntimeAnchor {
        &self.runtime
    }

    /// Returns the permanent boot current-thread header.
    pub const fn boot_thread(&self) -> &BootThreadHeader {
        &self.boot_thread
    }

    /// Compatibility accessor for the original line-1 name.
    pub const fn entry_scratch(&self) -> &CpuEntryScratch {
        &self.runtime
    }

    /// Validates immutable prefix identity.
    pub fn validate_init(&self, init: CpuAreaInitV2) -> Result<(), CpuAreaHeaderError> {
        self.header.validate_init(init)
    }
}

/// Size in bytes of the immutable [`CpuAreaHeader`].
pub const CPU_AREA_HEADER_SIZE: usize = size_of::<CpuAreaHeader>();
/// Byte offset of the immutable header.
pub const CPU_AREA_HEADER_OFFSET: usize = offset_of!(CpuAreaPrefixV2, header);
/// Byte offset of CPU runtime/trap state.
pub const CPU_AREA_RUNTIME_ANCHOR_OFFSET: usize = offset_of!(CpuAreaPrefixV2, runtime);
/// Compatibility name for line-1 runtime/trap state.
pub const CPU_AREA_ENTRY_OFFSET: usize = CPU_AREA_RUNTIME_ANCHOR_OFFSET;
/// Byte offset of the permanent boot current-thread header.
pub const CPU_AREA_BOOT_THREAD_OFFSET: usize = offset_of!(CpuAreaPrefixV2, boot_thread);
/// Byte offset of the runtime self pointer.
pub const CPU_AREA_SELF_BASE_OFFSET: usize = offset_of!(CpuAreaHeader, self_base);
/// Byte offset of the permanent boot current-thread pointer.
pub const CPU_AREA_BOOT_THREAD_POINTER_OFFSET: usize = offset_of!(CpuAreaHeader, boot_thread);
/// Byte offset of the logical CPU index.
pub const CPU_AREA_CPU_INDEX_OFFSET: usize = offset_of!(CpuAreaHeader, cpu_index);
/// Byte offset of the layout generation.
pub const CPU_AREA_GENERATION_OFFSET: usize = offset_of!(CpuAreaHeader, generation);
/// Byte offset of the layout cookie.
pub const CPU_AREA_COOKIE_OFFSET: usize = offset_of!(CpuAreaHeader, cookie);
/// Byte offset of the ABI version.
pub const CPU_AREA_ABI_VERSION_OFFSET: usize = offset_of!(CpuAreaHeader, abi_version);
/// Byte offset of the final-image register mode.
pub const CPU_AREA_REGISTER_MODE_OFFSET: usize = offset_of!(CpuAreaHeader, register_mode);
/// Byte offset of the privileged host level.
pub const CPU_AREA_HOST_LEVEL_OFFSET: usize = offset_of!(CpuAreaHeader, host_level);
/// Byte offset of the current-thread slot.
pub const CPU_AREA_CURRENT_THREAD_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, current_thread);
/// Byte offset of the kernel continuation stack.
pub const CPU_AREA_KERNEL_STACK_POINTER_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, kernel_stack_pointer);
/// Byte offset of the user trap frame.
pub const CPU_AREA_USER_TRAP_FRAME_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, user_trap_frame);
/// Byte offset of trap scratch word zero.
pub const CPU_AREA_ENTRY_SCRATCH0_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, scratch0);
/// Byte offset of trap scratch word one.
pub const CPU_AREA_ENTRY_SCRATCH1_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, scratch1);
/// Byte offset of the current header's first architecture entry scratch word.
pub const CURRENT_THREAD_TRAP_SCRATCH0_OFFSET: usize =
    offset_of!(CurrentThreadHeader, trap_scratch0);
/// Byte offset of the current header's second architecture entry scratch word.
pub const CURRENT_THREAD_TRAP_SCRATCH1_OFFSET: usize =
    offset_of!(CurrentThreadHeader, trap_scratch1);
/// Byte offset of the current header's bound CPU-area base.
pub const CURRENT_THREAD_CPU_BASE_OFFSET: usize = offset_of!(CurrentThreadHeader, cpu_base);

const _: () = {
    assert!(size_of::<CpuAreaHeader>() == 64);
    assert!(align_of::<CpuAreaHeader>() == 64);
    assert!(size_of::<CpuRuntimeAnchor>() == 64);
    assert!(align_of::<CpuRuntimeAnchor>() == 64);
    assert!(size_of::<CurrentThreadHeader>() == 64);
    assert!(align_of::<CurrentThreadHeader>() == 64);
    assert!(
        CURRENT_THREAD_TRAP_SCRATCH1_OFFSET
            == CURRENT_THREAD_TRAP_SCRATCH0_OFFSET + size_of::<usize>()
    );
    assert!(size_of::<BootThreadHeader>() == 64);
    assert!(align_of::<BootThreadHeader>() == 64);
    assert!(size_of::<CpuAreaPrefixV2>() == 192);
    assert!(align_of::<CpuAreaPrefixV2>() == 64);
    assert!(CPU_AREA_HEADER_OFFSET == 0);
    assert!(CPU_AREA_RUNTIME_ANCHOR_OFFSET == 64);
    assert!(CPU_AREA_BOOT_THREAD_OFFSET == 128);
};

/// Rejected typed CPU-area initialization fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuAreaInitError {
    /// Null or misaligned runtime base.
    AreaBase,
    /// Reserved logical CPU encoding.
    CpuIndex,
    /// Zero layout generation.
    Generation,
    /// Zero layout cookie.
    Cookie,
    /// Unsupported ABI version.
    AbiVersion,
    /// Unsupported register mode.
    RegisterMode,
    /// Unsupported host privilege.
    HostLevel,
    /// Derived prefix address overflow.
    AddressOverflow,
    /// Supplied boot-thread pointer does not match the fixed prefix slot.
    BootThread,
}

impl fmt::Display for CpuAreaInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AreaBase => "CPU area base is null or misaligned",
            Self::CpuIndex => "logical CPU index is invalid",
            Self::Generation => "layout generation must be nonzero",
            Self::Cookie => "layout cookie must be nonzero",
            Self::AbiVersion => "CPU-local ABI version is unsupported",
            Self::RegisterMode => "CPU register mode is unsupported",
            Self::HostLevel => "CPU host level is unsupported",
            Self::AddressOverflow => "CPU prefix address overflowed",
            Self::BootThread => "boot-thread pointer differs from the fixed prefix slot",
        })
    }
}

impl core::error::Error for CpuAreaInitError {}

/// Rejected frozen CPU-area header fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuAreaHeaderError {
    /// ABI version mismatch.
    AbiVersion,
    /// Logical CPU mismatch.
    CpuIndex,
    /// Runtime base mismatch.
    Anchor,
    /// Permanent boot current-thread pointer mismatch.
    BootThread,
    /// Layout generation mismatch.
    Generation,
    /// Layout cookie mismatch.
    Cookie,
    /// Register mode mismatch.
    RegisterMode,
    /// Host privilege mismatch.
    HostLevel,
}

impl fmt::Display for CpuAreaHeaderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AbiVersion => "CPU-local ABI version mismatch",
            Self::CpuIndex => "logical CPU mismatch",
            Self::Anchor => "runtime CPU-area base mismatch",
            Self::BootThread => "boot current-thread pointer mismatch",
            Self::Generation => "layout generation mismatch",
            Self::Cookie => "layout cookie mismatch",
            Self::RegisterMode => "CPU register mode mismatch",
            Self::HostLevel => "CPU host level mismatch",
        })
    }
}

impl core::error::Error for CpuAreaHeaderError {}

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

#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu.000.header")]
pub static mut __AX_CPU_AREA_PREFIX: CpuAreaPrefixV2 = CpuAreaPrefixV2::template();

#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu_end")]
pub static __AX_CPU_AREA_TEMPLATE_END: u8 = 0;

#[cfg(test)]
mod tests {
    use super::*;

    fn init(cpu: usize, area_base: usize) -> CpuAreaInitV2 {
        CpuAreaInitV2::new(
            image_register_mode(),
            HostLevelV1::Supervisor,
            CpuIndex::try_from(cpu).unwrap(),
            11,
            area_base,
            area_base + CPU_AREA_BOOT_THREAD_OFFSET,
            0x55aa,
        )
    }

    #[test]
    fn prefix_v2_uses_three_cache_lines() {
        assert_eq!(size_of::<CpuAreaPrefixV2>(), 192);
        assert_eq!(CPU_AREA_RUNTIME_ANCHOR_OFFSET, 64);
        assert_eq!(CPU_AREA_BOOT_THREAD_OFFSET, 128);
        assert_eq!(CPU_AREA_CURRENT_THREAD_OFFSET, 64);
        assert_eq!(
            CPU_AREA_KERNEL_STACK_POINTER_OFFSET,
            64 + size_of::<usize>()
        );
    }

    #[test]
    fn final_prefix_publishes_the_permanent_boot_header() {
        let prefix = CpuAreaPrefixV2::initialize(init(3, 0x8000)).unwrap();
        assert_eq!(prefix.runtime_anchor().current_thread_raw(), 0x8080);
        let binding = prefix.boot_thread().header().cpu_binding().unwrap();
        assert_eq!(binding.area_base(), 0x8000);
        assert_eq!(binding.cpu_index().as_u32(), 3);
    }

    #[test]
    fn pinned_identity_and_four_phase_cpu_binding_round_trip() {
        let context = ContextIdentity::from_raw(7).unwrap();
        let thread = ThreadIdentity::from_parts(4, 2).unwrap();
        let header = Box::pin(CurrentThreadHeader::new(context));
        header.as_ref().bind_thread(thread).unwrap();
        let binding = init(1, 0x8000).binding();
        // SAFETY: this fixture is the only scheduler owner.
        let epoch = unsafe { header.as_ref().bind_cpu(binding) }.unwrap();
        assert_eq!(header.cpu_binding().unwrap().epoch(), epoch);
        // SAFETY: no CPU current slot publishes this fixture.
        unsafe { header.as_ref().unbind_cpu(epoch) }.unwrap();
        assert!(header.cpu_binding().is_none());
        // SAFETY: rebind occurs only after the previous unbind Release.
        let next = unsafe { header.as_ref().bind_cpu(binding) }.unwrap();
        assert_ne!(epoch, next);
    }

    #[test]
    fn failed_second_thread_bind_does_not_modify_identity() {
        let first = ThreadIdentity::from_parts(4, 2).unwrap();
        let second = ThreadIdentity::from_parts(5, 9).unwrap();
        let header = Box::pin(CurrentThreadHeader::new(
            ContextIdentity::from_raw(1).unwrap(),
        ));
        header.as_ref().bind_thread(first).unwrap();
        assert_eq!(
            header.as_ref().bind_thread(second),
            Err(CurrentThreadError::ThreadAlreadyBound)
        );
        assert_eq!(header.thread_identity(), Some(first));
    }
}
