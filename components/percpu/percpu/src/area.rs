use core::{
    marker::PhantomData,
    mem::size_of,
    sync::atomic::{Ordering, compiler_fence},
};

pub use ax_cpu_local::{
    CpuAreaHeader, CpuAreaPrefix, CpuIndex, CpuLocalAnchor, CpuPin, PerCpuRelocation,
};

/// Currently supported flag bits in [`PerCpuLayoutV1`].
pub const PERCPU_LAYOUT_V1_SUPPORTED_FLAGS: u32 = 0;
const CPU_AREA_GENERATION: u32 = 1;

static INSTALLED_LAYOUT: spin::Once<InstalledLayout> = spin::Once::new();

/// Versioned value-only description of contiguous runtime CPU-local areas.
///
/// Template link address, initialized template size, generation, and identity
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
        InstalledLayout::from_public(self).map(|_| ())
    }
}

/// Installs the process-wide runtime CPU-area layout exactly once.
///
/// Reinstalling the identical value is idempotent. A conflicting value is
/// rejected so remote CPU-area lookup cannot silently change beneath callers.
///
/// # Safety
///
/// Every byte described by the validated area count, stride, and linked
/// template size must be mapped, initialized from the complete template,
/// correctly aligned, and remain readable for the kernel lifetime. Each area
/// must remain writable until its unique [`bind_current`] publication. These
/// guarantees let [`bound_current`] reject an untrusted architecture-register
/// value by range and stride before it dereferences an immutable header.
pub unsafe fn install_layout(layout: PerCpuLayoutV1) -> Result<(), PerCpuError> {
    let candidate = InstalledLayout::from_public(layout)?;
    let installed = INSTALLED_LAYOUT.try_call_once(|| Ok::<_, PerCpuError>(candidate))?;
    if installed.public == layout {
        Ok(())
    } else {
        Err(PerCpuError::LayoutAlreadyInstalled {
            installed: installed.public,
            requested: layout,
        })
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
/// the raw architecture-register value to the installed layout and only then
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
                link_base: 0,
                area_size: 0,
                generation: CPU_AREA_GENERATION,
                cookie: ax_cpu_local::CPU_AREA_DEFAULT_COOKIE,
            },
        })
    }

    #[cfg(not(feature = "sp-naive"))]
    {
        // SAFETY: CpuPin covers the register read. The returned integer remains
        // untrusted until area_from_runtime_base proves range and stride.
        let runtime_base = unsafe { ax_cpu_local::current_area_base_raw(pin) };
        if runtime_base == 0 {
            return Err(PerCpuError::CurrentAreaUnbound);
        }
        let current_area = installed_layout()?.area_from_runtime_base(runtime_base)?;
        // SAFETY: unsafe install_layout guarantees the matched area is mapped;
        // CpuPin keeps the current architecture register stable while its
        // immutable header is checked.
        unsafe { verify_current(current_area, pin) }?;
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

#[derive(Debug)]
struct PreparedCurrentBinding {
    area: PerCpuArea,
    prefix: CpuAreaPrefix,
    pin: CpuPin,
}

/// Initializes an offline area and installs it as the current CPU's anchor.
///
/// The returned capability is `!Send` and does not restore the previous anchor
/// when dropped; CPU-local binding is CPU-lifetime state.
///
/// # Safety
///
/// The current execution context must be unable to migrate, local IRQs must be
/// disabled, `area` must belong to this physical CPU, and its full memory range
/// must be mapped, writable, exclusively owned, initialized from the complete
/// linked per-CPU template, and remain live for the CPU's lifetime. Merely
/// zeroing the area is insufficient because a macro-generated object may not
/// admit an all-zero bit pattern. Each physical CPU and each area may be bound
/// exactly once. No trap may consume the anchor until this function returns.
///
/// # Panics
///
/// Panics after publication only if the architecture register implementation
/// violates its installation contract. Recoverable validation failures are
/// returned before either the header or register is changed.
pub unsafe fn bind_current(area: PerCpuArea) -> Result<InstalledPerCpuArea, PerCpuError> {
    // SAFETY: forwarded caller guarantees mapped exclusive storage and a
    // migration/IRQ-free preparation window.
    let prepared = unsafe { prepare_current_binding(area)? };
    // SAFETY: the same caller contract remains live across the immediately
    // following irreversible publication.
    Ok(unsafe { commit_current_binding(prepared) })
}

/// Validates every recoverable binding prerequisite without publishing state.
///
/// # Safety
///
/// `area` must satisfy [`bind_current`]'s storage, lifetime, CPU ownership, and
/// execution-context requirements for the complete prepare/commit sequence.
unsafe fn prepare_current_binding(area: PerCpuArea) -> Result<PreparedCurrentBinding, PerCpuError> {
    // SAFETY: the caller guarantees a live, aligned, initialized prefix. A
    // published header is immutable, so observing it cannot race a legal
    // binder; exclusive ownership rules out two first binders.
    let header = unsafe { &*area.header_ptr() };
    if !header.is_unbound() {
        return Err(PerCpuError::AreaAlreadyBound {
            cpu_index: area.cpu_index,
        });
    }
    // SAFETY: the caller guarantees this context cannot migrate.
    let pin = unsafe { CpuPin::new_unchecked() };
    let prefix =
        CpuAreaPrefix::for_area(area.cpu_index, area.anchor(), area.generation, area.cookie);
    Ok(PreparedCurrentBinding { area, prefix, pin })
}

/// Publishes a prepared header and architecture register as one fatal commit.
///
/// # Safety
///
/// The caller must preserve the complete safety contract used to construct
/// `prepared`. Once this function starts, an architecture mismatch is an
/// unrecoverable implementation invariant rather than an ordinary error.
unsafe fn commit_current_binding(prepared: PreparedCurrentBinding) -> InstalledPerCpuArea {
    let PreparedCurrentBinding { area, prefix, pin } = prepared;
    // SAFETY: the caller owns mapped writable storage for the complete area.
    unsafe { area.prefix_ptr().write(prefix) };
    compiler_fence(Ordering::Release);
    // SAFETY: prefix publication and caller IRQ/lifetime guarantees satisfy the
    // architecture installation contract.
    unsafe { ax_cpu_local::install_current(area.anchor()) };
    // SAFETY: the same caller contract keeps the area mapped and pinned.
    if let Err(error) = unsafe { verify_current(area, &pin) } {
        fatal_current_binding_invariant(error);
    }
    InstalledPerCpuArea {
        area,
        _not_send_or_sync: PhantomData,
    }
}

#[cold]
#[inline(never)]
fn fatal_current_binding_invariant(error: PerCpuError) -> ! {
    panic!("architecture CPU-local binding commit violated its invariant: {error}")
}

/// Verifies one area against the current architecture anchor and header.
///
/// # Safety
///
/// `area` must describe mapped live storage initialized by [`bind_current`],
/// and `pin` must cover this complete verification operation.
pub unsafe fn verify_current(area: PerCpuArea, pin: &CpuPin) -> Result<(), PerCpuError> {
    compiler_fence(Ordering::Acquire);
    // Read the register as a value before touching memory. An architecture's
    // early-boot anchor may still name a boot identity record before binding.
    // SAFETY: CpuPin covers this register read; the caller separately promises
    // that `area` is mapped and live.
    let actual_base = unsafe { ax_cpu_local::current_area_base_raw(pin) };
    if actual_base != area.runtime_base {
        return Err(PerCpuError::RegisterBaseMismatch {
            expected: area.runtime_base,
            actual: actual_base,
        });
    }
    // SAFETY: forwarded caller contract guarantees a live aligned immutable
    // header. Borrow only the first cache line: trap entry owns the adjacent
    // CpuEntryScratch and may mutate it asynchronously.
    let header = unsafe { &*area.header_ptr() };
    if header.is_unbound() {
        return Err(PerCpuError::CurrentAreaUnbound);
    }
    header
        .validate(area.cpu_index, area.anchor(), area.generation, area.cookie)
        .map_err(PerCpuError::Header)
}

/// Descriptor for one CPU-local area in the installed layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuArea {
    cpu_index: CpuIndex,
    runtime_base: usize,
    link_base: usize,
    area_size: usize,
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

    /// Returns the link-to-runtime relocation for this area.
    pub fn relocation(self) -> PerCpuRelocation {
        PerCpuRelocation::from_bases(self.runtime_base(), self.link_base)
    }

    /// Returns the architecture installation value for this area.
    pub fn anchor(self) -> CpuLocalAnchor {
        CpuLocalAnchor::new(self.runtime_base(), self.relocation())
    }

    fn prefix_ptr(self) -> *mut CpuAreaPrefix {
        self.runtime_base as *mut CpuAreaPrefix
    }

    fn header_ptr(self) -> *const CpuAreaHeader {
        self.runtime_base as *const CpuAreaHeader
    }
}

