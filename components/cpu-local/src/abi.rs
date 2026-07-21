//! Value-only CPU-local ABI shared by boot, platform, and scheduler layers.

use core::mem::{align_of, offset_of, size_of};

use trait_ffi::def_extern_trait;

use crate::CpuIndex;

/// Current CPU-area ABI version.
pub const CPU_LOCAL_ABI_VERSION: u16 = 2;

/// Final-image ownership of the architecture thread-pointer register.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RegisterModeV1 {
    /// Linux-like images keep current-thread identity in the architecture TP.
    LinuxCurrent = 0,
    /// No-userspace unikernels keep task-local storage in the architecture TP.
    UnikernelTls = 1,
}

impl RegisterModeV1 {
    /// Converts a stable ABI byte.
    pub const fn try_from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::LinuxCurrent),
            1 => Some(Self::UnikernelTls),
            _ => None,
        }
    }

    /// Returns the stable ABI byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Returns the stable ABI value widened for boot interfaces.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Returns the register mode selected by the final Cargo feature graph.
pub const fn image_register_mode() -> RegisterModeV1 {
    if cfg!(feature = "tls") {
        RegisterModeV1::UnikernelTls
    } else {
        RegisterModeV1::LinuxCurrent
    }
}

/// Privileged exception level hosting the kernel image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HostLevelV1 {
    /// Normal supervisor/kernel privilege (EL1, S-mode, or ring 0).
    Supervisor = 0,
    /// Hypervisor host privilege (EL2 or HS-mode).
    Hypervisor = 1,
}

impl HostLevelV1 {
    /// Converts a stable ABI byte.
    pub const fn try_from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Supervisor),
            1 => Some(Self::Hypervisor),
            _ => None,
        }
    }

    /// Returns the stable ABI byte.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Frozen value-only description of one initialized CPU area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CpuBindingV1 {
    /// [`CPU_LOCAL_ABI_VERSION`].
    pub abi_version: u16,
    /// [`RegisterModeV1`] as a stable byte.
    pub register_mode: u8,
    /// [`HostLevelV1`] as a stable byte.
    pub host_level: u8,
    /// Logical CPU index.
    pub cpu_index: u32,
    /// Frozen layout generation.
    pub generation: u32,
    /// Direct runtime base of the fixed CPU-area prefix.
    pub area_base: usize,
    /// Permanent boot current-thread header pointer.
    pub boot_thread: usize,
    /// Frozen layout identity cookie.
    pub cookie: usize,
}

impl CpuBindingV1 {
    /// Absent payload carried when [`CpuBindingResultV1::status`] is not `Ok`.
    pub const EMPTY: Self = Self {
        abi_version: 0,
        register_mode: 0,
        host_level: 0,
        cpu_index: CpuIndex::INVALID_RAW,
        generation: 0,
        area_base: 0,
        boot_thread: 0,
        cookie: 0,
    };

    /// Constructs a typed binding from validated values.
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

    /// Validates a value received from an untrusted scalar ABI boundary.
    pub const fn validated(self) -> Option<Self> {
        if self.abi_version != CPU_LOCAL_ABI_VERSION
            || CpuIndex::from_u32(self.cpu_index).is_none()
            || RegisterModeV1::try_from_raw(self.register_mode).is_none()
            || HostLevelV1::try_from_raw(self.host_level).is_none()
            || self.area_base == 0
            || self.boot_thread == 0
            || self.cookie == 0
            || self.generation == 0
        {
            return None;
        }
        Some(self)
    }

    /// Returns the typed logical CPU index.
    pub const fn cpu_index(self) -> Option<CpuIndex> {
        CpuIndex::from_u32(self.cpu_index)
    }

    /// Returns the typed register mode.
    pub const fn register_mode(self) -> Option<RegisterModeV1> {
        RegisterModeV1::try_from_raw(self.register_mode)
    }

