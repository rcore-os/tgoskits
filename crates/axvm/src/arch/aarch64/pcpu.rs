use alloc::collections::VecDeque;
use core::arch::asm;

use spin::{Mutex, Once};

use crate::{HyperCraftHal, HyperResult, HyperError, HostPhysAddr, HostVirtAddr, GuestPhysAddr};
use crate::arch::vcpu::VCpu;
use crate::arch::ContextFrame;
use crate::arch::ContextFrameTrait;

/// need to move to a suitable file?
const PAGE_SIZE_4K: usize = 0x1000;

pub const CPU_MASTER: usize = 0;
pub const CONTEXT_GPR_NUM: usize = 31;
pub const PTE_PER_PAGE: usize = 512;

/// The base address of the per-CPU memory region.
static PER_CPU_BASE: Once<HostPhysAddr> = Once::new();

pub static mut CURRENT_CPU_HV: Mutex<HostPhysAddr> = Mutex::new(0);

pub fn set_current_cpu(addr: HostPhysAddr) {
    unsafe {
        let mut current_cpu = CURRENT_CPU_HV.lock();
        *current_cpu = addr;
    }
}
pub fn get_current_cpu() -> HostPhysAddr {
    unsafe {
        let current_cpu = CURRENT_CPU_HV.lock();
        *current_cpu
    }
}

/// Per-CPU data. A pointer to this struct is loaded into TP when a CPU starts. This structure
/// sits at the top of a secondary CPU's stack.
#[repr(C)]
#[repr(align(4096))]
pub struct PerCpu<H:HyperCraftHal>{   //stack_top_addr has no use yet?
    /// per cpu id
    pub cpu_id: usize,
    /// current active vcpu
    pub active_vcpu: Option<VCpu<H>>,
    /// save for correspond vcpus
    pub vcpu_queue: Mutex<VecDeque<usize>>,
    // stack_top_addr: HostVirtAddr,
    /// context address of this cpu
    pub ctx: Option<usize>,
    
    marker: core::marker::PhantomData<H>,
}

