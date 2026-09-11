//! Owned extended-register images for a synchronous guest machine window.

pub use core::arch::x86_64::CpuidResult;
use core::{arch::x86_64::__cpuid_count, marker::PhantomData};

use super::memory::ControlRegion;
use crate::virtualization::{ControlMemory, VirtualizationError};

/// Current CPU's complete extended-state storage geometry.
///
/// XSAVE sizes follow CPUID.0DH component offsets and compacted alignment,
/// matching Linux `xstate_required_size`. The storage covers disabled components
/// too: changing XCR0 must not discard a guest's dormant register contents.
#[derive(Clone, Copy, Debug)]
pub struct XstateLayout {
    user: u64,
    supervisor: u64,
    initial_xcr0: u64,
    size: usize,
    mode: u64,
    xfd_mask: u64,
    components: [[u32; 4]; 64],
}

impl XstateLayout {
    /// Probes the current privileged CPU's state management configuration.
    ///
    /// # Safety
    /// Execute at ring 0 with a stable CPU. The host must retain OSXSAVE and
    /// use the same component layout on every CPU that runs this state image.
    pub unsafe fn current() -> Self {
        let cr4: u64;
        // SAFETY: the caller executes at ring 0 on a stable CPU.
        unsafe { core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack)) };
        if cr4 & (1 << 18) == 0 {
            return Self {
                user: 3,
                supervisor: 0,
                initial_xcr0: 0,
                size: 512,
                mode: 0,
                xfd_mask: 0,
                components: [[0; 4]; 64],
            };
        }
        let user = __cpuid_count(0xd, 0);
        let features = __cpuid_count(0xd, 1);
        let compacted = features.eax & (1 << 3) != 0;
        let user = u64::from(user.eax) | (u64::from(user.edx) << 32);
        let supervisor = if compacted {
            u64::from(features.ecx) | (u64::from(features.edx) << 32)
        } else {
            0
        };
        let mut components = [[0; 4]; 64];
        for (index, record) in components.iter_mut().enumerate() {
            if index < 2 || (user | supervisor) & (1 << index) != 0 {
                let value = __cpuid_count(0xd, index as u32);
                *record = [value.eax, value.ebx, value.ecx, value.edx];
            }
        }
        // EBX in the first two leaves depends on the currently loaded banks.
        // Keep only invariant geometry; guest sizes are computed from its masks.
        components[0][1] = 0;
        components[1][1] = 0;
        let mut xfd_mask = 0;
        if features.eax & (1 << 4) != 0 {
            for (index, component) in components.iter().enumerate().skip(2) {
                if user & (1 << index) != 0 && component[2] & 4 != 0 {
                    xfd_mask |= 1 << index;
                }
            }
        }
        // SAFETY: OSXSAVE is enabled, so XGETBV(0) is available.
        let initial_xcr0 = unsafe { core::arch::x86_64::_xgetbv(0) };
        Self {
            user,
            supervisor,
            initial_xcr0,
            size: required_size(&components, user | supervisor, compacted),
            mode: if compacted { 2 } else { 1 },
            xfd_mask,
            components,
        }
    }

    /// Minimum bytes required in each 64-byte-aligned host and guest image.
    pub const fn byte_len(&self) -> usize {
        self.size
    }
    /// Hardware-supported XCR0 components retained by the machine window.
    pub const fn user_features(&self) -> u64 {
        self.user
    }
    /// Hardware-supported IA32_XSS components retained by the machine window.
    pub const fn supervisor_features(&self) -> u64 {
        self.supervisor
    }
}

fn required_size(components: &[[u32; 4]; 64], mut mask: u64, compacted: bool) -> usize {
    let mut size = 576;
    mask &= !3;
    for (index, component) in components.iter().enumerate().skip(2) {
        if mask & (1 << index) == 0 {
            continue;
        }
        let offset = if compacted {
            if component[2] & 2 != 0 {
                (size + 63) & !63
            } else {
                size
            }
        } else {
            component[1] as usize
        };
        size = size.max(offset + component[0] as usize);
    }
    size
}

/// Scratch and operands consumed exclusively by the entry assembly.
#[repr(C)]
pub(crate) struct XstateSwitch {
    pub(crate) host: usize,
    pub(crate) guest: usize,
    pub(crate) mode: u64,
    pub(crate) full_xcr0: u64,
    pub(crate) full_xss: u64,
    pub(crate) host_xcr0: u64,
    pub(crate) guest_xcr0: u64,
    pub(crate) host_xss: u64,
    pub(crate) guest_xss: u64,
    pub(crate) has_xfd: u64,
    pub(crate) host_xfd: u64,
    pub(crate) guest_xfd: u64,
    pub(crate) host_xfd_err: u64,
    pub(crate) guest_xfd_err: u64,
}

/// Exclusive host and guest FP/XSAVE storage, allocated by the embedding host.
///
/// The CPU machine window saves and restores this data before returning to Rust.
/// No allocation, CPU-local storage, or scheduler state is required here.
pub struct GuestXstate<M: ControlMemory> {
    switch: XstateSwitch,
    layout: XstateLayout,
    _host: ControlRegion<M>,
    _guest: ControlRegion<M>,
    local: PhantomData<*mut ()>,
}

