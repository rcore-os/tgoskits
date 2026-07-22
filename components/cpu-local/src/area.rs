use core::{
    mem::{MaybeUninit, align_of, offset_of, size_of},
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{CpuIndex, CpuLocalError, CurrentThreadHeader};

/// CPU-local scalar state shared by trap entry and scheduler publication.
#[repr(C, align(64))]
pub struct CpuRuntimeAnchor {
    current_thread: AtomicUsize,
    architecture_state: [AtomicUsize; 4],
    reserved: [u8; 64 - 5 * size_of::<usize>()],
}

impl CpuRuntimeAnchor {
    const fn for_boot_thread(boot_thread: usize) -> Self {
        Self {
            current_thread: AtomicUsize::new(boot_thread),
            architecture_state: [const { AtomicUsize::new(0) }; 4],
            reserved: [0; 64 - 5 * size_of::<usize>()],
        }
    }

    /// Acquires the current-thread pointer published by the scheduler.
    pub fn current_thread_raw(&self) -> usize {
        self.current_thread.load(Ordering::Acquire)
    }

    pub(crate) const fn current_thread_slot(&self) -> &AtomicUsize {
        &self.current_thread
    }
}

/// Permanent current header used before the scheduler publishes a task.
#[repr(transparent)]
pub struct BootThreadHeader(CurrentThreadHeader);

impl BootThreadHeader {
    const fn for_area(area_base: usize) -> Self {
        Self(CurrentThreadHeader::boot(area_base))
    }

    /// Returns the permanent pinned header.
    pub const fn header(&self) -> &CurrentThreadHeader {
        &self.0
    }
}

const fn area_header_reserved_size() -> usize {
    64 - size_of::<u32>() * 2 - size_of::<usize>()
}

/// Immutable identity stored at the beginning of each initialized CPU area.
#[repr(C, align(64))]
pub struct CpuAreaHeader {
    cpu_index: u32,
    reserved_word: u32,
    self_base: usize,
    reserved: [u8; area_header_reserved_size()],
}

impl CpuAreaHeader {
    const fn new(cpu_index: CpuIndex, self_base: usize) -> Self {
        Self {
            cpu_index: cpu_index.as_u32(),
            reserved_word: 0,
            self_base,
            reserved: [0; area_header_reserved_size()],
        }
    }

    /// Returns the logical CPU index assigned to this area.
    pub const fn cpu_index(&self) -> CpuIndex {
        match CpuIndex::from_u32(self.cpu_index) {
            Some(index) => index,
            None => panic!("initialized CPU area contains the reserved CPU index"),
        }
    }

    /// Returns the permanent runtime base recorded by this area.
    pub const fn self_base(&self) -> usize {
        self.self_base
    }
}

/// Fixed three-cache-line prefix of every initialized runtime CPU area.
#[repr(C, align(64))]
pub struct CpuAreaPrefix {
    header: CpuAreaHeader,
    runtime: CpuRuntimeAnchor,
    boot_thread: BootThreadHeader,
}

impl CpuAreaPrefix {
    /// Constructs the prefix value for one exclusively owned offline area.
    ///
    /// # Errors
    ///
    /// Returns an address error when `area_base` is null, misaligned, or its
    /// fixed boot-thread address overflows.
    pub fn initialize(cpu_index: CpuIndex, area_base: usize) -> Result<Self, CpuLocalError> {
        validate_area_base(area_base)?;
        area_base
            .checked_add(CPU_AREA_BOOT_THREAD_OFFSET)
            .ok_or(CpuLocalError::AddressOverflow)?;
        Ok(Self {
            header: CpuAreaHeader::new(cpu_index, area_base),
            runtime: CpuRuntimeAnchor::for_boot_thread(area_base + CPU_AREA_BOOT_THREAD_OFFSET),
            boot_thread: BootThreadHeader::for_area(area_base),
        })
    }

    /// Returns immutable area identity.
    pub const fn header(&self) -> &CpuAreaHeader {
        &self.header
    }

    /// Returns CPU runtime and trap state.
    pub const fn runtime_anchor(&self) -> &CpuRuntimeAnchor {
        &self.runtime
    }

    /// Returns the permanent boot current-thread header.
    pub const fn boot_thread(&self) -> &BootThreadHeader {
        &self.boot_thread
    }
}

/// Permanent typed reference to one fully initialized runtime CPU area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuAreaRef {
    prefix: NonNull<CpuAreaPrefix>,
    cpu_index: CpuIndex,
}

// SAFETY: the initialization contract keeps the immutable prefix and runtime
// anchor mapped until shutdown. Mutable CPU-owned fields provide their own
// atomic or external synchronization contracts.
unsafe impl Send for CpuAreaRef {}
// SAFETY: see the Send implementation; sharing this descriptor does not grant
// mutable access to non-atomic per-CPU values.
unsafe impl Sync for CpuAreaRef {}