    /// Returns the typed host level.
    pub const fn host_level(self) -> Option<HostLevelV1> {
        HostLevelV1::try_from_raw(self.host_level)
    }
}

/// Status returned by the value-only CPU-local platform ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum CpuLocalStatus {
    /// Operation completed successfully.
    Ok             = 0,
    /// CPU-local platform state is not initialized.
    NotInitialized = 1,
    /// CPU-local ABI version or final-image mode differs.
    AbiMismatch    = 2,
    /// Current CPU or thread publication is inconsistent.
    InvalidBinding = 3,
    /// Operation is unavailable in the selected register mode.
    Unsupported    = 4,
}

/// Fallible value-only result for a complete CPU binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct CpuBindingResultV1 {
    /// Operation status; inspect this before reading `binding`.
    pub status: CpuLocalStatus,
    /// Complete binding on success, [`CpuBindingV1::EMPTY`] otherwise.
    pub binding: CpuBindingV1,
}

impl CpuBindingResultV1 {
    /// Creates a successful result.
    pub const fn ok(binding: CpuBindingV1) -> Self {
        Self {
            status: CpuLocalStatus::Ok,
            binding,
        }
    }

    /// Creates an error without an apparently valid binding payload.
    pub const fn error(status: CpuLocalStatus) -> Self {
        Self {
            status,
            binding: CpuBindingV1::EMPTY,
        }
    }
}

const _: () = {
    assert!(offset_of!(CpuBindingV1, abi_version) == 0);
    assert!(offset_of!(CpuBindingV1, register_mode) == 2);
    assert!(offset_of!(CpuBindingV1, host_level) == 3);
    assert!(offset_of!(CpuBindingV1, cpu_index) == 4);
    assert!(offset_of!(CpuBindingV1, generation) == 8);
    assert!(offset_of!(CpuBindingV1, area_base) == if size_of::<usize>() == 8 { 16 } else { 12 });
    assert!(
        offset_of!(CpuBindingV1, boot_thread)
            == offset_of!(CpuBindingV1, area_base) + size_of::<usize>()
    );
    assert!(
        offset_of!(CpuBindingV1, cookie)
            == offset_of!(CpuBindingV1, boot_thread) + size_of::<usize>()
    );
    assert!(size_of::<CpuBindingV1>() == if size_of::<usize>() == 8 { 40 } else { 24 });
    assert!(align_of::<CpuBindingV1>() == align_of::<usize>());
    assert!(offset_of!(CpuBindingResultV1, status) == 0);
    assert!(offset_of!(CpuBindingResultV1, binding) == if size_of::<usize>() == 8 { 8 } else { 4 });
    assert!(size_of::<CpuBindingResultV1>() == if size_of::<usize>() == 8 { 48 } else { 28 });
    assert!(align_of::<CpuBindingResultV1>() == align_of::<usize>());
    assert!(size_of::<CpuLocalStatus>() == size_of::<u32>());
};

/// Static platform operations needed by generic CPU-local consumers.
///
/// Every value crossing this trait-ffi boundary is a `repr(C)` aggregate or an
/// integer. Pinning, pointer provenance, and typed header validation remain in
/// the safe facade above this interface.
#[def_extern_trait(mod_path = "cpu_local", abi = "rust")]
pub trait CpuLocalPlatformV1 {
    /// Returns the frozen binding of the calling CPU.
    fn current_cpu_binding() -> CpuBindingResultV1;

    /// Reads current-header identity in LinuxCurrent or kernel TLS in UnikernelTls.
    fn get_tp() -> usize;

    /// Installs current-header identity in LinuxCurrent or kernel TLS in UnikernelTls.
    ///
    /// # Safety
    ///
    /// The caller must own the CPU switch/boot boundary and guarantee that the
    /// value is valid for the context being installed.
    unsafe fn set_tp(value: usize) -> CpuLocalStatus;

    /// Returns the raw pinned [`crate::CurrentThreadHeader`] pointer.
    fn current_thread() -> usize;
}