impl<M: ControlMemory> GuestXstate<M> {
    /// Initializes distinct inactive leases with architectural initial FP state.
    pub fn new(layout: XstateLayout, host: M, guest: M) -> Result<Self, VirtualizationError> {
        let mut host = ControlRegion::new(host, layout.size, 64)?;
        let mut guest = ControlRegion::new(guest, layout.size, 64)?;
        host.clear();
        guest.clear();
        // SAFETY: the checked leases cover aligned, exclusively owned images.
        // The legacy defaults also support the FXSAVE-only fallback; XSAVE's
        // zero XSTATE_BV asks XRSTOR to initialize every requested component.
        unsafe {
            for region in [&host, &guest] {
                region.pointer().cast::<u16>().as_ptr().write(0x37f);
                region
                    .pointer()
                    .as_ptr()
                    .add(24)
                    .cast::<u32>()
                    .write(0x1f80);
                if layout.mode == 2 {
                    region
                        .pointer()
                        .as_ptr()
                        .add(520)
                        .cast::<u64>()
                        .write((1 << 63) | layout.user | layout.supervisor);
                }
            }
        }
        Ok(Self {
            switch: XstateSwitch {
                host: host.pointer().as_ptr() as usize,
                guest: guest.pointer().as_ptr() as usize,
                mode: layout.mode,
                full_xcr0: layout.user,
                full_xss: layout.supervisor,
                host_xcr0: 0,
                guest_xcr0: layout.initial_xcr0,
                host_xss: 0,
                guest_xss: 0,
                has_xfd: u64::from(layout.xfd_mask != 0),
                host_xfd: 0,
                guest_xfd: 0,
                host_xfd_err: 0,
                guest_xfd_err: 0,
            },
            layout,
            _host: host,
            _guest: guest,
            local: PhantomData,
        })
    }

    /// Reads the guest's configured XCR0.
    pub const fn xcr0(&self) -> u64 {
        self.switch.guest_xcr0
    }
    /// Validates the guest's XCR0 feature dependencies before a later entry.
    pub fn set_xcr0(&mut self, value: u64) -> Result<(), VirtualizationError> {
        let avx512 = value & 0xe0;
        if self.layout.mode == 0
            || value & !self.layout.user != 0
            || value & 1 == 0
            || (value & 4 != 0 && value & 2 == 0)
            || (value & 0x18 != 0 && value & 0x18 != 0x18)
            || (avx512 != 0 && (avx512 != 0xe0 || value & 6 != 6))
            || (value & (3 << 17) != 0 && value & (3 << 17) != (3 << 17))
        {
            return Err(VirtualizationError::InvalidExtendedState);
        }
        self.switch.guest_xcr0 = value;
        Ok(())
    }
    /// Reads the guest's IA32_XSS bank.
    pub const fn xss(&self) -> u64 {
        self.switch.guest_xss
    }
    /// Selects hardware-supported supervisor state for guest XSAVES/XRSTORS.
    pub fn set_xss(&mut self, value: u64) -> Result<(), VirtualizationError> {
        if self.layout.mode != 2 || value & !self.layout.supervisor != 0 {
            return Err(VirtualizationError::InvalidExtendedState);
        }
        self.switch.guest_xss = value;
        Ok(())
    }
    /// Reads the guest's extended-feature-disable mask.
    pub const fn xfd(&self) -> u64 {
        self.switch.guest_xfd
    }
    /// Configures supported dynamically disabled state components.
    pub fn set_xfd(&mut self, value: u64) -> Result<(), VirtualizationError> {
        if self.layout.xfd_mask == 0 || value & !self.layout.xfd_mask != 0 {
            return Err(VirtualizationError::InvalidExtendedState);
        }
        self.switch.guest_xfd = value;
        Ok(())
    }
    /// Reads the guest's last extended-feature-disable fault mask.
    pub const fn xfd_error(&self) -> u64 {
        self.switch.guest_xfd_err
    }
    /// Clears the guest's extended-feature-disable fault mask.
    pub fn clear_xfd_error(&mut self) {
        self.switch.guest_xfd_err = 0;
    }

    /// Returns CPUID.0DH with sizes for this guest's configured feature banks.
    /// This does not temporarily load guest registers into the host CPU.
    pub fn cpuid(&self, subleaf: u32) -> CpuidResult {
        let [eax, ebx, ecx, edx] = self
            .layout
            .components
            .get(subleaf as usize)
            .copied()
            .unwrap_or([0; 4]);
        let mut result = CpuidResult { eax, ebx, ecx, edx };
        if subleaf == 0 && self.layout.mode != 0 {
            result.ebx = required_size(&self.layout.components, self.xcr0(), false) as u32;
        } else if subleaf == 1 && result.eax & ((1 << 1) | (1 << 3)) != 0 {
            result.ebx =
                required_size(&self.layout.components, self.xcr0() | self.xss(), true) as u32;
        }
        result
    }

    /// Rejects a target CPU whose enabled save mechanism or CPUID component
    /// geometry differs from the layout used to allocate these images.
    ///
    /// # Safety
    /// Execute at ring 0 on the CPU being bound, then retain that CPU and its
    /// OSXSAVE configuration throughout every corresponding entry operation.
    pub unsafe fn check_current_cpu(&self) -> Result<(), VirtualizationError> {
        // SAFETY: the caller retains the privileged binding CPU.
        let current = unsafe { XstateLayout::current() };
        if current.mode != self.layout.mode || current.components != self.layout.components {
            return Err(VirtualizationError::InvalidExtendedState);
        }
        Ok(())
    }

    pub(crate) fn switch(&mut self) -> *mut XstateSwitch {
        &mut self.switch
    }
}
