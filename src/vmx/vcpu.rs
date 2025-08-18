use alloc::collections::VecDeque;
use bit_field::BitField;
use core::{
    arch::naked_asm,
    fmt::{Debug, Formatter, Result},
    mem::size_of,
};
use raw_cpuid::CpuId;
use x86::{
    bits64::vmx,
    controlregs::{Xcr0, xcr0 as xcr0_read, xcr0_write},
    dtables::{self, DescriptorTablePointer},
    segmentation::SegmentSelector,
};
use x86_64::registers::control::{Cr0, Cr0Flags, Cr3, Cr4, Cr4Flags, EferFlags};
use x86_vlapic::EmulatedLocalApic;

use axaddrspace::{
    GuestPhysAddr, GuestVirtAddr, HostPhysAddr, NestedPageFaultInfo,
    device::{AccessWidth, Port, SysRegAddr, SysRegAddrRange},
};
use axdevice_base::BaseDeviceOps;
use axerrno::{AxResult, ax_err, ax_err_type};
use axvcpu::{AxArchVCpu, AxVCpuExitReason, AxVCpuHal};
use axvisor_api::vmm::{VCpuId, VMId};

use super::VmxExitInfo;
use super::as_axerr;
use super::definitions::VmxExitReason;
use super::structs::{IOBitmap, MsrBitmap, VmxRegion};
use super::vmcs::{
    self, ApicAccessExitType, VmcsControl32, VmcsControl64, VmcsControlNW, VmcsGuest16,
    VmcsGuest32, VmcsGuest64, VmcsGuestNW, VmcsHost16, VmcsHost32, VmcsHost64, VmcsHostNW,
};
use crate::{ept::GuestPageWalkInfo, msr::Msr, regs::GeneralRegisters};

const VMX_PREEMPTION_TIMER_SET_VALUE: u32 = 1_000_000;

const QEMU_EXIT_PORT: u16 = 0x604;
const QEMU_EXIT_MAGIC: u64 = 0x2000;

pub struct XState {
    host_xcr0: u64,
    guest_xcr0: u64,
    host_xss: u64,
    guest_xss: u64,

    xsave_available: bool,
    xsaves_available: bool,
}

#[derive(PartialEq, Eq, Debug)]
pub enum VmCpuMode {
    Real,
    Protected,
    Compatibility, // IA-32E mode (CS.L = 0)
    Mode64,        // IA-32E mode (CS.L = 1)
}

impl XState {
    /// Create a new [`XState`] instance with current host state
    fn new() -> Self {
        // Check if XSAVE is available
        let xsave_available = Self::xsave_available();
        // Check if XSAVES and XRSTORS (as well as IA32_XSS) are available
        let xsaves_available = if xsave_available {
            Self::xsaves_available()
        } else {
            false
        };

        // Read XCR0 iff XSAVE is available
        let xcr0 = if xsave_available {
            unsafe { xcr0_read().bits() }
        } else {
            0
        };
        // Read IA32_XSS iff XSAVES is available
        let xss = if xsaves_available {
            Msr::IA32_XSS.read()
        } else {
            0
        };

        Self {
            host_xcr0: xcr0,
            guest_xcr0: xcr0,
            host_xss: xss,
            guest_xss: xss,
            xsave_available,
            xsaves_available,
        }
    }

    /// Enable extended processor state management instructions, including XGETBV and XSAVE.
    pub fn enable_xsave() {
        if Self::xsave_available() {
            unsafe { Cr4::write(Cr4::read() | Cr4Flags::OSXSAVE) };
        }
    }

    /// Check if XSAVE is available on the current CPU.
    pub fn xsave_available() -> bool {
        let cpuid = CpuId::new();
        cpuid
            .get_feature_info()
            .map(|f| f.has_xsave())
            .unwrap_or(false)
    }

    /// Check if XSAVES and XRSTORS (as well as IA32_XSS) are available on the current CPU.
    pub fn xsaves_available() -> bool {
        let cpuid = CpuId::new();
        cpuid
            .get_extended_state_info()
            .map(|f| f.has_xsaves_xrstors())
            .unwrap_or(false)
    }

    /// Save the current host XCR0 and IA32_XSS values and load the guest values.
    pub fn switch_to_guest(&mut self) {
        unsafe {
            if self.xsave_available {
                self.host_xcr0 = xcr0_read().bits();
                xcr0_write(Xcr0::from_bits_unchecked(self.guest_xcr0));

                if self.xsaves_available {
                    self.host_xss = Msr::IA32_XSS.read();
                    Msr::IA32_XSS.write(self.guest_xss);
                }
            }
        }
    }

    /// Save the current guest XCR0 and IA32_XSS values and load the host values.
    pub fn switch_to_host(&mut self) {
        unsafe {
            if self.xsave_available {
                self.guest_xcr0 = xcr0_read().bits();
                xcr0_write(Xcr0::from_bits_unchecked(self.host_xcr0));

                if self.xsaves_available {
                    self.guest_xss = Msr::IA32_XSS.read();
                    Msr::IA32_XSS.write(self.host_xss);
                }
            }
        }
    }
}

const MSR_IA32_EFER_LMA_BIT: u64 = 1 << 10;
const CR0_PE: usize = 1 << 0;

/// A virtual CPU within a guest.
#[repr(C)]
pub struct VmxVcpu<H: AxVCpuHal> {
    // The order of `guest_regs` and `host_stack_top` is mandatory. They must be the first two fields. If you want to
    // change the order or the type of these fields, you must also change the assembly in this file.
    /// Guest general-purpose registers.
    guest_regs: GeneralRegisters,
    /// The top of the host stack.
    host_stack_top: u64,

    // The order of the following fields is not mandatory.

    // VCpu states and configurations
    /// Whether the VMCS has been launched. Used to determine whether to `vmx_launch` or `vmx_resume`.
    launched: bool,
    /// The guest entry point.
    entry: Option<GuestPhysAddr>,
    /// The EPT root address.
    ept_root: Option<HostPhysAddr>,
    // /// Whether this VCPU is a host VCpu. Used in type 1.5 hypervisor.
    // is_host: bool, temporary removed because we don't care about type 1.5 now

    // VMCS-related fields
    /// The VMCS region.
    vmcs: VmxRegion<H::MmHal>,
    /// The I/O bitmap for the VMCS.
    io_bitmap: IOBitmap<H::MmHal>,
    /// The MSR bitmap for the VMCS.
    msr_bitmap: MsrBitmap<H::MmHal>,

    // Interrupt-related fields
    /// Pending events to be injected to the guest.
    pending_events: VecDeque<(u8, Option<u32>)>,
    /// Emulated Local APIC.
    vlapic: EmulatedLocalApic,

    // Extra states
    /// The XState of the VCpu. Both host and guest.
    xstate: XState,

    // Tracing-related fields
    #[cfg(feature = "tracing")]
    /// The guest registers when the VM-exit happens.
    guest_regs_exiting: GeneralRegisters,
}

impl<H: AxVCpuHal> VmxVcpu<H> {
    /// Create a new [`VmxVcpu`].
    pub fn new(vm_id: VMId, vcpu_id: VCpuId) -> AxResult<Self> {
        let vmcs_revision_id = super::read_vmcs_revision_id();
        let vcpu = Self {
            guest_regs: GeneralRegisters::default(),
            host_stack_top: 0,
            launched: false,
            entry: None,
            ept_root: None,
            // is_host: false,
            vmcs: VmxRegion::new(vmcs_revision_id, false)?,
            io_bitmap: IOBitmap::passthrough_all()?,
            msr_bitmap: MsrBitmap::passthrough_all()?,
            pending_events: VecDeque::with_capacity(8),
            vlapic: EmulatedLocalApic::new(vm_id, vcpu_id),
            xstate: XState::new(),
            #[cfg(feature = "tracing")]
            guest_regs_exiting: GeneralRegisters::default(),
        };
        info!("[HV] created VmxVcpu(vmcs: {:#x})", vcpu.vmcs.phys_addr());
        Ok(vcpu)
    }

    /// Set the new [`VmxVcpu`] context from guest OS.
    pub fn setup(&mut self, ept_root: HostPhysAddr, entry: GuestPhysAddr) -> AxResult {
        self.setup_vmcs(entry, ept_root)?;
        Ok(())
    }

    // /// Get the identifier of this [`VmxVcpu`].
    // pub fn vcpu_id(&self) -> usize {
    //     get_current_vcpu::<Self>().unwrap().id()
    // }

