use alloc::{format, sync::Weak};
// use spinlock::{SpinNoIrq, SpinNoIrqGuard};
use axerrno::{ax_err, ax_err_type, AxResult};
use page_table::PagingIf;
use core::cell::{RefCell, UnsafeCell};

use crate::{arch::AxArchVCpu, config::AxVCpuConfig, AxVM, AxVMHal, GuestPhysAddr};

/// The constant part of `AxVCpu`.
struct AxVCpuInnerConst<H: AxVMHal> {
    /// The id of the vcpu.
    id: usize,
    /// The VM this vcpu belongs to.
    vm: Weak<AxVM<H>>,
    /// The id of the physical CPU who has the priority to run this vcpu. Not implemented yet.
    favor_phys_cpu: usize,
    /// The mask of physical CPUs who can run this vcpu. Not implemented yet.
    affinity: usize,
    /// The entry point of the vcpu.
    entry: GuestPhysAddr,
}

/// The state of a virtual CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VCpuState {
    /// An invalid state.
    Invalid = 0,
    /// The vcpu is free and can be bound to a physical CPU.
    Free = 1,
    /// The vcpu is bound to a physical CPU and ready to run.
    Ready = 2,
    /// The vcpu is running.
    Running = 3,
    /// The vcpu is blocked.
    Blocked = 4,
}

/// The mutable part of `AxVCpu`.
pub struct AxVCpuInnerMut<H: AxVMHal> {
    /// The state of the vcpu.
    state: VCpuState,
    _marker: core::marker::PhantomData<H>,
}

impl<H: AxVMHal> AxVCpuInnerMut<H> {
    pub fn new(_config: AxVCpuConfig) -> AxResult<Self> {
        Ok(Self {
            state: VCpuState::Free,
            _marker: core::marker::PhantomData,
        })
    }
}

/// The width of an access.
///
/// Note that the term "word" here refers to 16-bit data, as in the x86 architecture.
pub enum AccessWidth {
    Byte,
    Word,
    Dword,
    Qword,
}

impl TryFrom<usize> for AccessWidth {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Byte),
            2 => Ok(Self::Word),
            4 => Ok(Self::Dword),
            8 => Ok(Self::Qword),
            _ => Err(()),
        }
    }
}

impl From<AccessWidth> for usize {
    fn from(width: AccessWidth) -> usize {
        match width {
            AccessWidth::Byte => 1,
            AccessWidth::Word => 2,
            AccessWidth::Dword => 4,
            AccessWidth::Qword => 8,
        }
    }
}

type Port = u16;

/// The result of `AxArchVCpu::run`.
pub enum AxArchVCpuExitReason {
    /// The instruction executed by the vcpu performs a MMIO read operation.
    MmioRead {
        addr: GuestPhysAddr,
        width: AccessWidth,
    },
    /// The instruction executed by the vcpu performs a MMIO write operation.
    MmioWrite {
        addr: GuestPhysAddr,
        width: AccessWidth,
        data: u64,
    },
    /// The instruction executed by the vcpu performs a I/O read operation.
    ///
    /// It's unnecessary to specify the destination register because it's always `al`, `ax`, or `eax`.
    IoRead { port: Port, width: AccessWidth },
    /// The instruction executed by the vcpu performs a I/O write operation.
    ///
    /// It's unnecessary to specify the source register because it's always `al`, `ax`, or `eax`.
    IoWrite {
        port: Port,
        width: AccessWidth,
        data: u64,
    },
    /// The vcpu is halted.
    Halt,
}

/// A virtual CPU.
///
/// This struct handles internal mutability itself, almost all the methods are `&self`.
///
/// Note that the `AxVCpu` is not thread-safe. It's caller's responsibility to ensure the safety.
pub struct AxVCpu<H: AxVMHal> {
    /// The constant part of the vcpu.
    inner_const: AxVCpuInnerConst<H>,
    /// The mutable part of the vcpu.
    inner_mut: RefCell<AxVCpuInnerMut<H>>,
    /// The architecture-specific state of the vcpu.
    ///
    /// `UnsafeCell` is used to allow interior mutability.
    ///
    /// `RefCell` or `Mutex` is not suitable here because it's not possible to drop the guard when launching a vcpu.
    arch_vcpu: UnsafeCell<AxArchVCpu<H>>,
}

