use core::mem::size_of;

use super::{
    CPU_AREA_BOOT_THREAD_OFFSET, CPU_AREA_GENERATION, CPU_LOCAL_ABI_VERSION, CpuAreaInitV2,
    CpuAreaPrefix, CpuAreaPrefixV2, CpuBindingV1, CpuIndex, CpuLocalStatus, CpuRuntimeAnchor,
    HostLevelV1, PERCPU_LAYOUT_V1_SUPPORTED_FLAGS, PerCpuLayoutV1, RegisterModeV1,
};
#[cfg(test)]
use super::{CpuBindingResultV1, PerCpuLayoutInitV2};

static INSTALLED_LAYOUT: spin::Once<InstalledLayout> = spin::Once::new();

/// Descriptor for one CPU-local area in the installed layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuArea {
    pub(super) cpu_index: CpuIndex,
    pub(super) runtime_base: usize,
    area_size: usize,
    abi_version: u16,
    register_mode: u8,
    host_level: u8,
    pub(super) generation: u32,
    pub(super) cookie: usize,
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
            "an installed CPU area must have a nonzero runtime base"
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

    pub(crate) fn runtime_ptr(self) -> *mut u8 {
        self.runtime_base as *mut u8
    }

    pub(crate) fn prefix_ptr(self) -> *mut CpuAreaPrefixV2 {
        self.runtime_base as *mut CpuAreaPrefixV2
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InstalledLayout {
    pub(super) public: PerCpuLayoutV1,
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
        let template_base = crate::template_base();
        let area_size = crate::template_size();
        Self {
            abi_version: CPU_LOCAL_ABI_VERSION,
            register_mode: cpu_local::image_register_mode().as_u8(),
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
        let area_size = crate::template_size();
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
        let template_base = crate::template_base();
        if !template_base.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedTemplateBase {
                base: template_base,
                alignment: required_alignment,
            });
        }
        let header_address = cpu_local::cpu_area_template_base();
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

    pub(crate) const fn area_size(self) -> usize {
        self.area_size
    }

    pub(crate) const fn template_base(self) -> usize {
        self.template_base
    }

    pub(crate) const fn required_alignment(self) -> usize {
        self.required_alignment
    }

    pub(crate) const fn public(self) -> PerCpuLayoutV1 {
        self.public
    }

    pub(super) fn area_from_binding(
        self,
        binding: CpuBindingV1,
    ) -> Result<PerCpuArea, PerCpuError> {
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

pub(crate) fn installed_layout() -> Result<InstalledLayout, PerCpuError> {
    INSTALLED_LAYOUT
        .get()
        .copied()
        .ok_or(PerCpuError::LayoutNotInstalled)
}

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
        ^ cpu_local::CPU_AREA_DEFAULT_COOKIE;
    if mixed == 0 {
        cpu_local::CPU_AREA_DEFAULT_COOKIE
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
    Header(cpu_local::CpuAreaHeaderError),
    /// Complete fixed-prefix construction facts are invalid.
    #[error("CPU-local prefix initialization failed: {0}")]
    Prefix(cpu_local::CpuAreaInitError),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "host-test")]
    use crate::{CpuPin, bound_current};

    struct UnitCpuLocalPlatform;

    #[cpu_local::impl_extern_trait(name = "cpu-local_0_1", abi = "rust")]
    impl cpu_local::CpuLocalPlatformV1 for UnitCpuLocalPlatform {
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

    #[cfg(feature = "host-test")]
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
