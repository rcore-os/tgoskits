use core::cell::{RefCell, UnsafeCell};

use axaddrspace::{GuestPhysAddr, HostPhysAddr};
use axerrno::{ax_err, AxResult};

use super::{AxArchVCpu, AxVCpuExitReason};

/// The constant part of `AxVCpu`.
struct AxVCpuInnerConst {
    /// The id of the vcpu.
    id: usize,
    /// The id of the physical CPU who has the priority to run this vcpu.
    favor_phys_cpu: usize,
    /// The set of physical CPUs who can run this vcpu.
    /// If `None`, the vcpu can run on any physical CPU.
    /// Refer to [CPU_SET](https://man7.org/linux/man-pages/man3/CPU_SET.3.html) in Linux.
    phys_cpu_set: Option<usize>,
}

/// The state of a virtual CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VCpuState {
    /// An invalid state.
    Invalid = 0,
    /// The vcpu is created but not initialized yet.
    Created = 1,
    /// The vcpu is already initialized and can be bound to a physical CPU.
    Free = 2,
    /// The vcpu is bound to a physical CPU and ready to run.
    Ready = 3,
    /// The vcpu is bound to a physical CPU and running.
    Running = 4,
    /// The vcpu is blocked.
    Blocked = 5,
}

/// The mutable part of [`AxVCpu`].
pub struct AxVCpuInnerMut {
    /// The state of the vcpu.
    state: VCpuState,
}

/// A virtual CPU with architecture-independent interface.
///
/// By delegating the architecture-specific operations to a struct implementing [`AxArchVCpu`], this struct provides
/// a unified interface and state management model for virtual CPUs of different architectures.
///
/// The architecture-specific operations are delegated to a struct implementing [`AxArchVCpu`].
///
/// Note that:
/// - This struct handles internal mutability itself, almost all the methods are `&self`.
/// - This struct is not thread-safe. It's caller's responsibility to ensure the safety.
pub struct AxVCpu<A: AxArchVCpu> {
    /// The constant part of the vcpu.
    inner_const: AxVCpuInnerConst,
    /// The mutable part of the vcpu.
    inner_mut: RefCell<AxVCpuInnerMut>,
    /// The architecture-specific state of the vcpu.
    ///
    /// `UnsafeCell` is used to allow interior mutability. Note that `RefCell` or `Mutex` is not suitable here
    /// because it's not possible to drop the guard when launching a vcpu.
    arch_vcpu: UnsafeCell<A>,
}

impl<A: AxArchVCpu> AxVCpu<A> {
    /// Create a new [`AxVCpu`].
    pub fn new(
        id: usize,
        favor_phys_cpu: usize,
        phys_cpu_set: Option<usize>,
        arch_config: A::CreateConfig,
    ) -> AxResult<Self> {
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                id,
                favor_phys_cpu,
                phys_cpu_set,
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
        self.manipulate_arch_vcpu(VCpuState::Created, VCpuState::Free, |arch_vcpu| {
            arch_vcpu.set_entry(entry)?;
            arch_vcpu.set_ept_root(ept_root)?;
            arch_vcpu.setup(arch_config)?;
            Ok(())
        })
    }

    /// Get the id of the vcpu.
    pub const fn id(&self) -> usize {
        self.inner_const.id
    }

    /// Get the id of the physical CPU who has the priority to run this vcpu.
    /// Currently unused.
    pub const fn favor_phys_cpu(&self) -> usize {
        self.inner_const.favor_phys_cpu
    }

    /// Get the set of physical CPUs who can run this vcpu.
    /// If `None`, this vcpu has no limitation and can be scheduled on any physical CPU.
    pub const fn phys_cpu_set(&self) -> Option<usize> {
        self.inner_const.phys_cpu_set
    }

    /// Get whether the vcpu is the BSP. We always assume the first vcpu (vcpu with id #0) is the BSP.
    pub const fn is_bsp(&self) -> bool {
        self.inner_const.id == 0
    }

    /// Get the state of the vcpu.
    pub fn state(&self) -> VCpuState {
        self.inner_mut.borrow().state
    }

    /// Set the state of the vcpu.
    /// # Safety
    /// This method is unsafe because it may break the state transition model.
    /// Use it with caution.
    pub unsafe fn set_state(&self, state: VCpuState) {
        self.inner_mut.borrow_mut().state = state;
    }

    /// Execute a block with the state of the vcpu transitioned from `from` to `to`. If the current state is not `from`, return an error.
    ///
    /// The state will be set to [`VCpuState::Invalid`] if an error occurs (including the case that the current state is not `from`).
    ///
    /// The state will be set to `to` if the block is executed successfully.
    pub fn with_state_transition<F, T>(&self, from: VCpuState, to: VCpuState, f: F) -> AxResult<T>
    where
        F: FnOnce() -> AxResult<T>,
    {
        let mut inner_mut = self.inner_mut.borrow_mut();
        if inner_mut.state != from {
            inner_mut.state = VCpuState::Invalid;
            ax_err!(
                BadState,
                format!("VCpu state is not {:?}, but {:?}", from, inner_mut.state)
            )
        } else {
            let result = f();
            inner_mut.state = if result.is_err() {
                VCpuState::Invalid
            } else {
                to
            };
            result
        }
    }