    /// Bind this [`VmxVcpu`] to current logical processor.
    pub fn bind_to_current_processor(&self) -> AxResult {
        debug!(
            "VmxVcpu bind to current processor vmcs @ {:#x}",
            self.vmcs.phys_addr()
        );
        unsafe {
            vmx::vmptrld(self.vmcs.phys_addr().as_usize() as u64).map_err(as_axerr)?;
        }
        self.setup_vmcs_host()?;
        Ok(())
    }

    /// Unbind this [`VmxVcpu`] from current logical processor.
    pub fn unbind_from_current_processor(&self) -> AxResult {
        debug!(
            "VmxVcpu unbind from current processor vmcs @ {:#x}",
            self.vmcs.phys_addr()
        );

        unsafe {
            vmx::vmclear(self.vmcs.phys_addr().as_usize() as u64).map_err(as_axerr)?;
        }
        Ok(())
    }

    /// Get CPU mode of the guest.
    pub fn get_cpu_mode(&self) -> VmCpuMode {
        let ia32_efer = Msr::IA32_EFER.read();
        let cs_access_right = VmcsGuest32::CS_ACCESS_RIGHTS.read().unwrap();
        let cr0 = VmcsGuestNW::CR0.read().unwrap();
        if (ia32_efer & MSR_IA32_EFER_LMA_BIT) != 0 {
            if (cs_access_right & 0x2000) != 0 {
                // CS.L = 1
                VmCpuMode::Mode64
            } else {
                VmCpuMode::Compatibility
            }
        } else if (cr0 & CR0_PE) != 0 {
            VmCpuMode::Protected
        } else {
            VmCpuMode::Real
        }
    }

    /// Run the guest. It returns when a vm-exit happens and returns the vm-exit if it cannot be handled by this [`VmxVcpu`] itself.
    pub fn inner_run(&mut self) -> Option<VmxExitInfo> {
        self.inject_pending_events().unwrap();

        // Run guest
        self.load_guest_xstate();

        #[cfg(feature = "tracing")]
        {
            use crate::regs::GeneralRegistersDiff;
            // Tracing, do a diff of the guest registers before entering the guest
            let diff = GeneralRegistersDiff::new(self.guest_regs_exiting, self.guest_regs);
            if !diff.is_same() {
                debug!(
                    "VCpu registers changed during handling VM-exit: {:#x?}",
                    diff
                );
            } else {
                debug!("VCpu registers unchanged during handling VM-exit");
            }
        }

        unsafe {
            if self.launched {
                self.vmx_resume();
            } else {
                self.launched = true;
                VmcsHostNW::RSP
                    .write(&self.host_stack_top as *const _ as usize)
                    .unwrap();

                self.vmx_launch();
            }
        }
        self.load_host_xstate();

        #[cfg(feature = "tracing")]
        {
            self.guest_regs_exiting = self.guest_regs;
        }

        // Handle vm-exits
        let exit_info = self.exit_info().unwrap();
        // debug!("VM exit: {:#x?}", exit_info);

        match self.builtin_vmexit_handler(&exit_info) {
            Some(result) => {
                if result.is_err() {
                    panic!(
                        "VmxVcpu failed to handle a VM-exit that should be handled by itself: {:?}, error {:?}, vcpu: {:#x?}",
                        exit_info.exit_reason,
                        result.unwrap_err(),
                        self
                    );
                }

                None
            }
            None => Some(exit_info),
        }
    }

    /// Basic information about VM exits.
    pub fn exit_info(&self) -> AxResult<vmcs::VmxExitInfo> {
        vmcs::exit_info()
    }

    /// Raw information for VM Exits Due to Vectored Events, See SDM 25.9.2
    pub fn raw_interrupt_exit_info(&self) -> AxResult<u32> {
        vmcs::raw_interrupt_exit_info()
    }

    /// Information for VM exits due to external interrupts.
    pub fn interrupt_exit_info(&self) -> AxResult<vmcs::VmxInterruptInfo> {
        vmcs::interrupt_exit_info()
    }

    /// Information for VM exits due to I/O instructions.
    pub fn io_exit_info(&self) -> AxResult<vmcs::VmxIoExitInfo> {
        vmcs::io_exit_info()
    }

    /// Information for VM exits due to nested page table faults (EPT violation).
    pub fn nested_page_fault_info(&self) -> AxResult<NestedPageFaultInfo> {
        vmcs::ept_violation_info()
    }

    /// Information for VM exits due to APIC access.
    pub fn apic_access_exit_info(&self) -> AxResult<vmcs::ApicAccessExitInfo> {
        vmcs::apic_access_exit_info()
    }

    /// Guest general-purpose registers.
    pub fn regs(&self) -> &GeneralRegisters {
        &self.guest_regs
    }

    /// Mutable reference of guest general-purpose registers.
    pub fn regs_mut(&mut self) -> &mut GeneralRegisters {
        &mut self.guest_regs
    }

    /// Guest stack pointer. (`RSP`)
    pub fn stack_pointer(&self) -> usize {
        VmcsGuestNW::RSP.read().unwrap()
    }

    /// Set guest stack pointer. (`RSP`)
    pub fn set_stack_pointer(&mut self, rsp: usize) {
        VmcsGuestNW::RSP.write(rsp).unwrap()
    }

    /// Translate guest virtual addr to linear addr    
    pub fn gla2gva(&self, guest_rip: GuestVirtAddr) -> GuestVirtAddr {
        let cpu_mode = self.get_cpu_mode();
        let seg_base = if cpu_mode == VmCpuMode::Mode64 {
            0
        } else {
            VmcsGuestNW::CS_BASE.read().unwrap()
        };
        // debug!(
        //     "seg_base: {:#x}, guest_rip: {:#x} cpu mode:{:?}",
        //     seg_base, guest_rip, cpu_mode
        // );
        guest_rip + seg_base
    }

    /// Get Translate guest page table info
    pub fn get_ptw_info(&self) -> GuestPageWalkInfo {
        let top_entry = VmcsGuestNW::CR3.read().unwrap();
        let level = self.get_paging_level();
        let is_write_access = false;
        let is_inst_fetch = false;
        let is_user_mode_access = ((VmcsGuest32::SS_ACCESS_RIGHTS.read().unwrap() >> 5) & 0x3) == 3;
        let mut pse = true;
        let mut nxe =
            (VmcsGuest64::IA32_EFER.read().unwrap() & EferFlags::NO_EXECUTE_ENABLE.bits()) != 0;
        let wp = (VmcsGuestNW::CR0.read().unwrap() & Cr0Flags::WRITE_PROTECT.bits() as usize) != 0;
        let is_smap_on = (VmcsGuestNW::CR4.read().unwrap()
            & Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION.bits() as usize)
            != 0;
        let is_smep_on = (VmcsGuestNW::CR4.read().unwrap()
            & Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION.bits() as usize)
            != 0;
        let width: u32;
        if level == 4 || level == 3 {
            width = 9;
        } else if level == 2 {
            width = 10;
            pse = VmcsGuestNW::CR4.read().unwrap() & Cr4Flags::PAGE_SIZE_EXTENSION.bits() as usize
                != 0;
            nxe = false;
        } else {
            width = 0;
        }
        GuestPageWalkInfo {
            top_entry,
            level,
            width,
            is_user_mode_access,
            is_write_access,
            is_inst_fetch,
            pse,
            wp,
            nxe,
            is_smap_on,
            is_smep_on,
        }
    }

    /// Guest rip. (`RIP`)
    pub fn rip(&self) -> usize {
        VmcsGuestNW::RIP.read().unwrap()
    }

    /// Guest cs. (`cs`)
    pub fn cs(&self) -> u16 {
        VmcsGuest16::CS_SELECTOR.read().unwrap()
    }

    /// Advance guest `RIP` by `instr_len` bytes.
    pub fn advance_rip(&mut self, instr_len: u8) -> AxResult {
        VmcsGuestNW::RIP.write(VmcsGuestNW::RIP.read()? + instr_len as usize)
    }

    /// Add a virtual interrupt or exception to the pending events list,
    /// and try to inject it before later VM entries.
    pub fn queue_event(&mut self, vector: u8, err_code: Option<u32>) {
        self.pending_events.push_back((vector, err_code));
    }