impl CpuAreaRef {
    /// Reconstructs a reference from an initialized shutdown-lifetime prefix.
    ///
    /// # Safety
    ///
    /// `area_base` must point to a fully initialized [`CpuAreaPrefix`] that
    /// remains mapped until shutdown. No caller may mutate its identity fields.
    #[doc(hidden)]
    pub unsafe fn from_initialized_base(area_base: usize) -> Result<Self, CpuLocalError> {
        validate_area_base(area_base)?;
        let prefix = NonNull::new(area_base as *mut CpuAreaPrefix)
            .ok_or(CpuLocalError::InvalidAreaBase { base: area_base })?;
        // SAFETY: forwarded caller contract provides a live initialized prefix.
        let header = unsafe { prefix.as_ref() }.header();
        let cpu_index =
            CpuIndex::from_u32(header.cpu_index).ok_or(CpuLocalError::AreaIdentityMismatch)?;
        if header.self_base != area_base {
            return Err(CpuLocalError::AreaIdentityMismatch);
        }
        let expected_boot = area_base
            .checked_add(CPU_AREA_BOOT_THREAD_OFFSET)
            .ok_or(CpuLocalError::AddressOverflow)?;
        if unsafe { prefix.as_ref() }
            .runtime_anchor()
            .current_thread_raw()
            == 0
            || unsafe { prefix.as_ref() }
                .boot_thread()
                .header()
                .raw_cpu_binding()
                .map(|(boot_area, _)| boot_area)
                != Some(area_base)
            || expected_boot
                != core::ptr::addr_of!(unsafe { prefix.as_ref() }.boot_thread.0) as usize
        {
            return Err(CpuLocalError::AreaIdentityMismatch);
        }
        Ok(Self { prefix, cpu_index })
    }

    /// Returns this area's logical CPU index.
    pub const fn cpu_index(self) -> CpuIndex {
        self.cpu_index
    }

    /// Returns the exact runtime prefix address used as area identity.
    pub fn base(self) -> usize {
        self.prefix.as_ptr() as usize
    }

    /// Returns the initialized fixed prefix.
    pub fn prefix(self) -> &'static CpuAreaPrefix {
        // SAFETY: construction requires a shutdown-lifetime mapping.
        unsafe { self.prefix.as_ref() }
    }

    /// Returns this area's runtime/trap anchor.
    pub fn runtime_anchor(self) -> &'static CpuRuntimeAnchor {
        self.prefix().runtime_anchor()
    }
}

fn validate_area_base(area_base: usize) -> Result<(), CpuLocalError> {
    if area_base == 0 || !area_base.is_multiple_of(align_of::<CpuAreaPrefix>()) {
        Err(CpuLocalError::InvalidAreaBase { base: area_base })
    } else {
        Ok(())
    }
}

/// Size in bytes of the immutable area header.
pub const CPU_AREA_HEADER_SIZE: usize = size_of::<CpuAreaHeader>();
/// Byte offset of CPU runtime/trap state.
pub const CPU_AREA_RUNTIME_ANCHOR_OFFSET: usize = offset_of!(CpuAreaPrefix, runtime);
/// Byte offset of the permanent boot current-thread header.
pub const CPU_AREA_BOOT_THREAD_OFFSET: usize = offset_of!(CpuAreaPrefix, boot_thread);
/// Byte offset of the runtime self pointer.
pub const CPU_AREA_SELF_BASE_OFFSET: usize = offset_of!(CpuAreaHeader, self_base);
/// Byte offset of the logical CPU index.
pub const CPU_AREA_CPU_INDEX_OFFSET: usize = offset_of!(CpuAreaHeader, cpu_index);
/// Byte offset of the current-thread slot.
pub const CPU_AREA_CURRENT_THREAD_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, current_thread);
/// Byte offset of architecture-owned CPU trap state.
pub const CPU_AREA_ARCH_STATE_OFFSET: usize =
    CPU_AREA_RUNTIME_ANCHOR_OFFSET + offset_of!(CpuRuntimeAnchor, architecture_state);
/// Reserved bytes available to the architecture-owned CPU trap state.
pub const CPU_AREA_ARCH_STATE_SIZE: usize = 4 * size_of::<usize>();

const _: () = {
    assert!(size_of::<CpuAreaHeader>() == 64);
    assert!(align_of::<CpuAreaHeader>() == 64);
    assert!(size_of::<CpuRuntimeAnchor>() == 64);
    assert!(align_of::<CpuRuntimeAnchor>() == 64);
    assert!(size_of::<BootThreadHeader>() == 64);
    assert!(align_of::<BootThreadHeader>() == 64);
    assert!(size_of::<CpuAreaPrefix>() == 192);
    assert!(align_of::<CpuAreaPrefix>() == 64);
    assert!(CPU_AREA_RUNTIME_ANCHOR_OFFSET == 64);
    assert!(CPU_AREA_BOOT_THREAD_OFFSET == 128);
};

#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu.template.header")]
pub static mut __CPU_LOCAL_AREA_PREFIX: MaybeUninit<CpuAreaPrefix> = MaybeUninit::uninit();

#[doc(hidden)]
#[used]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".percpu.template.end")]
pub static __CPU_LOCAL_TEMPLATE_END: u8 = 0;