impl<H: AxVMHal> AxVCpu<H> {
    pub fn new(
        config: AxVCpuConfig,
        id: usize,
        vm: Weak<AxVM<H>>,
        favor_phys_cpu: usize,
        affinity: usize,
    ) -> AxResult<Self> {
        Ok(Self {
            inner_const: AxVCpuInnerConst {
                id,
                vm,
                favor_phys_cpu,
                affinity,
                entry: if id == 0 {
                    config.bsp_entry
                } else {
                    config.ap_entry
                },
            },
            inner_mut: RefCell::new(AxVCpuInnerMut::new(config)?),
            arch_vcpu: UnsafeCell::new(AxArchVCpu::new(config.arch_config)?),
        })
    }

    pub fn init(&self) -> AxResult {
        let vm = self
            .vm()
            .upgrade()
            .ok_or(ax_err_type!(BadState, "VM is dropped"))?;
        let ept_root = vm.ept_root();
        let arch_vcpu = self.get_arch_vcpu();
        debug!("set entry:{:#x}", self.inner_const.entry);
        arch_vcpu.set_entry(self.inner_const.entry)?;
        arch_vcpu.set_ept_root(ept_root)?;
        Ok(())
    }

    pub fn id(&self) -> usize {
        self.inner_const.id
    }

    pub fn is_bsp(&self) -> bool {
        self.inner_const.id == 0
    }

    pub fn vm(&self) -> Weak<AxVM<H>> {
        self.inner_const.vm.clone()
    }

    pub fn state(&self) -> VCpuState {
        self.inner_mut.borrow().state
    }

    pub fn set_state(&self, state: VCpuState) {
        self.inner_mut.borrow_mut().state = state;
    }

    /// Transition the state of the vcpu. If the current state is not `from`, return an error.
    pub fn transition_state(&self, from: VCpuState, to: VCpuState) -> AxResult<()> {
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

    pub fn get_arch_vcpu(&self) -> &mut AxArchVCpu<H> {
        unsafe { &mut *self.arch_vcpu.get() }
    }

    pub fn run(&self) -> AxResult<AxArchVCpuExitReason> {
        self.transition_state(VCpuState::Ready, VCpuState::Running)?;
        set_current_vcpu(&self);
        let result = self.get_arch_vcpu().run()?;
        clear_current_vcpu::<H>();
        self.transition_state(VCpuState::Running, VCpuState::Ready)?;
        Ok(result)
    }

    pub fn bind(&self) -> AxResult<()> {
        self.transition_state(VCpuState::Free, VCpuState::Ready)?;
        self.get_arch_vcpu().bind()
    }

    pub fn unbind(&self) -> AxResult<()> {
        self.transition_state(VCpuState::Ready, VCpuState::Free)?;
        self.get_arch_vcpu().unbind()
    }
}

#[percpu::def_percpu]
static mut CURRENT_VCPU: Option<*mut u8> = None;

pub fn get_current_vcpu<H: AxVMHal>() -> Option<&'static AxVCpu<H>> {
    unsafe {
        CURRENT_VCPU.current_ref_raw().as_ref().copied().map(|p| (p as *const AxVCpu<H>).as_ref()).flatten()
    }
}

pub fn get_current_vcpu_mut<H: AxVMHal>() -> Option<&'static mut AxVCpu<H>> {
    unsafe {
        CURRENT_VCPU.current_ref_mut_raw().as_mut().copied().map(|p| (p as *mut AxVCpu<H>).as_mut()).flatten()
    }
}

pub fn set_current_vcpu<H: AxVMHal>(vcpu: &AxVCpu<H>) {
    unsafe {
        CURRENT_VCPU.current_ref_mut_raw().replace(vcpu as *const _ as *mut u8);
    }
}

pub fn clear_current_vcpu<H: AxVMHal>() {
    unsafe {
        CURRENT_VCPU.current_ref_mut_raw().take();
    }
}
