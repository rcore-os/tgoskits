pub use cpu_local::{
    CPU_AREA_BOOT_THREAD_OFFSET, CPU_LOCAL_ABI_VERSION, CpuAreaHeader, CpuAreaInitV2,
    CpuAreaPrefix, CpuAreaPrefixV2, CpuBindingResultV1, CpuBindingV1, CpuIndex, CpuLocalStatus,
    CpuPin, CpuRuntimeAnchor, HostLevelV1, RegisterModeV1,
};

mod layout;

pub(crate) use layout::{
    InstalledLayout, LayoutIdentity, freeze_initialized_layout, installed_layout,
};
pub use layout::{PerCpuArea, PerCpuError};

/// Currently supported flag bits in [`PerCpuLayoutV1`].
pub const PERCPU_LAYOUT_V1_SUPPORTED_FLAGS: u32 = 0;
const CPU_AREA_GENERATION: u32 = 1;

/// Versioned value-only description of contiguous runtime CPU-local areas.
///
/// Loaded template address, initialized template size, generation, and identity
/// cookie are crate-owned facts and deliberately do not cross this FFI shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PerCpuLayoutV1 {
    /// Runtime address of CPU zero's fixed [`CpuAreaPrefix`].
    pub runtime_base: usize,
    /// Runtime byte stride between adjacent CPU areas.
    pub area_stride: usize,
    /// Number of addressable CPU areas.
    pub area_count: u32,
    /// Reserved ABI flags. Unknown bits are rejected.
    pub flags: u32,
}

impl PerCpuLayoutV1 {
    /// Validates this value against the linked per-CPU template.
    pub fn validate(self) -> Result<(), PerCpuError> {
        InstalledLayout::from_public(self, LayoutIdentity::for_supervisor_image(self)).map(|_| ())
    }
}

/// Complete value-only facts for one final CPU-area initialization.
///
/// The platform owns the runtime storage geometry and its shutdown-lifetime
/// identity. `ax-percpu` validates these scalars against the loaded template
/// before constructing any Rust value in any area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PerCpuLayoutInitV2 {
    /// Runtime address of CPU zero's fixed [`CpuAreaPrefixV2`].
    pub runtime_base: usize,
    /// Runtime byte stride between adjacent CPU areas.
    pub area_stride: usize,
    /// Number of addressable CPU areas.
    pub area_count: u32,
    /// Reserved layout flags. Unknown bits are rejected.
    pub flags: u32,
    /// CPU-local prefix ABI version.
    pub abi_version: u16,
    /// [`RegisterModeV1`] encoded as a stable byte.
    pub register_mode: u8,
    /// [`HostLevelV1`] encoded as a stable byte.
    pub host_level: u8,
    /// Nonzero generation frozen into every area header.
    pub generation: u32,
    /// Nonzero identity cookie frozen into every area header.
    pub cookie: usize,
}

impl PerCpuLayoutInitV2 {
    /// Creates initialization facts with a typed register-ownership mode.
    pub const fn new(
        layout: PerCpuLayoutV1,
        generation: u32,
        cookie: usize,
        register_mode: RegisterModeV1,
        host_level: HostLevelV1,
    ) -> Self {
        Self {
            runtime_base: layout.runtime_base,
            area_stride: layout.area_stride,
            area_count: layout.area_count,
            flags: layout.flags,
            abi_version: CPU_LOCAL_ABI_VERSION,
            register_mode: register_mode.as_u8(),
            host_level: host_level.as_u8(),
            generation,
            cookie,
        }
    }

    /// Creates facts for linked host fixtures and supervisor-only images.
    ///
    /// Platform boot paths must use [`Self::new`] with their live host level.
    #[cfg(feature = "host-test")]
    pub(crate) fn for_supervisor_image(layout: PerCpuLayoutV1) -> Self {
        let identity = LayoutIdentity::for_supervisor_image(layout);
        Self {
            runtime_base: layout.runtime_base,
            area_stride: layout.area_stride,
            area_count: layout.area_count,
            flags: layout.flags,
            abi_version: identity.abi_version,
            register_mode: identity.register_mode,
            host_level: identity.host_level,
            generation: identity.generation,
            cookie: identity.cookie,
        }
    }