impl <H: HyperCraftHal + 'static> PerCpu<H> {
    const fn new(cpu_id: usize) -> Self {
        Self {
            cpu_id: cpu_id,
            active_vcpu: None,
            // stack_top_addr: stack_top_addr,
            vcpu_queue: Mutex::new(VecDeque::new()),
            marker: core::marker::PhantomData,
            ctx: None,
        }
    }

    /// Initializes the `PerCpu` structures for each CPU. This (the boot CPU's) per-CPU
    /// area is initialized and loaded into TPIDR_EL1 as well.
    pub fn init(boot_id: usize) -> HyperResult<()> {
        let cpu_nums: usize = 2;
        let pcpu_size = core::mem::size_of::<PerCpu<H>>() * cpu_nums;
        debug!("pcpu_size: {:#x}", pcpu_size);
        let pcpu_pages = H::alloc_pages((pcpu_size + PAGE_SIZE_4K - 1) / PAGE_SIZE_4K)
            .ok_or(HyperError::NoMemory)?;
        debug!("pcpu_pages: {:#x}", pcpu_pages);
        PER_CPU_BASE.call_once(|| pcpu_pages);
        for cpu_id in 0..cpu_nums {
            /*let stack_top_addr = if cpu_id == boot_id {
                let boot_stack_top = Self::boot_cpu_stack()?;
                debug!("boot_stack_top: {:#x}", boot_stack_top);
                boot_stack_top
            } else {
                H::alloc_pages((stack_size + PAGE_SIZE_4K - 1) / PAGE_SIZE_4K)
                    .ok_or(HyperError::NoMemory)?
            };*/
            let pcpu: PerCpu<H> = Self::new(cpu_id);
            let ptr = Self::ptr_for_cpu(cpu_id);
            // Safety: ptr is guaranteed to be properly aligned and point to valid memory owned by
            // PerCpu. No other CPUs are alive at this point, so it cannot be concurrently modified
            // either.
            unsafe { core::ptr::write(ptr as *mut PerCpu<H>, pcpu) };
        }

        // Initialize TP register and set this CPU online to be consistent with secondary CPUs.
        Self::setup_this_cpu(boot_id)?;

        Ok(())
    }

    /// Initializes the TP pointer to point to PerCpu data.
    pub fn setup_this_cpu(cpu_id: usize) -> HyperResult<()> {
        // Load TP with address of pur PerCpu struct.
        let tp = PER_CPU_BASE.get().unwrap() + cpu_id * core::mem::size_of::<PerCpu<H>>();
        // let tp = Self::ptr_for_cpu(cpu_id) as usize;
        // unsafe {
            // asm!("msr TPIDR_EL1, {}", in(reg) tp)
        // };
        set_current_cpu(tp);
        Ok(())
    }

    /// Returns this CPU's `PerCpu` structure.
    pub fn this_cpu() -> &'static mut PerCpu<H> {
        // Make sure PerCpu has been set up.
        assert!(PER_CPU_BASE.get().is_some());
        // let tp: u64;
        let tp = get_current_cpu() as u64;
        // unsafe { core::arch::asm!("mrs {}, TPIDR_EL1", out(reg) tp) };
        // let pcpu_ptr = tp as *mut PerCpu<H>;
        let pcpu_ptr = tp as *mut PerCpu<H>;
        let pcpu = unsafe {
            // Safe since TP is set uo to point to a valid PerCpu
            pcpu_ptr.as_mut().unwrap()
        };
        pcpu
    }

    /// Create a `Vcpu`, set the entry point to `entry` and bind this vcpu into the current CPU.
    pub fn create_vcpu(&mut self, vm_id: usize, vcpu_id: usize) -> HyperResult<VCpu<H>> {
        self.vcpu_queue.lock().push_back(vcpu_id);
        let vcpu = VCpu::<H>::new(vm_id, vcpu_id, self.cpu_id);
        let result = Ok(vcpu);
        result
    }
    
    /// Get the current active vcpu.
    pub fn set_active_vcpu(&mut self, active_vcpu: Option<VCpu<H>>) {
        self.active_vcpu = active_vcpu;
    }
    
    /// Get the current active vcpu.
    pub fn get_active_vcpu(&self) -> Option<&VCpu<H>> {
        self.active_vcpu.as_ref()
    }

    /// Get the current active vcpu.
    pub fn get_active_vcpu_mut(&mut self) -> Option<&mut VCpu<H>> {
        self.active_vcpu.as_mut()
    }

    /// Returns a pointer to the `PerCpu` for the given CPU.
    pub fn ptr_for_cpu(cpu_id: usize) ->  &'static mut PerCpu<H> {
        let pcpu_addr = PER_CPU_BASE.get().unwrap() + cpu_id * core::mem::size_of::<PerCpu<H>>();
        let pcpu_ptr = pcpu_addr as *mut PerCpu<H>;
        let pcpu = unsafe {
            // Safe since TP is set uo to point to a valid PerCpu
            pcpu_ptr.as_mut().unwrap()
        };
        pcpu
    }

    /// set ctx of this cpu
    pub fn set_ctx(&mut self, ctx: &mut ContextFrame) {
        self.ctx = Some(ctx as *mut _ as usize);
    }

    /// get ctx of this cpu
    pub fn get_ctx(&mut self) -> Option<&mut ContextFrame> {
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                Some(unsafe { &mut *ctx })
            }
            None => None,
        }
    }

    /// clear ctx of this cpu
    pub fn clear_ctx(&mut self) {
        self.ctx = None;
    }

    /// set gpr by index of this cpu ctx
    pub fn set_gpr(&self, idx: usize, val: usize) {
        if idx >= CONTEXT_GPR_NUM {
            return;
        }
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                unsafe {
                    (*ctx).set_gpr(idx, val);
                }
            }
            None => {}
        }
    }

    /// get gpr by index of this cpu ctx
    pub fn get_gpr(&self, idx: usize) -> usize {
        if idx >= CONTEXT_GPR_NUM {
            return 0;
        }
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                unsafe { (*ctx).gpr(idx) }
            }
            None => 0,
        }
    }

    /// get elr of this cpu ctx
    pub fn get_elr(&self) -> usize {
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                unsafe { (*ctx).exception_pc() }
            }
            None => 0,
        }
    }

    /// get sp of this cpu ctx
    pub fn get_spsr(&self) -> usize {
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                unsafe { (*ctx).spsr as usize }
            }
            None => 0,
        }
    }

    /// get sp of this cpu ctx
    pub fn set_elr(&self, val: usize) {
        match self.ctx {
            Some(ctx_addr) => {
                if ctx_addr < 0x1000 {
                    panic!("illegal ctx addr {:x}", ctx_addr);
                }
                let ctx = ctx_addr as *mut ContextFrame;
                unsafe { (*ctx).set_exception_pc(val) }
            }
            None => {}
        }
    }
}
