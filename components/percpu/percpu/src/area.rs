use core::mem::size_of;

pub use ax_cpu_local::{
    CPU_AREA_BOOT_THREAD_OFFSET, CPU_LOCAL_ABI_VERSION, CpuAreaHeader, CpuAreaInitV2,
    CpuAreaPrefix, CpuAreaPrefixV2, CpuBindingResultV1, CpuBindingV1, CpuIndex, CpuLocalStatus,
    CpuPin, CpuRuntimeAnchor, HostLevelV1, RegisterModeV1,
};

/// Currently supported flag bits in [`PerCpuLayoutV1`].
pub const PERCPU_LAYOUT_V1_SUPPORTED_FLAGS: u32 = 0;
const CPU_AREA_GENERATION: u32 = 1;

static INSTALLED_LAYOUT: spin::Once<InstalledLayout> = spin::Once::new();

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
    #[cfg(all(
        not(feature = "sp-naive"),
        any(not(feature = "custom-base"), feature = "host-test")
    ))]
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
    #[cfg(feature = "sp-naive")]
    {
        Ok(BoundCpuPin {
            _migration_pin: pin,
            area: PerCpuArea {
                cpu_index: CpuIndex::from_u32(0).expect("CPU zero must be representable"),
                runtime_base: 0,
                area_size: 0,
                abi_version: CPU_LOCAL_ABI_VERSION,
                register_mode: ax_cpu_local::image_register_mode().as_u8(),
                host_level: HostLevelV1::Supervisor.as_u8(),
                generation: CPU_AREA_GENERATION,
                cookie: ax_cpu_local::CPU_AREA_DEFAULT_COOKIE,
            },
        })
    }

    #[cfg(not(feature = "sp-naive"))]
    {
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
}

/// Returns the logical index owned by the verified current CPU area.
///
/// The immutable [`CpuAreaHeader`] is the single source of current-CPU
/// identity. Callers must keep `pin` alive across every operation whose
/// correctness depends on this result.
pub const fn current_cpu_index(pin: &BoundCpuPin<'_>) -> Result<CpuIndex, PerCpuError> {
    Ok(pin.area.cpu_index)
}

#[cfg(not(feature = "sp-naive"))]
fn current_platform_binding() -> Result<CpuBindingV1, PerCpuError> {
    match ax_cpu_local::platform::current_cpu_binding() {
        Ok(binding) => Ok(binding),
        Err(CpuLocalStatus::NotInitialized) => Err(PerCpuError::CurrentAreaUnbound),
        Err(status) => Err(PerCpuError::PlatformBindingStatus(status)),
    }
}

/// Descriptor for one CPU-local area in the installed layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuArea {
    cpu_index: CpuIndex,
    runtime_base: usize,
    area_size: usize,
    abi_version: u16,
    register_mode: u8,
    host_level: u8,
    generation: u32,
    cookie: usize,
}

impl PerCpuArea {
    /// Returns this area's logical CPU index.
    pub const fn cpu_index(self) -> CpuIndex {
        self.cpu_index
    }

    /// Returns the runtime address of this area's prefix.
    pub const fn runtime_base(self) -> usize {
        self.runtime_base
    }

    /// Returns the initialized bytes in this area.
    pub const fn area_size(self) -> usize {
        self.area_size
    }

    /// Returns the complete value-only binding for the platform binder.
    pub fn binding(self) -> CpuBindingV1 {
        self.init_facts().binding()
    }