/// Verified current-CPU binding capability.
///
/// ```compile_fail
/// fn require_send<T: Send>() {}
/// require_send::<ax_percpu::InstalledPerCpuArea>();
/// ```
#[derive(Debug)]
pub struct InstalledPerCpuArea {
    area: PerCpuArea,
    _not_send_or_sync: PhantomData<*mut ()>,
}

impl InstalledPerCpuArea {
    /// Returns the installed area descriptor.
    pub const fn area(&self) -> PerCpuArea {
        self.area
    }

    /// Revalidates the current register and immutable header identity.
    ///
    /// A binding remains installed for the CPU lifetime, but it is not a
    /// migration guard. The caller must therefore supply a fresh pin covering
    /// this operation instead of reusing the early-boot proof.
    pub fn verify(&self, pin: &CpuPin) -> Result<(), PerCpuError> {
        // SAFETY: successful construction established live storage and the
        // caller-provided pin covers this complete verification.
        unsafe { verify_current(self.area, pin) }
    }

    /// Returns the fixed immutable area header.
    pub fn header(&self) -> &CpuAreaHeader {
        // SAFETY: successful bind established a live aligned immutable
        // header. This does not borrow the trap-owned entry scratch cache line.
        unsafe { &*self.area.header_ptr() }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InstalledLayout {
    public: PerCpuLayoutV1,
    link_base: usize,
    area_size: usize,
    generation: u32,
    cookie: usize,
}

impl InstalledLayout {
    fn from_public(public: PerCpuLayoutV1) -> Result<Self, PerCpuError> {
        if public.area_count == 0 {
            return Err(PerCpuError::EmptyLayout);
        }
        if public.flags & !PERCPU_LAYOUT_V1_SUPPORTED_FLAGS != 0 {
            return Err(PerCpuError::UnsupportedFlags(public.flags));
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
        let link_base = crate::percpu_link_base();
        if !link_base.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedTemplateBase {
                base: link_base,
                alignment: required_alignment,
            });
        }
        let header_link_address = ax_cpu_local::cpu_area_header_link_address();
        if link_base != header_link_address {
            return Err(PerCpuError::HeaderPlacement {
                template_link_base: link_base,
                header_link_address,
            });
        }
        let last_index = public.area_count as usize - 1;
        let last_offset = public
            .area_stride
            .checked_mul(last_index)
            .ok_or(PerCpuError::AddressOverflow)?;
        public
            .runtime_base
            .checked_add(last_offset)
            .and_then(|base| base.checked_add(area_size))
            .ok_or(PerCpuError::AddressOverflow)?;
        let cookie = layout_cookie(public, link_base, area_size);
        Ok(Self {
            public,
            link_base,
            area_size,
            generation: CPU_AREA_GENERATION,
            cookie,
        })
    }

    fn area(self, cpu_index: CpuIndex) -> Result<PerCpuArea, PerCpuError> {
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
            link_base: self.link_base,
            area_size: self.area_size,
            generation: self.generation,
            cookie: self.cookie,
        })
    }

    #[cfg(not(feature = "sp-naive"))]
    fn area_from_runtime_base(self, runtime_base: usize) -> Result<PerCpuArea, PerCpuError> {
        let offset = runtime_base
            .checked_sub(self.public.runtime_base)
            .ok_or(PerCpuError::CurrentAreaOutsideLayout { runtime_base })?;
        if !offset.is_multiple_of(self.public.area_stride) {
            return Err(PerCpuError::CurrentAreaOutsideLayout { runtime_base });
        }
        let index = offset / self.public.area_stride;
        if index >= self.public.area_count as usize {
            return Err(PerCpuError::CurrentAreaOutsideLayout { runtime_base });
        }
        let cpu_index = CpuIndex::try_from(index).map_err(|_| PerCpuError::AddressOverflow)?;
        self.area(cpu_index)
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

fn layout_cookie(layout: PerCpuLayoutV1, link_base: usize, area_size: usize) -> usize {
    let mixed = layout.runtime_base.rotate_left(7)
        ^ layout.area_stride.rotate_left(17)
        ^ (layout.area_count as usize).rotate_left(29)
        ^ link_base.rotate_left(11)
        ^ area_size
        ^ ax_cpu_local::CPU_AREA_DEFAULT_COOKIE;
    if mixed == 0 {
        ax_cpu_local::CPU_AREA_DEFAULT_COOKIE
    } else {
        mixed
    }
}

/// Failure to install, locate, bind, or verify a CPU-local area.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PerCpuError {
    /// No CPU areas were supplied.
    #[error("CPU-local layout contains no areas")]
    EmptyLayout,
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
    /// The fixed header is not first in the linked per-CPU template.
    #[error(
        "CPU-local template base {template_link_base:#x} differs from header address \
         {header_link_address:#x}"
    )]
    HeaderPlacement {
        /// Linked template start.
        template_link_base: usize,
        /// Fixed prefix link address.
        header_link_address: usize,
    },
    /// No runtime layout has been installed.
    #[error("CPU-local runtime layout is not installed")]
    LayoutNotInstalled,
    /// Another runtime layout was already installed.
    #[error("CPU-local layout is already installed as {installed:?}, rejected {requested:?}")]
    LayoutAlreadyInstalled {
        /// Existing immutable layout.
        installed: PerCpuLayoutV1,
        /// Conflicting requested layout.
        requested: PerCpuLayoutV1,
    },
    /// Requested logical CPU is not present in the installed layout.
    #[error("CPU {cpu_index:?} is outside layout area count {area_count}")]
    CpuOutOfRange {
        /// Requested CPU.
        cpu_index: CpuIndex,
        /// Number of installed areas.
        area_count: u32,
    },
    /// The requested area was already published for a CPU lifetime.
    #[error("CPU-local area for {cpu_index:?} is already bound")]
    AreaAlreadyBound {
        /// Logical CPU owning the immutable area header.
        cpu_index: CpuIndex,
    },
    /// The current architecture anchor names an unpublished area header.
    #[error("current CPU-local area is not bound to a logical CPU")]
    CurrentAreaUnbound,
    /// The raw architecture anchor does not name an exact installed area.
    #[error("current CPU-local anchor {runtime_base:#x} is outside the installed layout")]
    CurrentAreaOutsideLayout {
        /// Untrusted address read from the architecture register.
        runtime_base: usize,
    },
    /// Architecture register identity changed or differs from the expected area.
    #[error("current CPU-local anchor {actual:#x} differs from expected area {expected:#x}")]
    RegisterBaseMismatch {
        /// Expected runtime area base.
        expected: usize,
        /// Address observed from the architecture register.
        actual: usize,
    },
    /// Fixed-header identity differs from the expected area.
    #[error("CPU-local header verification failed: {0}")]
    Header(ax_cpu_local::CpuAreaHeaderError),
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn uninstalled_host_anchor_is_a_typed_unbound_error() {
        // SAFETY: the unit-test thread never enables a scheduler or migrates.
        let pin = unsafe { CpuPin::new_unchecked() };
        assert!(matches!(
            bound_current(&pin),
            Err(PerCpuError::CurrentAreaUnbound)
        ));
    }
}
