use core::mem::size_of;

use cpu_local::{CpuAreaPrefix, CpuIndex};

use crate::{PerCpuArea, PerCpuError, PerCpuRegion};

static INSTALLED_LAYOUT: spin::Once<PerCpuLayout> = spin::Once::new();

/// Frozen process-wide layout of initialized runtime per-CPU areas.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuLayout {
    region: PerCpuRegion,
    template_base: usize,
    area_size: usize,
    required_alignment: usize,
}

// SAFETY: the layout is immutable and its installation contract keeps the
// described storage mapped until shutdown.
unsafe impl Send for PerCpuLayout {}
// SAFETY: see Send; access still requires typed per-CPU capabilities.
unsafe impl Sync for PerCpuLayout {}

impl PerCpuLayout {
    pub(crate) fn validate(region: PerCpuRegion) -> Result<Self, PerCpuError> {
        let area_size = crate::template_size();
        let required_alignment = crate::required_area_alignment()?;
        if area_size < size_of::<CpuAreaPrefix>() {
            return Err(PerCpuError::AreaTooSmall {
                actual: area_size,
                minimum: size_of::<CpuAreaPrefix>(),
            });
        }
        let runtime_base = region.runtime_base();
        if !runtime_base.is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedRuntimeBase {
                base: runtime_base,
                alignment: required_alignment,
            });
        }
        if region.area_stride() < area_size {
            return Err(PerCpuError::StrideTooSmall {
                stride: region.area_stride(),
                area_size,
            });
        }
        if !region.area_stride().is_multiple_of(required_alignment) {
            return Err(PerCpuError::MisalignedStride {
                stride: region.area_stride(),
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
        let prefix_address = cpu_local::cpu_area_template_base();
        if template_base != prefix_address {
            return Err(PerCpuError::PrefixPlacement {
                template_base,
                prefix_address,
            });
        }
        let last_index = region.area_count().get() as usize - 1;
        let last_offset = region
            .area_stride()
            .checked_mul(last_index)
            .ok_or(PerCpuError::AddressOverflow)?;
        runtime_base
            .checked_add(last_offset)
            .and_then(|base| base.checked_add(area_size))
            .ok_or(PerCpuError::AddressOverflow)?;
        Ok(Self {
            region,
            template_base,
            area_size,
            required_alignment,
        })
    }

    /// Returns the platform-owned region geometry.
    pub const fn region(&self) -> PerCpuRegion {
        self.region
    }

    /// Returns CPU zero's runtime base.
    pub fn runtime_base(&self) -> usize {
        self.region.runtime_base()
    }

    /// Returns the byte stride between adjacent areas.
    pub const fn area_stride(&self) -> usize {
        self.region.area_stride()
    }

    /// Returns the number of initialized areas.
    pub const fn area_count(&self) -> u32 {
        self.region.area_count().get()
    }

    /// Returns the initialized byte size within each area.
    pub const fn area_size(&self) -> usize {
        self.area_size
    }

    /// Resolves one CPU area without consulting architecture registers.
    pub fn area(&self, cpu_index: CpuIndex) -> Result<PerCpuArea, PerCpuError> {
        if cpu_index.as_u32() >= self.area_count() {
            return Err(PerCpuError::CpuOutOfRange {
                cpu_index,
                area_count: self.area_count(),
            });
        }
        let offset = self
            .area_stride()
            .checked_mul(cpu_index.as_usize())
            .ok_or(PerCpuError::AddressOverflow)?;
        let runtime_base = self
            .runtime_base()
            .checked_add(offset)
            .ok_or(PerCpuError::AddressOverflow)?;
        Ok(PerCpuArea::new(cpu_index, runtime_base, self.area_size))
    }

    pub(crate) const fn template_base(&self) -> usize {
        self.template_base
    }

    pub(crate) const fn required_alignment(&self) -> usize {
        self.required_alignment
    }
}

pub(crate) fn installed_layout() -> Result<&'static PerCpuLayout, PerCpuError> {
    INSTALLED_LAYOUT
        .get()
        .ok_or(PerCpuError::LayoutNotInstalled)
}

pub(crate) fn freeze_initialized_layout(layout: PerCpuLayout) -> &'static PerCpuLayout {
    assert!(
        INSTALLED_LAYOUT.get().is_none(),
        "per-CPU layout publication must occur exactly once after initialization"
    );
    INSTALLED_LAYOUT.call_once(|| layout)
}
