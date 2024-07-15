use super::{AxArchVCpu, AxArchVCpuExitReason, GuestPhysAddr, HostPhysAddr};
use axerrno::{ax_err, AxResult};
use core::cell::{RefCell, UnsafeCell};
extern crate alloc;
use alloc::format;

/// The constant part of `AxVCpu`.
struct AxVCpuInnerConst {
    /// The id of the vcpu.
    id: usize,
    #[allow(dead_code)]
    /// The id of the physical CPU who has the priority to run this vcpu. Not implemented yet.
    favor_phys_cpu: usize,
    #[allow(dead_code)]
    /// The mask of physical CPUs who can run this vcpu. Not implemented yet.
    affinity: usize,
}

/// The state of a virtual CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VCpuState {
    /// An invalid state.
    Invalid = 0,
    /// The vcpu is created but not initialized yet.
    Created = 1,
    /// The vcpu is free and can be bound to a physical CPU.
    Free = 2,
    /// The vcpu is bound to a physical CPU and ready to run.
    Ready = 3,
    /// The vcpu is running.
    Running = 4,
    /// The vcpu is blocked.
    Blocked = 5,
}

/// The mutable part of `AxVCpu`.
pub struct AxVCpuInnerMut {
    /// The state of the vcpu.
    state: VCpuState,
}

/// A virtual CPU.
///
/// This struct handles internal mutability itself, almost all the methods are `&self`.
///
/// Note that the `AxVCpu` is not thread-safe. It's caller's responsibility to ensure the safety.
pub struct AxVCpu<A: AxArchVCpu> {
    /// The constant part of the vcpu.
    inner_const: AxVCpuInnerConst,
    /// The mutable part of the vcpu.
    inner_mut: RefCell<AxVCpuInnerMut>,
    /// The architecture-specific state of the vcpu.
    ///
    /// `UnsafeCell` is used to allow interior mutability.
    ///
    /// `RefCell` or `Mutex` is not suitable here because it's not possible to drop the guard when launching a vcpu.
    arch_vcpu: UnsafeCell<A>,
}

/// Execute a block with the current vcpu set to `$self`.
macro_rules! with_current_cpu_set {
    ($self:ident, $arch:ident, $block:block) => {
        if get_current_vcpu::<$arch>().is_some() {
            panic!("Nested vcpu operation is not allowed!");
        } else {
            set_current_vcpu($self);
            let result = $block;
            clear_current_vcpu::<$arch>();
            result
        }
    };
}

impl<A: AxArchVCpu> AxVCpu<A> {
    /// Create a new [`AxVCpu`].
    pub fn new(
        id: usize,
        favor_phys_cpu: usize,
        affinity: usize,
        arch_config: A::CreateConfig,
    ) -> AxResult<Self> {
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                id,
                favor_phys_cpu,
                affinity,
            },
            inner_mut: RefCell::new(AxVCpuInnerMut {
                state: VCpuState::Created,
            }),
            arch_vcpu: UnsafeCell::new(A::new(arch_config)?),
        })
    }

    /// Setup the vcpu.
    pub fn setup(
        &self,
        entry: GuestPhysAddr,
        ept_root: HostPhysAddr,
        arch_config: A::SetupConfig,
    ) -> AxResult {
        self.transition_state(VCpuState::Created, VCpuState::Free)?;

        with_current_cpu_set!(self, A, {
            let arch_vcpu = self.get_arch_vcpu();
            arch_vcpu.set_entry(entry)?;
            arch_vcpu.set_ept_root(ept_root)?;
            arch_vcpu.setup(arch_config)?;
        });

        Ok(())
    }

    /// Get the id of the vcpu.
    pub fn id(&self) -> usize {
        self.inner_const.id
    }

    /// Get whether the vcpu is the BSP. We always assume the first vcpu is the BSP.
    pub fn is_bsp(&self) -> bool {
        self.inner_const.id == 0
    }

    /// Get the state of the vcpu.
    pub fn state(&self) -> VCpuState {
        self.inner_mut.borrow().state
    }

    /// Set the state of the vcpu. This method is unsafe because it shouldn't be called unless the caller DOES know what it's doing.
    pub unsafe fn set_state(&self, state: VCpuState) {
        self.inner_mut.borrow_mut().state = state;
    }

    /// Transition the state of the vcpu. If the current state is not `from`, return an error.
    pub fn transition_state(&self, from: VCpuState, to: VCpuState) -> AxResult<()> {
        // TODO: make this method a macro or add a function parameter to ensure that whenever a error occurs, the state is set to [`VCpuState::Invalid`]
        let mut inner_mut = self.inner_mut.borrow_mut();
        if inner_mut.state != from {
            ax_err!(
                BadState,
                format!("VCpu state is not {:?}, but {:?}", from, inner_mut.state)
            )
        } else {
            inner_mut.state = to;
            Ok(())
        }
    }

    pub fn get_arch_vcpu(&self) -> &mut A {
        unsafe { &mut *self.arch_vcpu.get() }
    }

    pub fn run(&self) -> AxResult<AxArchVCpuExitReason> {
        self.transition_state(VCpuState::Ready, VCpuState::Running)?;
        let result = with_current_cpu_set!(self, A, { self.get_arch_vcpu().run()? });
        self.transition_state(VCpuState::Running, VCpuState::Ready)?;
        Ok(result)
    }

    pub fn bind(&self) -> AxResult<()> {
        self.transition_state(VCpuState::Free, VCpuState::Ready)?;
        with_current_cpu_set!(self, A, { self.get_arch_vcpu().bind() })
    }

    pub fn unbind(&self) -> AxResult<()> {
        self.transition_state(VCpuState::Ready, VCpuState::Free)?;
        with_current_cpu_set!(self, A, { self.get_arch_vcpu().unbind() })
    }
}

#[percpu::def_percpu]
static mut CURRENT_VCPU: Option<*mut u8> = None;

pub fn get_current_vcpu<'a, A: AxArchVCpu>() -> Option<&'a AxVCpu<A>> {
    unsafe {
        CURRENT_VCPU
            .current_ref_raw()
            .as_ref()
            .copied()
            .map(|p| (p as *const AxVCpu<A>).as_ref())
            .flatten()
    }
}

pub fn get_current_vcpu_mut<'a, A: AxArchVCpu>() -> Option<&'a mut AxVCpu<A>> {
    unsafe {
        CURRENT_VCPU
            .current_ref_mut_raw()
            .as_mut()
            .copied()
            .map(|p| (p as *mut AxVCpu<A>).as_mut())
            .flatten()
    }
}

pub fn set_current_vcpu<A: AxArchVCpu>(vcpu: &AxVCpu<A>) {
    unsafe {
        CURRENT_VCPU
            .current_ref_mut_raw()
            .replace(vcpu as *const _ as *mut u8);
    }
}

pub fn clear_current_vcpu<A: AxArchVCpu>() {
    unsafe {
        CURRENT_VCPU.current_ref_mut_raw().take();
    }
}