    /// Returns the typed shutdown-lifetime prefix for this initialized area.
    pub fn prefix(self) -> &'static CpuAreaPrefixV2 {
        assert_ne!(
            self.runtime_base, 0,
            "the sp-naive compatibility area has no CPU-area prefix"
        );
        // SAFETY: PerCpuArea is obtainable only from the frozen initialized
        // layout, whose unsafe installation contract keeps every area mapped
        // for the shutdown lifetime.
        unsafe { &*self.prefix_ptr() }
    }

    /// Returns this remote area's shutdown-lifetime runtime anchor.
    pub fn runtime_anchor(self) -> &'static CpuRuntimeAnchor {
        self.prefix().runtime_anchor()
    }

    pub(crate) fn init_facts(self) -> CpuAreaInitV2 {
        let boot_thread = self
            .runtime_base
            .checked_add(CPU_AREA_BOOT_THREAD_OFFSET)
            .expect("installed CPU-area boot-thread address must not overflow");
        CpuAreaInitV2::from_binding(CpuBindingV1 {
            abi_version: self.abi_version,
            register_mode: self.register_mode,
            host_level: self.host_level,
            cpu_index: self.cpu_index.as_u32(),
            generation: self.generation,
            area_base: self.runtime_base,
            boot_thread,
            cookie: self.cookie,
        })
        .expect("installed layout must retain validated CPU-area initialization facts")
    }

    #[cfg(not(feature = "sp-naive"))]
    pub(crate) fn runtime_ptr(self) -> *mut u8 {
        self.runtime_base as *mut u8
    }

    pub(crate) fn prefix_ptr(self) -> *mut CpuAreaPrefixV2 {
        self.runtime_base as *mut CpuAreaPrefixV2
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstalledLayout {
    public: PerCpuLayoutV1,
    template_base: usize,
    area_size: usize,
    required_alignment: usize,
    abi_version: u16,
    register_mode: u8,
    host_level: u8,
    generation: u32,
    cookie: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LayoutIdentity {
    pub(crate) abi_version: u16,
    pub(crate) register_mode: u8,
    pub(crate) host_level: u8,
    pub(crate) generation: u32,
    pub(crate) cookie: usize,
}

impl LayoutIdentity {
    pub(crate) fn for_supervisor_image(layout: PerCpuLayoutV1) -> Self {
        let template_base = crate::percpu_template_base();
        let area_size = crate::percpu_area_size();
        Self {
            abi_version: CPU_LOCAL_ABI_VERSION,
            register_mode: ax_cpu_local::image_register_mode().as_u8(),
            host_level: HostLevelV1::Supervisor.as_u8(),
            generation: CPU_AREA_GENERATION,
            cookie: layout_cookie(layout, template_base, area_size),
        }
    }
}

impl InstalledLayout {
    pub(crate) fn from_public(
        public: PerCpuLayoutV1,
        identity: LayoutIdentity,
    ) -> Result<Self, PerCpuError> {
        if public.area_count == 0 {
            return Err(PerCpuError::EmptyLayout);
        }
        if public.flags & !PERCPU_LAYOUT_V1_SUPPORTED_FLAGS != 0 {
            return Err(PerCpuError::UnsupportedFlags(public.flags));
        }
        if identity.abi_version != CPU_LOCAL_ABI_VERSION
            || identity.generation == 0
            || identity.cookie == 0
            || RegisterModeV1::try_from_raw(identity.register_mode).is_none()
            || HostLevelV1::try_from_raw(identity.host_level).is_none()
        {
            return Err(PerCpuError::InvalidLayoutIdentity {
                abi_version: identity.abi_version,
                generation: identity.generation,
                cookie: identity.cookie,
                register_mode: identity.register_mode,
                host_level: identity.host_level,
            });
        }
        let area_size = crate::percpu_area_size();
        let required_alignment = crate::required_area_alignment()?;
        if area_size < size_of::<CpuAreaPrefix>() {
            return Err(PerCpuError::AreaTooSmall {
                actual: area_size,
                minimum: size_of::<CpuAreaPrefix>(),
            });
        }
        Self::validate_area_base(public.runtime_base, required_alignment)?;
        if public.area_stride < area_size {
            return Err(PerCpuError::StrideTooSmall {
                stride: public.area_stride,
                area_size,
            });
        }
        if !public.area_stride.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedStride {
                stride: public.area_stride,
                alignment: required_alignment,
            });
        }
        let template_base = crate::percpu_template_base();
        if !template_base.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedTemplateBase {
                base: template_base,
                alignment: required_alignment,
            });
        }
        let header_address = ax_cpu_local::cpu_area_template_base();
        if template_base != header_address {
            return Err(PerCpuError::HeaderPlacement {
                template_base,
                header_address,
            });
        }
        let last_index = public.area_count as usize - 1;
        CpuIndex::from_u32(public.area_count - 1)
            .ok_or(PerCpuError::InvalidAreaCount(public.area_count))?;
        let last_offset = public
            .area_stride
            .checked_mul(last_index)
            .ok_or(PerCpuError::AddressOverflow)?;
        public
            .runtime_base
            .checked_add(last_offset)
            .and_then(|base| base.checked_add(area_size))
            .ok_or(PerCpuError::AddressOverflow)?;
        Ok(Self {
            public,
            template_base,
            area_size,
            required_alignment,
            abi_version: identity.abi_version,
            register_mode: identity.register_mode,
            host_level: identity.host_level,
            generation: identity.generation,
            cookie: identity.cookie,
        })
    }

    pub(crate) fn area(self, cpu_index: CpuIndex) -> Result<PerCpuArea, PerCpuError> {
        if cpu_index.as_u32() >= self.public.area_count {
            return Err(PerCpuError::CpuOutOfRange {
                cpu_index,
                area_count: self.public.area_count,
            });
        }
        let offset = self
            .public
            .area_stride
            .checked_mul(cpu_index.as_usize())
            .ok_or(PerCpuError::AddressOverflow)?;
        let runtime_base = self
            .public
            .runtime_base
            .checked_add(offset)
            .ok_or(PerCpuError::AddressOverflow)?;
        Ok(PerCpuArea {
            cpu_index,
            runtime_base,
            area_size: self.area_size,
            abi_version: self.abi_version,
            register_mode: self.register_mode,
            host_level: self.host_level,
            generation: self.generation,
            cookie: self.cookie,
        })
    }

    #[cfg(not(feature = "sp-naive"))]
    pub(crate) const fn area_size(self) -> usize {
        self.area_size
    }

    #[cfg(not(feature = "sp-naive"))]
    pub(crate) const fn template_base(self) -> usize {
        self.template_base
    }

    #[cfg(not(feature = "sp-naive"))]
    pub(crate) const fn required_alignment(self) -> usize {
        self.required_alignment
    }

    #[cfg(not(feature = "sp-naive"))]
    pub(crate) const fn public(self) -> PerCpuLayoutV1 {
        self.public
    }

    #[cfg(not(feature = "sp-naive"))]
    fn area_from_binding(self, binding: CpuBindingV1) -> Result<PerCpuArea, PerCpuError> {
        let cpu_index = binding
            .cpu_index()
            .ok_or(PerCpuError::PlatformBindingStatus(
                CpuLocalStatus::InvalidBinding,
            ))?;
        let area = self.area(cpu_index)?;
        let expected = area.init_facts().binding();
        if binding != expected {
            return Err(PerCpuError::CurrentBindingMismatch {
                expected,
                actual: binding,
            });
        }
        Ok(area)
    }

    fn validate_area_base(
        runtime_base: usize,
        required_alignment: usize,
    ) -> Result<(), PerCpuError> {
        if runtime_base == 0 {
            return Err(PerCpuError::NullRuntimeBase);
        }
        if !runtime_base.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedRuntimeBase {
                base: runtime_base,
                alignment: required_alignment,
            });
        }
        Ok(())
    }
}