    /// Returns the v1 storage geometry carried by these v2 facts.
    pub const fn layout(self) -> PerCpuLayoutV1 {
        PerCpuLayoutV1 {
            runtime_base: self.runtime_base,
            area_stride: self.area_stride,
            area_count: self.area_count,
            flags: self.flags,
        }
    }

    /// Validates these facts against the loaded template without writing.
    pub fn validate(self) -> Result<(), PerCpuError> {
        InstalledLayout::from_public(self.layout(), self.identity()).map(|_| ())
    }

    pub(crate) const fn identity(self) -> LayoutIdentity {
        LayoutIdentity {
            abi_version: self.abi_version,
            register_mode: self.register_mode,
            host_level: self.host_level,
            generation: self.generation,
            cookie: self.cookie,
        }
    }
}

/// Returns the descriptor for one remote or current CPU in O(1).
pub fn area(cpu_index: CpuIndex) -> Result<PerCpuArea, PerCpuError> {
    installed_layout()?.area(cpu_index)
}

/// Returns the immutable process-wide CPU-area layout.
pub fn layout() -> Result<PerCpuLayoutV1, PerCpuError> {
    Ok(installed_layout()?.public)
}

/// A migration pin strengthened by a verified current CPU-area binding.
///
/// This capability borrows the original [`CpuPin`], so it cannot outlive the
/// scheduler or IRQ guard that prevents migration. Construction first matches
/// the platform's value-only binding to the installed layout and only then
/// dereferences and validates the immutable header.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_percpu::BoundCpuPin<'static>>();
/// ```
#[derive(Clone, Copy, Debug)]
pub struct BoundCpuPin<'pin> {
    _migration_pin: &'pin CpuPin,
    area: PerCpuArea,
}

impl BoundCpuPin<'_> {
    /// Returns the exact installed area covered by this capability.
    pub const fn area(&self) -> PerCpuArea {
        self.area
    }

    /// Returns the logical CPU identity validated during construction.
    pub const fn cpu_index(&self) -> CpuIndex {
        self.area.cpu_index
    }

    /// Returns the layout generation validated during construction.
    pub const fn generation(&self) -> u32 {
        self.area.generation
    }

    /// Returns the layout cookie validated during construction.
    pub const fn cookie(&self) -> usize {
        self.area.cookie
    }

    /// Returns the fixed typed prefix covered by this migration pin.
    pub fn prefix(&self) -> &CpuAreaPrefixV2 {
        self.area.prefix()
    }

    /// Borrows the current CPU's runtime anchor under this migration pin.
    pub fn runtime_anchor(&self) -> &CpuRuntimeAnchor {
        self.prefix().runtime_anchor()
    }

    pub(crate) const fn area_base(&self) -> usize {
        self.area.runtime_base
    }
}

/// Verifies and borrows the live CPU-area binding covered by `pin`.
pub fn bound_current(pin: &CpuPin) -> Result<BoundCpuPin<'_>, PerCpuError> {
    let binding = current_platform_binding()?;
    let current_area = installed_layout()?.area_from_binding(binding)?;
    current_area
        .prefix()
        .validate_init(current_area.init_facts())
        .map_err(PerCpuError::Header)?;
    Ok(BoundCpuPin {
        _migration_pin: pin,
        area: current_area,
    })
}

/// Returns the logical index owned by the verified current CPU area.
///
/// The immutable [`CpuAreaHeader`] is the single source of current-CPU
/// identity. Callers must keep `pin` alive across every operation whose
/// correctness depends on this result.
pub const fn current_cpu_index(pin: &BoundCpuPin<'_>) -> Result<CpuIndex, PerCpuError> {
    Ok(pin.area.cpu_index)
}

fn current_platform_binding() -> Result<CpuBindingV1, PerCpuError> {
    match cpu_local::platform::current_cpu_binding() {
        Ok(binding) => Ok(binding),
        Err(CpuLocalStatus::NotInitialized) => Err(PerCpuError::CurrentAreaUnbound),
        Err(status) => Err(PerCpuError::PlatformBindingStatus(status)),
    }
}