    /// Execute a block with the current vcpu set to `&self`.
    pub fn with_current_cpu_set<F, T>(&self, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        if get_current_vcpu::<A>().is_some() {
            panic!("Nested vcpu operation is not allowed!");
        } else {
            unsafe {
                set_current_vcpu(self);
            }
            let result = f();
            unsafe {
                clear_current_vcpu::<A>();
            }
            result
        }
    }

    /// Execute an operation on the architecture-specific vcpu, with the state transitioned from `from` to `to` and the current vcpu set to `&self`.
    ///
    /// This method is a combination of [`AxVCpu::with_state_transition`] and [`AxVCpu::with_current_cpu_set`].
    pub fn manipulate_arch_vcpu<F, T>(&self, from: VCpuState, to: VCpuState, f: F) -> AxResult<T>
    where
        F: FnOnce(&mut A) -> AxResult<T>,
    {
        self.with_state_transition(from, to, || {
            self.with_current_cpu_set(|| f(self.get_arch_vcpu()))
        })
    }

    /// Transition the state of the vcpu. If the current state is not `from`, return an error.
    pub fn transition_state(&self, from: VCpuState, to: VCpuState) -> AxResult {
        self.with_state_transition(from, to, || Ok(()))
    }

    /// Get the architecture-specific vcpu.
    #[allow(clippy::mut_from_ref)]
    pub fn get_arch_vcpu(&self) -> &mut A {
        unsafe { &mut *self.arch_vcpu.get() }
    }

    /// Run the vcpu.
    pub fn run(&self) -> AxResult<AxVCpuExitReason> {
        self.transition_state(VCpuState::Ready, VCpuState::Running)?;
        self.manipulate_arch_vcpu(VCpuState::Running, VCpuState::Ready, |arch_vcpu| {
            arch_vcpu.run()
        })
    }

    /// Bind the vcpu to the current physical CPU.
    pub fn bind(&self) -> AxResult {
        self.manipulate_arch_vcpu(VCpuState::Free, VCpuState::Ready, |arch_vcpu| {
            arch_vcpu.bind()
        })
    }

    /// Unbind the vcpu from the current physical CPU.
    pub fn unbind(&self) -> AxResult {
        self.manipulate_arch_vcpu(VCpuState::Ready, VCpuState::Free, |arch_vcpu| {
            arch_vcpu.unbind()
        })
    }

    /// Sets the value of a general-purpose register according to the given index.
    pub fn set_gpr(&self, reg: usize, val: usize) {
        self.get_arch_vcpu().set_gpr(reg, val);
    }
}

#[percpu::def_percpu]
static mut CURRENT_VCPU: Option<*mut u8> = None;

/// Get the current vcpu on the current physical CPU.
///
/// It's guaranteed that each time before a method of [`AxArchVCpu`] is called, the current vcpu is set to the corresponding [`AxVCpu`].
/// So methods of [`AxArchVCpu`] can always get the [`AxVCpu`] containing itself by calling this method.
pub fn get_current_vcpu<'a, A: AxArchVCpu>() -> Option<&'a AxVCpu<A>> {
    unsafe {
        CURRENT_VCPU
            .current_ref_raw()
            .as_ref()
            .copied()
            .and_then(|p| (p as *const AxVCpu<A>).as_ref())
    }
}

/// Get a mutable reference to the current vcpu on the current physical CPU.
///
/// See [`get_current_vcpu`] for more details.
pub fn get_current_vcpu_mut<'a, A: AxArchVCpu>() -> Option<&'a mut AxVCpu<A>> {
    unsafe {
        CURRENT_VCPU
            .current_ref_mut_raw()
            .as_mut()
            .copied()
            .and_then(|p| (p as *mut AxVCpu<A>).as_mut())
    }
}

/// Set the current vcpu on the current physical CPU.
///
/// # Safety
/// This method is marked as unsafe because it may result in unexpected behavior if not used properly.
/// Do not call this method unless you know what you are doing.
pub unsafe fn set_current_vcpu<A: AxArchVCpu>(vcpu: &AxVCpu<A>) {
    CURRENT_VCPU
        .current_ref_mut_raw()
        .replace(vcpu as *const _ as *mut u8);
}

/// Clear the current vcpu on the current physical CPU.
///
/// # Safety
/// This method is marked as unsafe because it may result in unexpected behavior if not used properly.
/// Do not call this method unless you know what you are doing.    
pub unsafe fn clear_current_vcpu<A: AxArchVCpu>() {
    CURRENT_VCPU.current_ref_mut_raw().take();
}