fn installed_layout() -> Result<InstalledLayout, PerCpuError> {
    INSTALLED_LAYOUT
        .get()
        .copied()
        .ok_or(PerCpuError::LayoutNotInstalled)
}

#[cfg(not(feature = "sp-naive"))]
pub(crate) fn freeze_initialized_layout(layout: InstalledLayout) {
    assert!(
        INSTALLED_LAYOUT.get().is_none(),
        "CPU-local layout publication must occur exactly once after typed initialization"
    );
    INSTALLED_LAYOUT.call_once(|| layout);
}

fn layout_cookie(layout: PerCpuLayoutV1, template_base: usize, area_size: usize) -> usize {
    let mixed = layout.runtime_base.rotate_left(7)
        ^ layout.area_stride.rotate_left(17)
        ^ (layout.area_count as usize).rotate_left(29)
        ^ template_base.rotate_left(11)
        ^ area_size
        ^ ax_cpu_local::CPU_AREA_DEFAULT_COOKIE;
    if mixed == 0 {
        ax_cpu_local::CPU_AREA_DEFAULT_COOKIE
    } else {
        mixed
    }
}

/// Failure to initialize, locate, or verify a CPU-local area.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PerCpuError {
    /// No CPU areas were supplied.
    #[error("CPU-local layout contains no areas")]
    EmptyLayout,
    /// The area count includes the reserved invalid CPU-index encoding.
    #[error("CPU-local area count {0} cannot be represented by CpuIndex")]
    InvalidAreaCount(u32),
    /// Reserved layout flags were supplied.
    #[error("CPU-local layout has unsupported flags {0:#x}")]
    UnsupportedFlags(u32),
    /// One area cannot hold the fixed prefix.
    #[error("CPU-local template size {actual:#x} is smaller than {minimum:#x}")]
    AreaTooSmall {
        /// Actual linked template size.
        actual: usize,
        /// Minimum supported prefix size.
        minimum: usize,
    },
    /// Adjacent runtime areas overlap.
    #[error("CPU-local stride {stride:#x} is smaller than area size {area_size:#x}")]
    StrideTooSmall {
        /// Supplied stride.
        stride: usize,
        /// Linked template size.
        area_size: usize,
    },
    /// Runtime base is null.
    #[error("CPU-local runtime base is null")]
    NullRuntimeBase,
    /// Runtime base is not prefix-aligned.
    #[error("CPU-local runtime base {base:#x} is not aligned to {alignment:#x}")]
    MisalignedRuntimeBase {
        /// Supplied runtime base.
        base: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// Area stride does not preserve prefix alignment.
    #[error("CPU-local stride {stride:#x} is not aligned to {alignment:#x}")]
    MisalignedStride {
        /// Supplied stride.
        stride: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// The linked template base does not preserve every symbol's alignment.
    #[error("CPU-local template base {base:#x} is not aligned to {alignment:#x}")]
    MisalignedTemplateBase {
        /// Linked template base.
        base: usize,
        /// Required alignment.
        alignment: usize,
    },
    /// Linker-provided alignment descriptor boundaries are inconsistent.
    #[error("CPU-local alignment metadata range {start:#x}..{end:#x} is malformed")]
    MalformedAlignmentMetadata {
        /// First descriptor address.
        start: usize,
        /// One-past-the-end descriptor address.
        end: usize,
    },
    /// A generated symbol alignment is not a nonzero power of two.
    #[error("CPU-local symbol alignment descriptor {0:#x} is invalid")]
    InvalidSymbolAlignment(usize),
    /// The linker layout and generated descriptor table disagree.
    #[error(
        "CPU-local alignment metadata requires {descriptors:#x}, but linker reports {linker:#x}"
    )]
    AlignmentMetadataMismatch {
        /// Maximum alignment from generated Rust descriptors.
        descriptors: usize,
        /// Alignment used by the linker for the template.
        linker: usize,
    },
    /// Address calculation overflowed.
    #[error("CPU-local layout address calculation overflowed")]
    AddressOverflow,
    /// Frozen ABI, ownership mode, host level, generation, or cookie is invalid.
    #[error(
        "CPU-local layout identity has ABI {abi_version}, mode {register_mode}, host level \
         {host_level}, generation {generation}, and cookie {cookie:#x}"
    )]
    InvalidLayoutIdentity {
        /// Requested CPU-local ABI version.
        abi_version: u16,
        /// Requested nonzero generation.
        generation: u32,
        /// Requested nonzero identity cookie.
        cookie: usize,
        /// Stable CPU register mode byte.
        register_mode: u8,
        /// Stable host privilege byte.
        host_level: u8,
    },
    /// Initializer table boundaries are inconsistent.
    #[error("CPU-local initializer table range {start:#x}..{end:#x} is malformed")]
    MalformedInitTable {
        /// First registration address.
        start: usize,
        /// One-past-the-end registration address.
        end: usize,
    },
    /// One typed initializer does not fit the validated template layout.
    #[error(
        "CPU-local initializer {index} has invalid offset {offset:#x}, size {size:#x}, or \
         alignment {alignment:#x}"
    )]
    MalformedInitRecord {
        /// Registration index in the final image.
        index: usize,
        /// Destination offset from the CPU-area prefix.
        offset: usize,
        /// Size of the typed storage object.
        size: usize,
        /// Alignment of the typed storage object.
        alignment: usize,
    },
    /// Two typed initializer destinations overlap.
    #[error(
        "CPU-local initializer destinations overlap at offsets {first_offset:#x} and \
         {second_offset:#x}"
    )]
    OverlappingInitRecords {
        /// First overlapping destination offset.
        first_offset: usize,
        /// Second overlapping destination offset.
        second_offset: usize,
    },
    /// Another CPU-local initialization attempt is still active.
    #[error("CPU-local layout initialization is already in progress")]
    LayoutInitializationInProgress,
    /// The CPU-local layout has already been initialized and frozen.
    #[error("CPU-local layout has already been initialized")]
    LayoutAlreadyInitialized,
    /// This target does not provide the ELF typed-initializer table contract.
    #[error("CPU-local typed initializer table is unavailable on this target")]
    InitializerTableUnavailable,
    /// The fixed header is not first in the linked per-CPU template.
    #[error(
        "CPU-local template base {template_base:#x} differs from header address \
         {header_address:#x}"
    )]
    HeaderPlacement {
        /// Loaded template start.
        template_base: usize,
        /// Fixed prefix address in the loaded image.
        header_address: usize,
    },
    /// No runtime layout has been installed.
    #[error("CPU-local runtime layout is not installed")]
    LayoutNotInstalled,
    /// Requested logical CPU is not present in the installed layout.
    #[error("CPU {cpu_index:?} is outside layout area count {area_count}")]
    CpuOutOfRange {
        /// Requested CPU.
        cpu_index: CpuIndex,
        /// Number of installed areas.
        area_count: u32,
    },
    /// The platform reports no published current area.
    #[error("current CPU-local area is not bound to a logical CPU")]
    CurrentAreaUnbound,
    /// The platform capability could not return a usable current binding.
    #[error("CPU-local platform binding query returned {0:?}")]
    PlatformBindingStatus(CpuLocalStatus),
    /// The platform binding differs from the installed layout identity.
    #[error("current CPU-local binding {actual:?} differs from expected {expected:?}")]
    CurrentBindingMismatch {
        /// Binding derived from the installed layout and logical CPU.
        expected: CpuBindingV1,
        /// Binding returned by the platform capability.
        actual: CpuBindingV1,
    },
    /// Fixed-header identity differs from the expected area.
    #[error("CPU-local header verification failed: {0}")]
    Header(ax_cpu_local::CpuAreaHeaderError),
    /// Complete fixed-prefix construction facts are invalid.
    #[error("CPU-local prefix initialization failed: {0}")]
    Prefix(ax_cpu_local::CpuAreaInitError),
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnitCpuLocalPlatform;

    #[ax_cpu_local::impl_extern_trait(name = "ax-cpu-local_0_1", abi = "rust")]
    impl ax_cpu_local::CpuLocalPlatformV1 for UnitCpuLocalPlatform {
        fn current_cpu_binding() -> CpuBindingResultV1 {
            CpuBindingResultV1::error(CpuLocalStatus::NotInitialized)
        }

        fn get_tp() -> usize {
            0
        }

        unsafe fn set_tp(_value: usize) -> CpuLocalStatus {
            CpuLocalStatus::Unsupported
        }

        fn current_thread() -> usize {
            0
        }
    }

    #[test]
    fn public_layout_shape_is_stable() {
        assert_eq!(size_of::<PerCpuLayoutV1>(), 2 * size_of::<usize>() + 8);
        assert_eq!(core::mem::offset_of!(PerCpuLayoutV1, runtime_base), 0);
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutV1, area_stride),
            size_of::<usize>()
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutV1, area_count),
            2 * size_of::<usize>()
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutV1, flags),
            2 * size_of::<usize>() + 4
        );
    }

    #[test]
    fn initialization_shape_is_value_only_and_stable() {
        assert_eq!(size_of::<PerCpuLayoutInitV2>(), 3 * size_of::<usize>() + 16);
        assert_eq!(core::mem::offset_of!(PerCpuLayoutInitV2, runtime_base), 0);
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, area_stride),
            size_of::<usize>()
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, abi_version),
            2 * size_of::<usize>() + 8
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, register_mode),
            2 * size_of::<usize>() + 10
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, host_level),
            2 * size_of::<usize>() + 11
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, generation),
            2 * size_of::<usize>() + 12
        );
        assert_eq!(
            core::mem::offset_of!(PerCpuLayoutInitV2, cookie),
            2 * size_of::<usize>() + 16
        );
    }

    #[test]
    fn layout_cookie_is_nonzero_and_depends_on_layout() {
        let first = PerCpuLayoutV1 {
            runtime_base: 0x1000,
            area_stride: 0x1000,
            area_count: 2,
            flags: 0,
        };
        let second = PerCpuLayoutV1 {
            runtime_base: 0x2000,
            ..first
        };
        assert_ne!(layout_cookie(first, 0, 0x200), 0);
        assert_ne!(
            layout_cookie(first, 0, 0x200),
            layout_cookie(second, 0, 0x200)
        );
    }

    #[cfg(all(feature = "host-test", not(feature = "sp-naive")))]
    #[test]
    fn uninstalled_host_binding_is_a_typed_unbound_error() {
        // SAFETY: the unit-test thread never enables a scheduler or migrates.
        let pin = unsafe { CpuPin::new_unchecked() };
        assert!(matches!(
            bound_current(&pin),
            Err(PerCpuError::CurrentAreaUnbound)
        ));
    }
}
