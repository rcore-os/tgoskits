use core::ptr::NonNull;

use cpu_local::{CpuAreaPrefix, CpuAreaRef, CpuIndex, CpuPin, CpuRuntimeAnchor};

use crate::{PerCpuError, layout::installed_layout};

/// Descriptor for one initialized runtime per-CPU area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerCpuArea {
    cpu_index: CpuIndex,
    runtime_base: usize,
    area_size: usize,
}

impl PerCpuArea {
    pub(crate) const fn new(cpu_index: CpuIndex, runtime_base: usize, area_size: usize) -> Self {
        Self {
            cpu_index,
            runtime_base,
            area_size,
        }
    }

    /// Returns this area's logical CPU index.
    pub const fn cpu_index(self) -> CpuIndex {
        self.cpu_index
    }

    /// Returns this area's exact runtime base.
    pub const fn runtime_base(self) -> usize {
        self.runtime_base
    }

    /// Returns the initialized byte size in this area.
    pub const fn area_size(self) -> usize {
        self.area_size
    }

    /// Returns the validated CPU-local prefix identity.
    pub fn cpu_area(self) -> Result<CpuAreaRef, PerCpuError> {
        // SAFETY: PerCpuArea is only produced by the frozen layout after every
        // prefix has been initialized in shutdown-lifetime storage.
        Ok(unsafe { CpuAreaRef::from_initialized_base(self.runtime_base) }?)
    }

    /// Returns the initialized fixed prefix.
    pub fn prefix(self) -> Result<&'static CpuAreaPrefix, PerCpuError> {
        Ok(self.cpu_area()?.prefix())
    }

    /// Returns this remote area's runtime/trap anchor.
    pub fn runtime_anchor(self) -> Result<&'static CpuRuntimeAnchor, PerCpuError> {
        Ok(self.cpu_area()?.runtime_anchor())
    }

    pub(crate) fn runtime_ptr(self) -> *mut u8 {
        self.runtime_base as *mut u8
    }

    pub(crate) fn prefix_ptr(self) -> *mut CpuAreaPrefix {
        self.runtime_base as *mut CpuAreaPrefix
    }
}

/// Returns one remote or current CPU area in O(1).
pub fn area(cpu_index: CpuIndex) -> Result<PerCpuArea, PerCpuError> {
    installed_layout()?.area(cpu_index)
}

/// Returns the immutable process-wide per-CPU layout.
pub fn layout() -> Result<&'static crate::PerCpuLayout, PerCpuError> {
    installed_layout()
}

/// Verifies that `pin` selects the area installed for its logical CPU.
pub fn current_area(pin: &CpuPin<'_>) -> Result<PerCpuArea, PerCpuError> {
    let expected = area(pin.area().cpu_index())?;
    let expected_cpu_area = expected.cpu_area()?;
    if expected_cpu_area != pin.area() {
        return Err(PerCpuError::CurrentAreaMismatch {
            expected: expected_cpu_area,
            actual: pin.area(),
        });
    }
    Ok(expected)
}

/// Returns the logical CPU index carried by a validated pin.
pub const fn current_cpu_index(pin: &CpuPin<'_>) -> CpuIndex {
    pin.area().cpu_index()
}

/// Calculates a typed pointer in a remote initialized area.
pub(crate) fn symbol_ptr<T>(area: PerCpuArea, offset: usize) -> NonNull<T> {
    // The offset comes from a validated final-image descriptor, and every
    // PerCpuArea uses the same template geometry.
    unsafe { NonNull::new_unchecked((area.runtime_base + offset) as *mut T) }
}