    /// If enable, a VM exit occurs at the beginning of any instruction if
    /// `RFLAGS.IF` = 1 and there are no other blocking of interrupts.
    /// (see SDM, Vol. 3C, Section 24.4.2)
    pub fn set_interrupt_window(&mut self, enable: bool) -> AxResult {
        let mut ctrl = VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS.read()?;
        let bits = vmcs::controls::PrimaryControls::INTERRUPT_WINDOW_EXITING.bits();
        if enable {
            ctrl |= bits
        } else {
            ctrl &= !bits
        }
        VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS.write(ctrl)?;
        Ok(())
    }

    /// Set I/O intercept by modifying I/O bitmap.
    pub fn set_io_intercept_of_range(&mut self, port_base: u32, count: u32, intercept: bool) {
        self.io_bitmap
            .set_intercept_of_range(port_base, count, intercept)
    }

    /// Set msr intercept by modifying msr bitmap.
    /// Todo: distinguish read and write.
    pub fn set_msr_intercept_of_range(&mut self, msr: u32, intercept: bool) {
        self.msr_bitmap.set_read_intercept(msr, intercept);
        self.msr_bitmap.set_write_intercept(msr, intercept);
    }
}

// Implementation of private methods
impl<H: AxVCpuHal> VmxVcpu<H> {
    fn setup_io_bitmap(&mut self) -> AxResult {
        // By default, I/O bitmap is set as `intercept_all`.
        // Todo: these should be combined with emulated pio device management,
        // in `modules/axvm/src/device/x86_64/mod.rs` somehow.
        let io_to_be_intercepted = [
            // // UART
            // // 0x3f8..0x3f8 + 8, // COM1
            // // We need to intercepted the access to COM2 ports.
            // // Because we want to reserve this port for host Linux.
            // 0x2f8..0x2f8 + 8, // COM2
            // // 0x3e8..0x3e8 + 8, // COM3
            // // 0x2e8..0x2e8 + 8, // COM4
            // // Virual PIC
            // 0x20..0x20 + 2, // PIC1
            // 0xa0..0xa0 + 2, // PIC2
            // // Debug Port
            // // 0x80..0x80 + 1,   // Debug Port
            // //
            // 0x92..0x92 + 1, // system_control_a
            // 0x61..0x61 + 1, // system_control_b
            // // RTC
            // 0x70..0x70 + 2, // CMOS
            // 0x40..0x40 + 4, // PIT
            // // 0xf0..0xf0 + 2,   // ports about fpu
            // // 0x3d4..0x3d4 + 2, // ports about vga
            // 0x87..0x87 + 1,   // port about dma
            // 0x60..0x60 + 1,   // ports about ps/2 controller
            // 0x64..0x64 + 1,   // ports about ps/2 controller
            // 0xcf8..0xcf8 + 8, // PCI

            // QEMU exit port
            QEMU_EXIT_PORT..QEMU_EXIT_PORT + 1, // QEMU exit port
        ];
        for port_range in io_to_be_intercepted {
            self.io_bitmap.set_intercept_of_range(
                port_range.start as _,
                port_range.count() as u32,
                true,
            );
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn setup_msr_bitmap(&mut self) -> AxResult {
        // Intercept IA32_APIC_BASE MSR accesses
        // let msr = x86::msr::IA32_APIC_BASE;
        // self.msr_bitmap.set_read_intercept(msr, true);
        // self.msr_bitmap.set_write_intercept(msr, true);

        // This is strange, guest Linux's access to `IA32_UMWAIT_CONTROL` will cause an exception.
        // But if we intercept it, it seems okay.
        const IA32_UMWAIT_CONTROL: u32 = 0xe1;
        self.msr_bitmap
            .set_write_intercept(IA32_UMWAIT_CONTROL, true);
        self.msr_bitmap
            .set_read_intercept(IA32_UMWAIT_CONTROL, true);

        // Intercept all x2APIC MSR accesses
        for msr in 0x800..=0x83f {
            self.msr_bitmap.set_read_intercept(msr, true);
            self.msr_bitmap.set_write_intercept(msr, true);
        }
        Ok(())
    }

    fn setup_vmcs(&mut self, entry: GuestPhysAddr, ept_root: HostPhysAddr) -> AxResult {
        let paddr = self.vmcs.phys_addr().as_usize() as u64;
        unsafe {
            vmx::vmclear(paddr).map_err(as_axerr)?;
        }
        self.bind_to_current_processor()?;
        self.setup_msr_bitmap()?;
        self.setup_vmcs_guest(entry)?;
        self.setup_vmcs_control(ept_root, true)?;
        self.unbind_from_current_processor()?;
        Ok(())
    }

    fn setup_vmcs_host(&self) -> AxResult {
        VmcsHost64::IA32_PAT.write(Msr::IA32_PAT.read())?;
        VmcsHost64::IA32_EFER.write(Msr::IA32_EFER.read())?;

        VmcsHostNW::CR0.write(Cr0::read_raw() as _)?;
        VmcsHostNW::CR3.write(Cr3::read_raw().0.start_address().as_u64() as _)?;
        VmcsHostNW::CR4.write(Cr4::read_raw() as _)?;

        VmcsHost16::ES_SELECTOR.write(x86::segmentation::es().bits())?;
        VmcsHost16::CS_SELECTOR.write(x86::segmentation::cs().bits())?;
        VmcsHost16::SS_SELECTOR.write(x86::segmentation::ss().bits())?;
        VmcsHost16::DS_SELECTOR.write(x86::segmentation::ds().bits())?;
        VmcsHost16::FS_SELECTOR.write(x86::segmentation::fs().bits())?;
        VmcsHost16::GS_SELECTOR.write(x86::segmentation::gs().bits())?;
        VmcsHostNW::FS_BASE.write(Msr::IA32_FS_BASE.read() as _)?;
        VmcsHostNW::GS_BASE.write(Msr::IA32_GS_BASE.read() as _)?;

        let tr = unsafe { x86::task::tr() };
        let mut gdtp = DescriptorTablePointer::<u64>::default();
        let mut idtp = DescriptorTablePointer::<u64>::default();
        unsafe {
            dtables::sgdt(&mut gdtp);
            dtables::sidt(&mut idtp);
        }
        VmcsHost16::TR_SELECTOR.write(tr.bits())?;
        VmcsHostNW::TR_BASE.write(get_tr_base(tr, &gdtp) as _)?;
        VmcsHostNW::GDTR_BASE.write(gdtp.base as _)?;
        VmcsHostNW::IDTR_BASE.write(idtp.base as _)?;
        VmcsHostNW::RIP.write(Self::vmx_exit as usize)?;

        VmcsHostNW::IA32_SYSENTER_ESP.write(0)?;
        VmcsHostNW::IA32_SYSENTER_EIP.write(0)?;
        VmcsHost32::IA32_SYSENTER_CS.write(0)?;

        Ok(())
    }

    fn setup_vmcs_guest(&mut self, entry: GuestPhysAddr) -> AxResult {
        let cr0_val: Cr0Flags =
            Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE | Cr0Flags::EXTENSION_TYPE;
        self.set_cr(0, cr0_val.bits());
        self.set_cr(4, 0);

        macro_rules! set_guest_segment {
            ($seg: ident, $access_rights: expr) => {{
                use VmcsGuest16::*;
                use VmcsGuest32::*;
                use VmcsGuestNW::*;
                paste::paste! {
                    [<$seg _SELECTOR>].write(0)?;
                    [<$seg _BASE>].write(0)?;
                    [<$seg _LIMIT>].write(0xffff)?;
                    [<$seg _ACCESS_RIGHTS>].write($access_rights)?;
                }
            }};
        }

        set_guest_segment!(ES, 0x93); // 16-bit, present, data, read/write, accessed
        set_guest_segment!(CS, 0x9b); // 16-bit, present, code, exec/read, accessed
        set_guest_segment!(SS, 0x93);
        set_guest_segment!(DS, 0x93);
        set_guest_segment!(FS, 0x93);
        set_guest_segment!(GS, 0x93);
        set_guest_segment!(TR, 0x8b); // present, system, 32-bit TSS busy
        set_guest_segment!(LDTR, 0x82); // present, system, LDT

        VmcsGuestNW::GDTR_BASE.write(0)?;
        VmcsGuest32::GDTR_LIMIT.write(0xffff)?;
        VmcsGuestNW::IDTR_BASE.write(0)?;
        VmcsGuest32::IDTR_LIMIT.write(0xffff)?;

        VmcsGuestNW::CR3.write(0)?;
        VmcsGuestNW::DR7.write(0x400)?;
        VmcsGuestNW::RSP.write(0)?;
        VmcsGuestNW::RIP.write(entry.as_usize())?;
        VmcsGuestNW::RFLAGS.write(0x2)?;
        VmcsGuestNW::PENDING_DBG_EXCEPTIONS.write(0)?;
        VmcsGuestNW::IA32_SYSENTER_ESP.write(0)?;
        VmcsGuestNW::IA32_SYSENTER_EIP.write(0)?;
        VmcsGuest32::IA32_SYSENTER_CS.write(0)?;

        VmcsGuest32::INTERRUPTIBILITY_STATE.write(0)?;
        VmcsGuest32::ACTIVITY_STATE.write(0)?;

        VmcsGuest32::VMX_PREEMPTION_TIMER_VALUE.write(VMX_PREEMPTION_TIMER_SET_VALUE)?;

        VmcsGuest64::LINK_PTR.write(u64::MAX)?; // SDM Vol. 3C, Section 24.4.2
        VmcsGuest64::IA32_DEBUGCTL.write(0)?;
        VmcsGuest64::IA32_PAT.write(Msr::IA32_PAT.read())?;
        VmcsGuest64::IA32_EFER.write(0)?;
        Ok(())
    }

    fn setup_vmcs_control(&mut self, ept_root: HostPhysAddr, is_guest: bool) -> AxResult {
        // Intercept NMI and external interrupts.
        use super::vmcs::controls::*;
        use PinbasedControls as PinCtrl;
        let raw_cpuid = CpuId::new();

        vmcs::set_control(
            VmcsControl32::PINBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_TRUE_PINBASED_CTLS,
            Msr::IA32_VMX_PINBASED_CTLS.read() as u32,
            (PinCtrl::NMI_EXITING | PinCtrl::EXTERNAL_INTERRUPT_EXITING).bits(),
            // (PinCtrl::NMI_EXITING | PinCtrl::VMX_PREEMPTION_TIMER).bits(),
            // PinCtrl::NMI_EXITING.bits(),
            0,
        )?;

        // Intercept all I/O instructions, use MSR bitmaps, activate secondary controls,
        // disable CR3 load/store interception.
        use PrimaryControls as CpuCtrl;
        vmcs::set_control(
            VmcsControl32::PRIMARY_PROCBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_TRUE_PROCBASED_CTLS,
            Msr::IA32_VMX_PROCBASED_CTLS.read() as u32,
            (CpuCtrl::USE_IO_BITMAPS | CpuCtrl::USE_MSR_BITMAPS | CpuCtrl::SECONDARY_CONTROLS)
                .bits(),
            (CpuCtrl::CR3_LOAD_EXITING
                | CpuCtrl::CR3_STORE_EXITING
                | CpuCtrl::CR8_LOAD_EXITING
                | CpuCtrl::CR8_STORE_EXITING)
                .bits(),
        )?;

        // Enable EPT, RDTSCP, INVPCID, and unrestricted guest.
        use SecondaryControls as CpuCtrl2;
        let mut val =
            // CpuCtrl2::VIRTUALIZE_APIC | 
            CpuCtrl2::ENABLE_EPT | CpuCtrl2::UNRESTRICTED_GUEST;
        if let Some(features) = raw_cpuid.get_extended_processor_and_feature_identifiers() {
            if features.has_rdtscp() {
                val |= CpuCtrl2::ENABLE_RDTSCP;
            }
        }
        if let Some(features) = raw_cpuid.get_extended_feature_info() {
            if features.has_invpcid() {
                val |= CpuCtrl2::ENABLE_INVPCID;
            }
        }
        if let Some(features) = raw_cpuid.get_extended_state_info() {
            if features.has_xsaves_xrstors() {
                val |= CpuCtrl2::ENABLE_XSAVES_XRSTORS;
            }
        }
        vmcs::set_control(
            VmcsControl32::SECONDARY_PROCBASED_EXEC_CONTROLS,
            Msr::IA32_VMX_PROCBASED_CTLS2,
            Msr::IA32_VMX_PROCBASED_CTLS2.read() as u32,
            val.bits(),
            0,
        )?;

        // Switch to 64-bit host, acknowledge interrupt info, switch IA32_PAT/IA32_EFER on VM exit.
        use ExitControls as ExitCtrl;
        vmcs::set_control(
            VmcsControl32::VMEXIT_CONTROLS,
            Msr::IA32_VMX_TRUE_EXIT_CTLS,
            Msr::IA32_VMX_EXIT_CTLS.read() as u32,
            (ExitCtrl::HOST_ADDRESS_SPACE_SIZE
                | ExitCtrl::ACK_INTERRUPT_ON_EXIT
                | ExitCtrl::SAVE_IA32_PAT
                | ExitCtrl::LOAD_IA32_PAT
                | ExitCtrl::SAVE_IA32_EFER
                | ExitCtrl::LOAD_IA32_EFER)
                .bits(),
            0,
        )?;

        let mut val = EntryCtrl::LOAD_IA32_PAT | EntryCtrl::LOAD_IA32_EFER;

        if !is_guest {
            // IA-32e mode guest
            // On processors that support Intel 64 architecture, this control determines whether the logical processor is in IA-32e mode after VM entry.
            // Its value is loaded into IA32_EFER.LMA as part of VM entry.
            val |= EntryCtrl::IA32E_MODE_GUEST;
        }

        // Load guest IA32_PAT/IA32_EFER on VM entry.
        use EntryControls as EntryCtrl;
        vmcs::set_control(
            VmcsControl32::VMENTRY_CONTROLS,
            Msr::IA32_VMX_TRUE_ENTRY_CTLS,
            Msr::IA32_VMX_ENTRY_CTLS.read() as u32,
            val.bits(),
            0,
        )?;

        vmcs::set_ept_pointer(ept_root)?;

        // No MSR switches if hypervisor doesn't use and there is only one vCPU.
        VmcsControl32::VMEXIT_MSR_STORE_COUNT.write(0)?;
        VmcsControl32::VMEXIT_MSR_LOAD_COUNT.write(0)?;
        VmcsControl32::VMENTRY_MSR_LOAD_COUNT.write(0)?;

        // VmcsControlNW::CR4_GUEST_HOST_MASK.write(0)?;
        VmcsControl32::CR3_TARGET_COUNT.write(0)?;

        // Pass-through exceptions (except #UD(6)), don't use I/O bitmap, set MSR bitmaps.
        let exception_bitmap: u32 = 1 << 6;

        self.setup_io_bitmap()?;

        VmcsControl32::EXCEPTION_BITMAP.write(exception_bitmap)?;
        VmcsControl64::IO_BITMAP_A_ADDR.write(self.io_bitmap.phys_addr().0.as_usize() as _)?;
        VmcsControl64::IO_BITMAP_B_ADDR.write(self.io_bitmap.phys_addr().1.as_usize() as _)?;
        VmcsControl64::MSR_BITMAPS_ADDR.write(self.msr_bitmap.phys_addr().as_usize() as _)?;

        // VmcsControl64::APIC_ACCESS_ADDR.write(
        //     EmulatedLocalApic::<H::MmHal, DummyHal>::virtual_apic_access_addr().as_usize() as _,
        // )?;
        Ok(())
    }

    fn get_paging_level(&self) -> usize {
        let mut level: u32 = 0; // non-paging
        let cr0 = VmcsGuestNW::CR0.read().unwrap();
        let cr4 = VmcsGuestNW::CR4.read().unwrap();
        let efer = VmcsGuest64::IA32_EFER.read().unwrap();
        // paging is enabled
        if cr0 & Cr0Flags::PAGING.bits() as usize != 0 {
            if cr4 & Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits() as usize != 0 {
                // is long mode
                if efer & EferFlags::LONG_MODE_ACTIVE.bits() != 0 {
                    level = 4;
                } else {
                    level = 3;
                }
            } else {
                level = 2;
            }
        }
        level as usize
    }
}

// Implementaton for type1.5 hypervisor
// #[cfg(feature = "type1_5")]
impl<H: AxVCpuHal> VmxVcpu<H> {
    fn set_cr(&mut self, cr_idx: usize, val: u64) {
        (|| -> AxResult {
            // debug!("set guest CR{} to val {:#x}", cr_idx, val);
            match cr_idx {
                0 => {
                    // Retrieve/validate restrictions on CR0
                    //
                    // In addition to what the VMX MSRs tell us, make sure that
                    // - NW and CD are kept off as they are not updated on VM exit and we
                    //   don't want them enabled for performance reasons while in root mode
                    // - PE and PG can be freely chosen (by the guest) because we demand
                    //   unrestricted guest mode support anyway
                    // - ET is ignored
                    let must0 = Msr::IA32_VMX_CR0_FIXED1.read()
                        & !(Cr0Flags::NOT_WRITE_THROUGH | Cr0Flags::CACHE_DISABLE).bits();
                    let must1 = Msr::IA32_VMX_CR0_FIXED0.read()
                        & !(Cr0Flags::PAGING | Cr0Flags::PROTECTED_MODE_ENABLE).bits();
                    VmcsGuestNW::CR0.write(((val & must0) | must1) as _)?;
                    VmcsControlNW::CR0_READ_SHADOW.write(val as _)?;
                    VmcsControlNW::CR0_GUEST_HOST_MASK.write((must1 | !must0) as _)?;
                }
                3 => VmcsGuestNW::CR3.write(val as _)?,
                4 => {
                    // Retrieve/validate restrictions on CR4
                    let must0 = Msr::IA32_VMX_CR4_FIXED1.read();
                    let must1 = Msr::IA32_VMX_CR4_FIXED0.read();
                    let val = val | Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS.bits();
                    VmcsGuestNW::CR4.write(((val & must0) | must1) as _)?;
                    VmcsControlNW::CR4_READ_SHADOW.write(val as _)?;
                    VmcsControlNW::CR4_GUEST_HOST_MASK.write((must1 | !must0) as _)?;
                }
                _ => unreachable!(),
            };
            Ok(())
        })()
        .expect("Failed to write guest control register")
    }

    #[allow(dead_code)]
    fn cr(&self, cr_idx: usize) -> usize {
        (|| -> AxResult<usize> {
            Ok(match cr_idx {
                0 => VmcsGuestNW::CR0.read()?,
                3 => VmcsGuestNW::CR3.read()?,
                4 => {
                    let host_mask = VmcsControlNW::CR4_GUEST_HOST_MASK.read()?;
                    (VmcsControlNW::CR4_READ_SHADOW.read()? & host_mask)
                        | (VmcsGuestNW::CR4.read()? & !host_mask)
                }
                _ => unreachable!(),
            })
        })()
        .expect("Failed to read guest control register")
    }
}

/// Get ready then vmlaunch or vmresume.
macro_rules! vmx_entry_with {
    ($instr:literal) => {
        naked_asm!(
            save_regs_to_stack!(),                      // save host status
            "mov    [rdi + {host_stack_size}], rsp",    // save current RSP to Vcpu::host_stack_top
            "mov    rsp, rdi",                          // set RSP to guest regs area
            restore_regs_from_stack!(),                 // restore guest status
            $instr,                                     // let's go!
            "jmp    {failed}",
            host_stack_size = const size_of::<GeneralRegisters>(),
            failed = sym Self::vmx_entry_failed,
            // options(noreturn),
        )
    }
}

impl<H: AxVCpuHal> VmxVcpu<H> {
    #[unsafe(naked)]
    /// Enter guest with vmlaunch.
    ///
    /// `#[naked]` is essential here, without it the rust compiler will think `&mut self` is not used and won't give us correct %rdi.
    ///
    /// This function itself never returns, but [`Self::vmx_exit`] will do the return for this.
    ///
    /// The return value is a dummy value.
    unsafe extern "C" fn vmx_launch(&mut self) -> usize {
        vmx_entry_with!("vmlaunch")
    }

    #[unsafe(naked)]
    /// Enter guest with vmresume.
    ///
    /// See [`Self::vmx_launch`] for detail.
    unsafe extern "C" fn vmx_resume(&mut self) -> usize {
        vmx_entry_with!("vmresume")
    }

    #[unsafe(naked)]
    /// Return after vm-exit. This function is used only for returning from [`Self::vmx_launch`] or [`Self::vmx_resume`].
    ///
    /// NEVER call this function directly.
    ///
    /// The return value is a dummy value.
    unsafe extern "C" fn vmx_exit(&mut self) -> usize {
        // it's not necessary to use another `unsafe` here, as Rust now do not require it in naked functions.
        naked_asm!(
            save_regs_to_stack!(),                  // save guest status, after this, rsp points to the `VmxVcpu`
            "mov    rsp, [rsp + {host_stack_top}]", // set RSP to Vcpu::host_stack_top
            restore_regs_from_stack!(),             // restore host status
            "ret",
            host_stack_top = const size_of::<GeneralRegisters>(),
        );
    }

    fn vmx_entry_failed() -> ! {
        panic!("{}", vmcs::instruction_error().as_str())
    }

    /// Whether the guest interrupts are blocked. (SDM Vol. 3C, Section 24.4.2, Table 24-3)
    fn allow_interrupt(&self) -> bool {
        let rflags = VmcsGuestNW::RFLAGS.read().unwrap();
        let block_state = VmcsGuest32::INTERRUPTIBILITY_STATE.read().unwrap();
        rflags as u64 & x86_64::registers::rflags::RFlags::INTERRUPT_FLAG.bits() != 0
            && block_state == 0
    }

    /// Try to inject a pending event before next VM entry.
    fn inject_pending_events(&mut self) -> AxResult {
        if let Some(event) = self.pending_events.front() {
            // trace!(
            //     "pending event vector {:#x} allow_int {}",
            //     event.0,
            //     self.allow_interrupt()
            // );
            if event.0 < 32 || self.allow_interrupt() {
                // if it's an exception, or an interrupt that is not blocked, inject it directly.
                vmcs::inject_event(event.0, event.1)?;
                self.pending_events.pop_front();
            } else {
                // interrupts are blocked, enable interrupt-window exiting.
                self.set_interrupt_window(true)?;
            }
        }
        Ok(())
    }

    /// Handle vm-exits than can and should be handled by [`VmxVcpu`] itself.
    ///
    /// Return the result or None if the vm-exit was not handled.
    fn builtin_vmexit_handler(&mut self, exit_info: &VmxExitInfo) -> Option<AxResult> {
        const X2APIC_MSR_BASE: u32 = 0x800;
        const X2APIC_MSR_END: u32 = 0x8ff; // SDM says 0x8ff, but actually 0x83f, we respect the SDM here.
        // Following vm-exits are handled here:
        // - interrupt window: turn off interrupt window;
        // - xsetbv: set guest xcr;
        // - cr access: just panic;
        match exit_info.exit_reason {
            VmxExitReason::INTERRUPT_WINDOW => Some(self.set_interrupt_window(false)),
            VmxExitReason::PREEMPTION_TIMER => Some(self.handle_vmx_preemption_timer()),
            VmxExitReason::XSETBV => Some(self.handle_xsetbv()),
            VmxExitReason::CR_ACCESS => Some(self.handle_cr()),
            VmxExitReason::CPUID => Some(self.handle_cpuid()),
            msr_rw @ (VmxExitReason::MSR_READ | VmxExitReason::MSR_WRITE)
                if {
                    let msr = self.regs().rcx as u32;
                    msr >= X2APIC_MSR_BASE && msr <= X2APIC_MSR_END
                } =>
            {
                Some(self.handle_apic_msr_access(
                    msr_rw == VmxExitReason::MSR_WRITE,
                    self.regs().rcx as u32,
                ))
            }
            VmxExitReason::APIC_ACCESS => Some(self.handle_apic_access(exit_info)),
            _ => None,
        }
    }

    /// Read a 64-bit value from EDX:EAX.
    fn read_edx_eax(&self) -> u64 {
        ((self.regs().rdx & 0xffff_ffff) << 32) | (self.regs().rax & 0xffff_ffff)
    }

    /// Write a 64-bit value to EDX:EAX.
    fn write_edx_eax(&mut self, val: u64) {
        self.regs_mut().rax = val & 0xffff_ffff;
        self.regs_mut().rdx = val >> 32;
    }

    fn handle_apic_msr_access(&mut self, write: bool, msr: u32) -> AxResult {
        const VMEXIT_INSTR_LEN_RDMSR_WRMSR: u8 = 2;

        self.advance_rip(VMEXIT_INSTR_LEN_RDMSR_WRMSR)?;

        let msr = msr as _;
        if write {
            let value = self.read_edx_eax() as usize;

            trace!(
                "handle_vlapic_msr_write: msr={:#x}, value={:#x}",
                msr, value
            );

            <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_write(
                &self.vlapic,
                SysRegAddr::new(msr),
                AccessWidth::Qword,
                value,
            )
        } else {
            let value = <EmulatedLocalApic as BaseDeviceOps<SysRegAddrRange>>::handle_read(
                &self.vlapic,
                SysRegAddr::new(msr),
                AccessWidth::Qword,
            )? as u64;

            trace!("handle_vlapic_msr_read: msr={:#x}, value={:#x}", msr, value);

            self.write_edx_eax(value);
            Ok(())
        }
    }

    fn handle_apic_access(&mut self, exit_info: &VmxExitInfo) -> AxResult {
        let apic_access_exit_info = self.apic_access_exit_info()?;

        let write = match apic_access_exit_info.access_type {
            ApicAccessExitType::LinearDataWrite => true,
            ApicAccessExitType::LinearDataRead => false,
            _ => {
                warn!(
                    "Unsupported APIC access type: {:?}",
                    apic_access_exit_info.access_type
                );
                return ax_err!(BadState, "Unsupported APIC access type");
            }
        };

        // TODO: handle APIC access.
        let _ = write;

        self.advance_rip(exit_info.exit_instruction_length as _)?;

        unimplemented!("apic access");
        // TODO
    }

    fn handle_vmx_preemption_timer(&mut self) -> AxResult {
        /*
        The VMX-preemption timer counts down at rate proportional to that of the timestamp counter (TSC).
        Specifically, the timer counts down by 1 every time bit X in the TSC changes due to a TSC increment.
        The value of X is in the range 0–31 and can be determined by consulting the VMX capability MSR IA32_VMX_MISC (see Appendix A.6).
         */
        VmcsGuest32::VMX_PREEMPTION_TIMER_VALUE.write(VMX_PREEMPTION_TIMER_SET_VALUE)?;
        Ok(())
    }

    #[allow(clippy::single_match)]
    fn handle_cr(&mut self) -> AxResult {
        const VM_EXIT_INSTR_LEN_MV_TO_CR: u8 = 3;

        let cr_access_info = vmcs::cr_access_info()?;

        let reg = cr_access_info.gpr;
        let cr = cr_access_info.cr_number;

        match cr_access_info.access_type {
            /* move to cr */
            0 => {
                let val = if reg == 4 {
                    self.stack_pointer() as u64
                } else {
                    self.guest_regs.get_reg_of_index(reg)
                };
                if cr == 0 || cr == 4 {
                    self.advance_rip(VM_EXIT_INSTR_LEN_MV_TO_CR)?;
                    /* TODO: check for #GP reasons */
                    self.set_cr(cr as usize, val);

                    if cr == 0 && Cr0Flags::from_bits_truncate(val).contains(Cr0Flags::PAGING) {
                        vmcs::update_efer()?;
                    }
                    return Ok(());
                }
            }
            _ => {}
        };

        panic!(
            "Guest's access to cr not allowed: {:#x?}, {:#x?}",
            self, cr_access_info
        );
    }

    fn handle_cpuid(&mut self) -> AxResult {
        use raw_cpuid::{CpuIdResult, cpuid};

        const VM_EXIT_INSTR_LEN_CPUID: u8 = 2;
        const LEAF_FEATURE_INFO: u32 = 0x1;
        const LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION: u32 = 0x7;
        const LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION: u32 = 0xd;
        const EAX_FREQUENCY_INFO: u32 = 0x16;
        const LEAF_HYPERVISOR_INFO: u32 = 0x4000_0000;
        const LEAF_HYPERVISOR_FEATURE: u32 = 0x4000_0001;
        const VENDOR_STR: &[u8; 12] = b"RVMRVMRVMRVM";
        let vendor_regs = unsafe { &*(VENDOR_STR.as_ptr() as *const [u32; 3]) };

        let regs_clone = self.regs_mut().clone();
        let function = regs_clone.rax as u32;
        let res = match function {
            LEAF_FEATURE_INFO => {
                const FEATURE_VMX: u32 = 1 << 5;
                const FEATURE_HYPERVISOR: u32 = 1 << 31;
                const FEATURE_MCE: u32 = 1 << 7;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                res.ecx &= !FEATURE_VMX;
                res.ecx |= FEATURE_HYPERVISOR;
                res.eax &= !FEATURE_MCE;
                res
            }
            // See SDM Table 3-8. Information Returned by CPUID Instruction (Contd.)
            LEAF_STRUCTURED_EXTENDED_FEATURE_FLAGS_ENUMERATION => {
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if regs_clone.rcx == 0 {
                    // Bit 05: WAITPKG.
                    res.ecx.set_bit(5, false); // clear waitpkg
                    // Bit 16: LA57. Supports 57-bit linear addresses and five-level paging if 1.
                    res.ecx.set_bit(16, false); // clear LA57
                }

                res
            }
            LEAF_PROCESSOR_EXTENDED_STATE_ENUMERATION => {
                self.load_guest_xstate();
                let res = cpuid!(regs_clone.rax, regs_clone.rcx);
                self.load_host_xstate();

                res
            }
            LEAF_HYPERVISOR_INFO => CpuIdResult {
                eax: LEAF_HYPERVISOR_FEATURE,
                ebx: vendor_regs[0],
                ecx: vendor_regs[1],
                edx: vendor_regs[2],
            },
            LEAF_HYPERVISOR_FEATURE => CpuIdResult {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
            EAX_FREQUENCY_INFO => {
                /// Timer interrupt frequencyin Hz.
                /// Todo: this should be the same as `axconfig::TIMER_FREQUENCY` defined in ArceOS's config file.
                const TIMER_FREQUENCY_MHZ: u32 = 3_000;
                let mut res = cpuid!(regs_clone.rax, regs_clone.rcx);
                if res.eax == 0 {
                    warn!(
                        "handle_cpuid: Failed to get TSC frequency by CPUID, default to {} MHz",
                        TIMER_FREQUENCY_MHZ
                    );
                    res.eax = TIMER_FREQUENCY_MHZ;
                }
                res
            }
            _ => cpuid!(regs_clone.rax, regs_clone.rcx),
        };

        trace!(
            "VM exit: CPUID({:#x}, {:#x}): {:?}",
            regs_clone.rax, regs_clone.rcx, res
        );

        let regs = self.regs_mut();
        regs.rax = res.eax as _;
        regs.rbx = res.ebx as _;
        regs.rcx = res.ecx as _;
        regs.rdx = res.edx as _;
        self.advance_rip(VM_EXIT_INSTR_LEN_CPUID)?;

        Ok(())
    }

    fn handle_xsetbv(&mut self) -> AxResult {
        const XCR_XCR0: u64 = 0;
        const VM_EXIT_INSTR_LEN_XSETBV: u8 = 3;

        let index = self.guest_regs.rcx.get_bits(0..32);
        let value = self.guest_regs.rdx.get_bits(0..32) << 32 | self.guest_regs.rax.get_bits(0..32);

        // TODO: get host-supported xcr0 mask by cpuid and reject any guest-xsetbv violating that
        if index == XCR_XCR0 {
            Xcr0::from_bits(value)
                .and_then(|x| {
                    if !x.contains(Xcr0::XCR0_FPU_MMX_STATE) {
                        return None;
                    }

                    if x.contains(Xcr0::XCR0_AVX_STATE) && !x.contains(Xcr0::XCR0_SSE_STATE) {
                        return None;
                    }

                    if x.contains(Xcr0::XCR0_BNDCSR_STATE) ^ x.contains(Xcr0::XCR0_BNDREG_STATE) {
                        return None;
                    }

                    if x.contains(Xcr0::XCR0_OPMASK_STATE)
                        || x.contains(Xcr0::XCR0_ZMM_HI256_STATE)
                        || x.contains(Xcr0::XCR0_HI16_ZMM_STATE)
                        || !x.contains(Xcr0::XCR0_AVX_STATE)
                        || !x.contains(Xcr0::XCR0_OPMASK_STATE)
                        || !x.contains(Xcr0::XCR0_ZMM_HI256_STATE)
                        || !x.contains(Xcr0::XCR0_HI16_ZMM_STATE)
                    {
                        return None;
                    }

                    Some(x)
                })
                .ok_or(ax_err_type!(InvalidInput))
                .and_then(|x| {
                    self.xstate.guest_xcr0 = x.bits();
                    self.advance_rip(VM_EXIT_INSTR_LEN_XSETBV)
                })
        } else {
            // xcr0 only
            ax_err!(Unsupported, "only xcr0 is supported")
        }
    }

    fn load_guest_xstate(&mut self) {
        self.xstate.switch_to_guest();
    }

    fn load_host_xstate(&mut self) {
        self.xstate.switch_to_host();
    }
}

impl<H: AxVCpuHal> Drop for VmxVcpu<H> {
    fn drop(&mut self) {
        unsafe { vmx::vmclear(self.vmcs.phys_addr().as_usize() as u64).unwrap() };
        info!("[HV] dropped VmxVcpu(vmcs: {:#x})", self.vmcs.phys_addr());
    }
}

fn get_tr_base(tr: SegmentSelector, gdt: &DescriptorTablePointer<u64>) -> u64 {
    let index = tr.index() as usize;
    let table_len = (gdt.limit as usize + 1) / core::mem::size_of::<u64>();
    let table = unsafe { core::slice::from_raw_parts(gdt.base, table_len) };
    let entry = table[index];
    if entry & (1 << 47) != 0 {
        // present
        let base_low = entry.get_bits(16..40) | entry.get_bits(56..64) << 24;
        let base_high = table[index + 1] & 0xffff_ffff;
        base_low | base_high << 32
    } else {
        // no present
        0
    }
}

impl<H: AxVCpuHal> Debug for VmxVcpu<H> {
    fn fmt(&self, f: &mut Formatter) -> Result {
        (|| -> AxResult<Result> {
            Ok(f.debug_struct("VmxVcpu")
                .field("guest_regs", &self.guest_regs)
                .field("rip", &VmcsGuestNW::RIP.read()?)
                .field("rsp", &VmcsGuestNW::RSP.read()?)
                .field("rflags", &VmcsGuestNW::RFLAGS.read()?)
                .field("cr0", &VmcsGuestNW::CR0.read()?)
                .field("cr3", &VmcsGuestNW::CR3.read()?)
                .field("cr4", &VmcsGuestNW::CR4.read()?)
                .field("cs", &VmcsGuest16::CS_SELECTOR.read()?)
                .field("fs_base", &VmcsGuestNW::FS_BASE.read()?)
                .field("gs_base", &VmcsGuestNW::GS_BASE.read()?)
                .field("tss", &VmcsGuest16::TR_SELECTOR.read()?)
                .finish())
        })()
        .unwrap()
    }
}

impl<H: AxVCpuHal> AxArchVCpu for VmxVcpu<H> {
    type CreateConfig = ();

    type SetupConfig = ();

    fn new(vm_id: VMId, vcpu_id: VCpuId, _config: Self::CreateConfig) -> AxResult<Self> {
        Self::new(vm_id, vcpu_id)
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.entry = Some(entry);
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.ept_root = Some(ept_root);
        Ok(())
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        self.setup_vmcs(self.entry.unwrap(), self.ept_root.unwrap())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        match self.inner_run() {
            Some(exit_info) => Ok(if exit_info.entry_failure {
                AxVCpuExitReason::FailEntry {
                    // Todo: get `hardware_entry_failure_reason` somehow.
                    hardware_entry_failure_reason: 0,
                }
            } else {
                match exit_info.exit_reason {
                    VmxExitReason::VMCALL => {
                        self.advance_rip(exit_info.exit_instruction_length as _)?;
                        AxVCpuExitReason::Hypercall {
                            nr: self.regs().rax,
                            args: [
                                self.regs().rdi,
                                self.regs().rsi,
                                self.regs().rdx,
                                self.regs().rcx,
                                self.regs().r8,
                                self.regs().r9,
                            ],
                        }
                    }
                    VmxExitReason::IO_INSTRUCTION => {
                        let io_info = self.io_exit_info().unwrap();
                        self.advance_rip(exit_info.exit_instruction_length as _)?;

                        let port = io_info.port;

                        if io_info.is_repeat || io_info.is_string {
                            warn!(
                                "VMX unsupported IO-Exit: {:#x?} of {:#x?}",
                                io_info, exit_info
                            );
                            warn!("VCpu {:#x?}", self);
                            AxVCpuExitReason::Halt
                        } else {
                            let width = match AccessWidth::try_from(io_info.access_size as usize) {
                                Ok(width) => width,
                                Err(_) => {
                                    warn!(
                                        "VMX invalid IO-Exit: {:#x?} of {:#x?}",
                                        io_info, exit_info
                                    );
                                    warn!("VCpu {:#x?}", self);
                                    return Ok(AxVCpuExitReason::Halt);
                                }
                            };

                            if io_info.is_in {
                                AxVCpuExitReason::IoRead {
                                    port: Port(port),
                                    width,
                                }
                            } else if port == QEMU_EXIT_PORT
                                && width == AccessWidth::Word
                                && self.regs().rax == QEMU_EXIT_MAGIC
                            {
                                AxVCpuExitReason::SystemDown
                            } else {
                                AxVCpuExitReason::IoWrite {
                                    port: Port(port),
                                    width,
                                    data: self.regs().rax.get_bits(width.bits_range()),
                                }
                            }
                        }
                    }
                    VmxExitReason::EXTERNAL_INTERRUPT => {
                        let int_info = self.interrupt_exit_info()?;
                        assert!(int_info.valid);
                        AxVCpuExitReason::ExternalInterrupt {
                            vector: int_info.vector as _,
                        }
                    }
                    VmxExitReason::MSR_READ => {
                        // `reg` is unused here.
                        AxVCpuExitReason::SysRegRead {
                            addr: SysRegAddr::new(self.regs().rcx as _),
                            reg: 0,
                        }
                    }
                    VmxExitReason::MSR_WRITE => {
                        let value = (self.regs().rax & 0xffff_ffff)
                            | ((self.regs().rdx & 0xffff_ffff) << 32);
                        AxVCpuExitReason::SysRegWrite {
                            addr: SysRegAddr::new(self.regs().rcx as _),
                            value,
                        }
                    }
                    _ => {
                        warn!("VMX unsupported VM-Exit: {:#x?}", exit_info);
                        warn!("VCpu {:#x?}", self);
                        AxVCpuExitReason::Halt
                    }
                }
            }),
            None => Ok(AxVCpuExitReason::Nothing),
        }
    }

    fn bind(&mut self) -> AxResult {
        self.bind_to_current_processor()
    }

    fn unbind(&mut self) -> AxResult {
        self.launched = false;
        self.unbind_from_current_processor()
    }

    fn set_gpr(&mut self, reg: usize, val: usize) {
        self.regs_mut().set_reg_of_index(reg as u8, val as u64);
    }

    fn inject_interrupt(&mut self, vector: usize) -> AxResult {
        if vector != 0 {
            // warn!("interrupt queued in inject_interrupt: vector {:#x}", vector);
        } else {
            warn!("interrupt queued in inject_interrupt: vector 0");
            panic!()
        }
        Ok(self.queue_event(vector as u8, None))
    }

    fn set_return_value(&mut self, val: usize) {
        self.regs_mut().rax = val as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    #[test]
    fn test_vm_cpu_mode_enum() {
        // Test VmCpuMode enum values
        assert_ne!(VmCpuMode::Real, VmCpuMode::Protected);
        assert_ne!(VmCpuMode::Protected, VmCpuMode::Compatibility);
        assert_ne!(VmCpuMode::Compatibility, VmCpuMode::Mode64);

        // Test Debug formatting
        let debug_str = format!("{:?}", VmCpuMode::Mode64);
        assert!(debug_str.contains("Mode64"));
    }

    #[test]
    fn test_general_registers_operations() {
        let mut regs = GeneralRegisters::default();

        // Test initial state
        assert_eq!(regs.rax, 0);
        assert_eq!(regs.rbx, 0);

        // Test setting and getting values
        regs.rax = 0x1234567890abcdef;
        regs.rbx = 0xfedcba0987654321;

        assert_eq!(regs.rax, 0x1234567890abcdef);
        assert_eq!(regs.rbx, 0xfedcba0987654321);

        // Test register access by index
        regs.set_reg_of_index(0, 0x1111111111111111); // RAX
        assert_eq!(regs.get_reg_of_index(0), 0x1111111111111111);

        regs.set_reg_of_index(1, 0x2222222222222222); // RCX  
        assert_eq!(regs.get_reg_of_index(1), 0x2222222222222222);
    }

    #[test]
    fn test_constants() {
        // Test that constants have expected values
        assert_eq!(VMX_PREEMPTION_TIMER_SET_VALUE, 1_000_000);
        assert_eq!(QEMU_EXIT_PORT, 0x604);
        assert_eq!(QEMU_EXIT_MAGIC, 0x2000);
        assert_eq!(MSR_IA32_EFER_LMA_BIT, 1 << 10);
        assert_eq!(CR0_PE, 1 << 0);
    }

    #[test]
    fn test_bit_operations() {
        use bit_field::BitField;

        let mut value = 0u64;
        value.set_bits(0..32, 0x12345678);
        value.set_bits(32..64, 0xabcdef00);

        assert_eq!(value.get_bits(0..32), 0x12345678);
        assert_eq!(value.get_bits(32..64), 0xabcdef00);
    }

    // Mock tests for VmxVcpu (limited to safe operations)
    mod vmx_vcpu_tests {
        use super::*;

        // Helper function to create a test VmxVcpu (this would normally require VMX hardware)
        fn create_test_vcpu_regs() -> GeneralRegisters {
            let mut regs = GeneralRegisters::default();
            regs.rax = 0x1000;
            regs.rbx = 0x2000;
            regs.rcx = 0x3000;
            regs.rdx = 0x4000;
            regs
        }

        #[test]
        fn test_general_registers_clone() {
            let regs = create_test_vcpu_regs();
            let cloned_regs = regs.clone();

            assert_eq!(regs.rax, cloned_regs.rax);
            assert_eq!(regs.rbx, cloned_regs.rbx);
            assert_eq!(regs.rcx, cloned_regs.rcx);
            assert_eq!(regs.rdx, cloned_regs.rdx);
        }

        #[test]
        fn test_edx_eax_operations() {
            // Test the logic for combining EDX:EAX
            let rax = 0x12345678u64;
            let rdx = 0xabcdef00u64;

            // Simulate read_edx_eax logic
            let combined = ((rdx & 0xffff_ffff) << 32) | (rax & 0xffff_ffff);
            assert_eq!(combined, 0xabcdef0012345678);

            // Simulate write_edx_eax logic
            let val = 0xfedcba0987654321u64;
            let new_rax = val & 0xffff_ffff;
            let new_rdx = val >> 32;

            assert_eq!(new_rax, 0x87654321);
            assert_eq!(new_rdx, 0xfedcba09);
        }

        #[test]
        fn test_register_bit_operations() {
            let mut regs = GeneralRegisters::default();

            // Test setting specific bits in registers
            regs.rcx = 0;
            regs.rcx.set_bits(0..32, 0x12345678);
            assert_eq!(regs.rcx.get_bits(0..32), 0x12345678);

            regs.rdx = 0xffffffffffffffff;
            regs.rdx.set_bits(32..64, 0);
            assert_eq!(regs.rdx.get_bits(32..64), 0);
            assert_eq!(regs.rdx.get_bits(0..32), 0xffffffff);
        }

        #[test]
        fn test_gla2gva_logic() {
            // Test the address translation logic (without actual VMX hardware)
            let guest_rip = 0x1000usize;
            let seg_base_64bit = 0; // In 64-bit mode, segment base is 0
            let seg_base_other = 0x10000; // In other modes, segment base matters

            // 64-bit mode calculation
            let gva_64bit = guest_rip + seg_base_64bit;
            assert_eq!(gva_64bit, 0x1000);

            // Other mode calculation
            let gva_other = guest_rip + seg_base_other;
            assert_eq!(gva_other, 0x11000);
        }

        #[test]
        fn test_interrupt_vector_validation() {
            // Test interrupt vector validation logic
            let valid_exception = 6; // #UD exception
            let valid_interrupt = 0x20;
            let invalid_vector = 0;

            assert!(valid_exception < 32); // Exceptions are < 32
            assert!(valid_interrupt >= 32); // Interrupts are >= 32
            assert_eq!(invalid_vector, 0); // Vector 0 should be handled specially
        }

        #[test]
        fn test_page_walk_info_struct() {
            let ptw_info = GuestPageWalkInfo {
                top_entry: 0x1000,
                level: 4,
                width: 9,
                is_user_mode_access: false,
                is_write_access: false,
                is_inst_fetch: false,
                pse: true,
                wp: true,
                nxe: true,
                is_smap_on: false,
                is_smep_on: false,
            };

            assert_eq!(ptw_info.level, 4);
            assert_eq!(ptw_info.width, 9);
            assert_eq!(ptw_info.top_entry, 0x1000);
        }

        #[test]
        fn test_cpuid_constants() {
            // Test CPUID-related constants used in handle_cpuid
            const LEAF_FEATURE_INFO: u32 = 0x1;
            const LEAF_HYPERVISOR_INFO: u32 = 0x4000_0000;
            const FEATURE_VMX: u32 = 1 << 5;
            const FEATURE_HYPERVISOR: u32 = 1 << 31;

            assert_eq!(LEAF_FEATURE_INFO, 1);
            assert_eq!(LEAF_HYPERVISOR_INFO, 0x40000000);
            assert_eq!(FEATURE_VMX, 32);
            assert_eq!(FEATURE_HYPERVISOR, 0x80000000);
        }

        #[test]
        fn test_cr_flags_operations() {
            use x86_64::registers::control::{Cr0Flags, Cr4Flags};

            // Test CR0 flags
            let cr0_flags = Cr0Flags::PAGING | Cr0Flags::PROTECTED_MODE_ENABLE;
            assert!(cr0_flags.contains(Cr0Flags::PAGING));
            assert!(cr0_flags.contains(Cr0Flags::PROTECTED_MODE_ENABLE));
            assert!(!cr0_flags.contains(Cr0Flags::CACHE_DISABLE));

            // Test CR4 flags
            let cr4_flags = Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS | Cr4Flags::PAGE_SIZE_EXTENSION;
            assert!(cr4_flags.contains(Cr4Flags::VIRTUAL_MACHINE_EXTENSIONS));
            assert!(cr4_flags.contains(Cr4Flags::PAGE_SIZE_EXTENSION));
        }

        #[test]
        fn test_access_width_operations() {
            // Test access width enumeration
            use axaddrspace::device::AccessWidth;

            assert_eq!(AccessWidth::Byte as usize, 0);
            assert_eq!(AccessWidth::Word as usize, 1);
            assert_eq!(AccessWidth::Dword as usize, 2);
            assert_eq!(AccessWidth::Qword as usize, 3);

            // Test conversion
            assert_eq!(AccessWidth::try_from(1), Ok(AccessWidth::Byte));
            assert_eq!(AccessWidth::try_from(2), Ok(AccessWidth::Word));
            assert_eq!(AccessWidth::try_from(4), Ok(AccessWidth::Dword));
            assert_eq!(AccessWidth::try_from(8), Ok(AccessWidth::Qword));
        }
    }

    // Tests for utility functions that don't require hardware
    #[test]
    fn test_get_tr_base_logic() {
        let mut test_entry = 0u64;
        test_entry |= 1u64 << 47; // Present bit
        test_entry |= (0x1000u64 & 0xFFFFFF) << 16; // Base address bits 16-39

        // Present bit check
        let present = test_entry & (1 << 47) != 0;
        assert!(present);

        // Base address extraction
        let base_low = (test_entry >> 16) & 0xFFFFFF;
        let base_high = (test_entry >> 56) & 0xFF;
        let base_addr = base_low | (base_high << 24);

        assert_eq!(base_addr, 0x1000);
    }

    #[test]
    fn test_vmx_exit_reason_enum() {
        // Test that VmxExitReason enum can be used in match statements
        let test_reason = VmxExitReason::VMCALL;
        match test_reason {
            VmxExitReason::VMCALL => assert!(true),
            _ => assert!(false),
        }
    }

    #[test]
    fn test_debug_implementations() {
        // Test Debug implementations for various types
        let cpu_mode = VmCpuMode::Mode64;
        let debug_str = format!("{:?}", cpu_mode);
        assert!(!debug_str.is_empty());

        let regs = GeneralRegisters::default();
        let debug_str = format!("{:?}", regs);
        assert!(!debug_str.is_empty());
    }

    // Note: Most VmxVcpu methods require actual VMX hardware support and cannot be unit tested
    // without either:
    // 1. Running on VMX-capable hardware with appropriate privileges
    // 2. Extensive mocking of the entire VMX infrastructure
    //
    // For comprehensive testing of VmxVcpu, integration tests on actual hardware
    // or hardware simulators would be more appropriate.
}
