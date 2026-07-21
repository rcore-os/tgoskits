use core::{
    fmt,
    mem::{align_of, offset_of, size_of},
    sync::atomic::{AtomicUsize, Ordering},
};

use super::{
    CURRENT_THREAD_TRAP_SCRATCH0_OFFSET, CURRENT_THREAD_TRAP_SCRATCH1_OFFSET, CurrentThreadHeader,
    cpu_header_reserved_size,
};
use crate::{
    CPU_LOCAL_ABI_VERSION, CpuBindingV1, CpuIndex, HostLevelV1, RegisterModeV1, image_register_mode,
};

/// Default nonzero identity cookie for compatibility CPU-area layouts.
pub const CPU_AREA_DEFAULT_COOKIE: usize = 0x4158_4350;

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
