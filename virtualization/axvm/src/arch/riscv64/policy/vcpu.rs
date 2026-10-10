// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
use core::marker::PhantomData;

use ax_cpu::{
    registers::GprIndex,
    virtualization::{
        Exception, GuestBinding, GuestInterrupt, Vcpu, VirtualizationError, enter_guest,
    },
};
use riscv_decode::{
    Instruction,
    types::{IType, SType},
};
use rustsbi::{Forward, RustSBI, SbiRet};
use sbi_spec::{hsm, legacy, rfnc, spi, srst};

use crate::arch::riscv64::policy::{
    EID_HVC, RiscvVcpuCreateConfig,
    consts::traps::irq::{S_SOFT, S_TIMER, is_supervisor_external},
    guest_mem,
    host::RiscvHostOps,
    sbi_console::*,
    types::{
        RiscvAccessFlags, RiscvAccessWidth, RiscvGuestPhysAddr, RiscvGuestVirtAddr, RiscvIpiAbi,
        RiscvIpiCompletion, RiscvIpiRequest, RiscvNestedPagingConfig, RiscvSbiCall, RiscvVcpuError,
        RiscvVcpuResult, RiscvVmExit,
    },
    vpmu::VirtualPmu,
};

const TINST_PSEUDO_STORE: u32 = 0x3020;
const TINST_PSEUDO_LOAD: u32 = 0x3000;
const EID_TIME: usize = 0x5449_4D45;
const FID_SET_TIMER: usize = 0;
#[cfg(feature = "sstc")]
const SYSTEM_OPCODE: u32 = 0x73;
#[cfg(feature = "sstc")]
const CSR_STIMECMP: u16 = 0x14d;

#[inline]
fn instr_is_pseudo(ins: u32) -> bool {
    ins == TINST_PSEUDO_STORE || ins == TINST_PSEUDO_LOAD
}

/// A virtual CPU within a guest.
///
/// The value is confined to the architecture adapter; it is not part of any
/// crate-public API. Its block-local hardware binding is installed and retired
/// only through the crate-private [`Self::bind`]/[`Self::unbind`] pair.
pub(crate) struct RiscvVcpu<H: RiscvHostOps> {
    regs: Vcpu,
    sbi: RISCVVCpuSbi,
    binding: Option<GuestBinding>,
    hart_id: usize,
    _host: PhantomData<fn() -> H>,
}

// `GuestBinding` is intentionally `!Send`: it owns the saved host VS/HS CSR
// banks of one hart and must be released on that same hart. That capability is
// the only non-`Send` payload of this otherwise plain value.
//
// SAFETY: the type is crate-private and its binding API is crate-private, so no
// downstream crate can name it. Within this crate a `GuestBinding` is installed
// only by [`RiscvVcpu::bind`], whose sole caller is
// `AxVCpu::with_backend_bound_current_cpu` through `BackendBinding::bind`
// (`virtualization/axvm/src/vcpu.rs`). That scope holds the vCPU exclusively by
// `&mut`, pins the host CPU, keeps the binding loaded until
// `BackendBinding::finish`/`Drop` calls [`RiscvVcpu::unbind`] on the same hart,
// and aborts the process if that retirement fails. The unique `&mut` borrow
// means the value cannot be moved across the load/run/unload window, and
// [`RiscvVcpu::bind`] rejects a second load, so the field carries a binding only
// while the value is pinned. Moving the value between execution contexts
// (construction, `Default`, and after `unbind`) always happens with an empty
// binding. Every other field (`regs`, `sbi`, `hart_id`, and the `fn() -> H`
// phantom) is `Send` and owns no hart-local hardware capability. Making the
// whole crate trusted is why this `unsafe impl` is sound even though safe crate
// code could, in principle, invoke `VmArchVcpuOps::bind` through the
// `AxvmRiscvVcpu` wrapper; that is a private capability the engine never
// exposes, and the review in the migration notes records this as the single
// residual obligation on the engine.
unsafe impl<H: RiscvHostOps> Send for RiscvVcpu<H> {}

#[derive(RustSBI)]
struct RISCVVCpuSbi {
    #[rustsbi(pmu)]
    pmu: VirtualPmu,
    #[rustsbi(ipi)]
    ipi: crate::arch::riscv64::policy::sbi_ipi::VirtualSbiIpi,
    #[rustsbi(console, fence, reset, info, hsm, timer)]
    forward: Forward,
}

#[cfg(feature = "sstc")]
/// Result of reading an instruction for virtual-instruction emulation.
enum VirtualInstructionRead {
    Instruction(u32),
    Handled(RiscvVmExit),
}

/// Result of decoding the trapped guest load/store instruction.
enum InstructionDecode {
    Decoded(Instruction, usize),
    Handled(RiscvVmExit),
}

impl Default for RISCVVCpuSbi {
    #[inline]
    fn default() -> Self {
        Self {
            pmu: VirtualPmu::default(),
            ipi: crate::arch::riscv64::policy::sbi_ipi::VirtualSbiIpi,
            forward: Forward,
        }
    }
}

impl<H: RiscvHostOps> Default for RiscvVcpu<H> {
    fn default() -> Self {
        Self {
            regs: Vcpu::default(),
            sbi: RISCVVCpuSbi::default(),
            binding: None,
            hart_id: 0,
            _host: PhantomData,
        }
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    /// Creates a new RISC-V vCPU.
    pub fn new(
        _vm_id: usize,
        _vcpu_id: usize,
        config: RiscvVcpuCreateConfig,
    ) -> RiscvVcpuResult<Self> {
        let mut regs = Vcpu::default();
        // Setup the guest's general purpose registers.
        // `a0` is the hartid
        regs.guest_regs.gprs.set_reg(GprIndex::A0, config.hart_id);
        // `a1` is the address of the device tree blob.
        regs.guest_regs.gprs.set_reg(GprIndex::A1, config.dtb_addr);

        Ok(Self {
            regs,
            sbi: RISCVVCpuSbi::default(),
            binding: None,
            hart_id: config.hart_id,
            _host: PhantomData,
        })
    }

    /// Completes architecture-specific setup.
    pub fn setup(&mut self, _config: ()) -> RiscvVcpuResult {
        self.regs.initialize_supervisor();
        Ok(())
    }

    /// Sets the guest entry point.
    pub fn set_entry(&mut self, entry: RiscvGuestPhysAddr) -> RiscvVcpuResult {
        self.regs.guest_regs.sepc = entry.as_usize();
        Ok(())
    }

    /// Initializes a hart started through the SBI HSM extension.
    ///
    /// The caller owns the target backend. `a0` carries the guest-visible hart
    /// ID and `a1` carries the opaque guest context, matching the SBI calling
    /// convention used by HSM.
    pub fn initialize_cpu_on(
        &mut self,
        entry: RiscvGuestPhysAddr,
        context_id: usize,
    ) -> RiscvVcpuResult {
        self.set_entry(entry)?;
        self.set_gpr_from_gpr_index(GprIndex::A0, self.hart_id);
        self.set_gpr_from_gpr_index(GprIndex::A1, context_id);
        Ok(())
    }

    /// Sets the nested page table used by guest-stage translation.
    pub fn set_nested_page_table(&mut self, config: RiscvNestedPagingConfig) -> RiscvVcpuResult {
        let expected_mode = match config.levels {
            3 => 8,
            4 => 9,
            _ => {
                return Err(RiscvVcpuError::InvalidInput);
            }
        };
        if config.mode != expected_mode || config.root_paddr.as_usize() & 0x3fff != 0 {
            return Err(RiscvVcpuError::InvalidInput);
        }

        let mode = match config.levels {
            3 => ax_cpu::virtualization::GStageMode::Sv39x4,
            4 => ax_cpu::virtualization::GStageMode::Sv48x4,
            _ => return Err(RiscvVcpuError::InvalidInput),
        };
        self.regs
            .set_page_table(config.root_paddr.as_usize().into(), mode)
            .map_err(binding_error)?;
        Ok(())
    }

    /// Runs only the machine transaction; SBI and device interpretation follow later.
    pub(crate) fn run_machine(&mut self) -> RiscvVcpuResult<ax_cpu::virtualization::Exit> {
        if self.binding.is_none() {
            return Err(RiscvVcpuError::BadState);
        }
        let enabled = ax_cpu::interrupt::irqs_enabled();
        ax_cpu::interrupt::disable_irqs();
        // SAFETY: AxVM owns the pinned binding and all guest memory. The CPU
        // entry restores host state and captures the exit before returning.
        let exit = unsafe { enter_guest(&mut self.regs) };
        if enabled {
            ax_cpu::interrupt::enable_irqs();
        }
        Ok(exit)
    }

    /// Interprets the captured exit outside the final machine IRQ window.
    pub(crate) fn process_exit(
        &mut self,
        exit: ax_cpu::virtualization::Exit,
    ) -> RiscvVcpuResult<RiscvVmExit> {
        if self.binding.is_none() {
            return Err(RiscvVcpuError::BadState);
        }
        self.regs.trap_csrs = exit;
        self.vmexit_handler()
    }

    /// Binds the vCPU to the current physical CPU.
    ///
    /// Crate-private on purpose: the only caller is the CPU-pinned
    /// `BackendBinding` scope, which owns the value by `&mut` and retires the
    /// binding with [`Self::unbind`] before releasing the host CPU. No other
    /// module may install a live binding and then move the value.
    pub(crate) fn bind(&mut self) -> RiscvVcpuResult {
        if self.binding.is_some() {
            return Err(RiscvVcpuError::BadState);
        }
        // SAFETY: AxVM holds the current-CPU scope through the matching unload;
        // this vCPU's roots remain owned by the VM for that complete scope.
        self.binding = Some(unsafe { GuestBinding::load(&self.regs) }.map_err(binding_error)?);
        self.sbi.pmu.backend_bind();
        Ok(())
    }

    /// Saves the guest bank and restores the original host bank.
    pub(crate) fn unbind(&mut self) -> RiscvVcpuResult {
        let binding = self.binding.take().ok_or(RiscvVcpuError::BadState)?;
        self.sbi.pmu.backend_unbind();
        // SAFETY: the caller retains the same current-CPU scope and excludes
        // concurrent access to the register image being captured.
        unsafe {
            binding.unload(&mut self.regs);
        }
        Ok(())
    }

    /// Set one of the vCPU's general purpose registers.
    pub fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0 => {
                // Do nothing, x0 is hardwired to zero
            }
            1..=31 => {
                if let Some(gpr_index) = GprIndex::from_raw(index as u32) {
                    self.set_gpr_from_gpr_index(gpr_index, val);
                } else {
                    warn!("RISCVVCpu: Failed to map general purpose register index: {index}");
                }
            }
            _ => {
                warn!("RISCVVCpu: Unsupported general purpose register index: {index}");
            }
        }
    }

    /// Injects a virtual interrupt into the guest.
    pub fn inject_interrupt(&mut self, vector: usize) -> RiscvVcpuResult {
        self.set_virtual_interrupt_pending(vector, true)
    }

    /// Synchronizes controller-derived VSEIP state on the loaded owner.
    ///
    /// The virtual PLIC remains the owner of pending and delivery state. The
    /// caller must be the target vCPU owner and must have loaded the backend on
    /// the prebound host hart; remote register writes are not permitted.
    pub fn sync_vseip_level(&mut self, asserted: bool) -> RiscvVcpuResult {
        // Reject an unbound owner before touching the register image, so the
        // error path leaves no partial controller-derived state behind.
        let binding = self.binding.as_mut().ok_or(RiscvVcpuError::BadState)?;
        self.regs
            .set_interrupt_pending(GuestInterrupt::External, asserted);
        binding.sync_interrupt(&self.regs, GuestInterrupt::External);
        Ok(())
    }

    /// Sets the guest return value register.
    pub fn set_return_value(&mut self, val: usize) {
        self.set_gpr_from_gpr_index(GprIndex::A0, val);
    }

    /// Completes a previously returned SBI IPI request.
    pub fn complete_ipi(&mut self, request: RiscvIpiRequest, completion: RiscvIpiCompletion) {
        let result = match completion {
            RiscvIpiCompletion::Success => SbiRet::success(0),
            RiscvIpiCompletion::InvalidParameter => SbiRet::invalid_param(),
            RiscvIpiCompletion::Failed => SbiRet::failed(),
        };
        match request.abi() {
            RiscvIpiAbi::Legacy => {
                self.set_gpr_from_gpr_index(GprIndex::A0, result.error);
            }
            RiscvIpiAbi::SbiV02 => self.set_sbi_result(result),
        }
    }

    fn set_virtual_interrupt_pending(&mut self, vector: usize, pending: bool) -> RiscvVcpuResult {
        let interrupt = match vector {
            S_SOFT => GuestInterrupt::Software,
            S_TIMER => GuestInterrupt::Timer,
            vector if is_supervisor_external(vector) => GuestInterrupt::External,
            _ => return Err(RiscvVcpuError::Unsupported),
        };
        self.regs.set_interrupt_pending(interrupt, pending);
        if let Some(binding) = &mut self.binding {
            binding.sync_interrupt(&self.regs, interrupt);
        }
        Ok(())
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    #[cfg(feature = "sstc")]
    #[inline]
    fn program_guest_timer(&mut self, deadline: usize) -> RiscvVcpuResult {
        self.set_virtual_interrupt_pending(S_TIMER, false)?;
        self.binding
            .as_mut()
            .ok_or(RiscvVcpuError::BadState)?
            .set_timer_compare(&mut self.regs, deadline);
        Ok(())
    }

    #[cfg(not(feature = "sstc"))]
    #[inline]
    fn program_guest_timer(&mut self, _deadline: usize) -> RiscvVcpuResult {
        Err(RiscvVcpuError::Unsupported)
    }

    /// Gets one of the vCPU's general purpose registers.
    pub fn get_gpr(&self, index: GprIndex) -> usize {
        self.regs.guest_regs.gprs.reg(index)
    }

    /// Set one of the vCPU's general purpose register.
    pub fn set_gpr_from_gpr_index(&mut self, index: GprIndex, val: usize) {
        self.regs.guest_regs.gprs.set_reg(index, val);
    }

    /// Advance guest pc by `instr_len` bytes
    pub fn advance_pc(&mut self, instr_len: usize) {
        self.regs.guest_regs.sepc += instr_len
    }
}

impl<H: RiscvHostOps> RiscvVcpu<H> {
    /// Inject a synchronous VS exception so the guest handles a fault that happened during
    /// hypervisor-side instruction emulation.
    fn inject_guest_exception(
        &mut self,
        exception: Exception,
        fault_addr: RiscvGuestVirtAddr,
    ) -> RiscvVcpuResult {
        self.binding
            .as_mut()
            .ok_or(RiscvVcpuError::BadState)?
            .inject_exception(&mut self.regs, exception, fault_addr.as_usize().into());
        Ok(())
    }

    fn handle_guest_instruction_fetch_fault(
        &mut self,
        fault: guest_mem::GuestInstructionFetchFault,
    ) -> RiscvVcpuResult<RiscvVmExit> {
        match fault {
            // HLVX reports load-class faults, but the emulated operation is a
            // guest instruction fetch. Convert them before injecting to VS mode.
            guest_mem::GuestInstructionFetchFault::PageFault { addr } => {
                self.inject_guest_exception(Exception::InstructionPageFault, addr)?;
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::AccessFault { addr } => {
                self.inject_guest_exception(Exception::InstructionFault, addr)?;
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::Misaligned { addr } => {
                self.inject_guest_exception(Exception::InstructionMisaligned, addr)?;
                Ok(RiscvVmExit::Nothing)
            }
            guest_mem::GuestInstructionFetchFault::GuestPageFault { addr } => {
                // G-stage faults must stay visible to AxVM so it can populate or
                // reject the nested mapping.
                Ok(RiscvVmExit::NestedPageFault {
                    addr,
                    access_flags: RiscvAccessFlags::EXECUTE,
                })
            }
            guest_mem::GuestInstructionFetchFault::Unhandled {
                scause,
                stval,
                htval,
            } => {
                warn!(
                    "unhandled riscv HLVX fault while fetching guest instruction: \
                     scause={scause:#x}, stval={stval:#x}, htval={htval:#x}"
                );
                Err(RiscvVcpuError::GuestMemoryFault)
            }
        }
    }

    fn vmexit_handler(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        use ax_cpu::virtualization::{Interrupt, Trap};

        trace!(
            "vmexit_handler: {:?}, sepc: {:#x}, stval: {:#x}",
            self.regs.trap_csrs.cause(),
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval
        );

        // Try to convert the raw trap cause to a standard RISC-V trap cause.
        let trap = self.regs.trap_csrs.cause().map_err(|_| {
            error!(
                "Unknown trap cause: scause={:#x}",
                self.regs.trap_csrs.scause
            );
            RiscvVcpuError::InvalidTrap
        })?;

        match trap {
            Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
                let a = self.regs.guest_regs.gprs.a_regs();
                let param = [a[0], a[1], a[2], a[3], a[4], a[5]];
                let extension_id = a[7];
                let function_id = a[6];

                trace!("sbi_call: eid {extension_id:#x} fid {function_id:#x} param {param:?}");
                match extension_id {
                    // Compatibility with Legacy Extensions.
                    legacy::LEGACY_SET_TIMER..=legacy::LEGACY_SHUTDOWN => match extension_id {
                        legacy::LEGACY_SET_TIMER => {
                            // info!("set timer: {}", param[0]);
                            self.sbi.pmu.record_set_timer();
                            self.program_guest_timer(param[0])?;

                            self.set_gpr_from_gpr_index(GprIndex::A0, 0);
                        }
                        legacy::LEGACY_CONSOLE_PUTCHAR | legacy::LEGACY_CONSOLE_GETCHAR => {
                            return Ok(self.task_sbi_call(extension_id, function_id, param));
                        }
                        legacy::LEGACY_SEND_IPI => {
                            return self.handle_legacy_send_ipi(param[0], |guest_va, bytes| {
                                guest_mem::copy_from_guest_va(
                                    bytes,
                                    RiscvGuestVirtAddr::from(guest_va),
                                    true,
                                )
                            });
                        }
                        legacy::LEGACY_CLEAR_IPI => {
                            self.set_virtual_interrupt_pending(S_SOFT, false)?;
                            self.set_gpr_from_gpr_index(GprIndex::A0, RET_SUCCESS);
                        }
                        legacy::LEGACY_SHUTDOWN => {
                            // sbi_call_legacy_0(LEGACY_SHUTDOWN)
                            return Ok(RiscvVmExit::SystemDown);
                        }
                        _ => {
                            warn!(
                                "Unsupported SBI legacy extension id {extension_id:#x} function \
                                 id {function_id:#x}"
                            );
                        }
                    },
                    spi::EID_SPI => match function_id {
                        spi::SEND_IPI => {
                            let request =
                                crate::arch::riscv64::policy::sbi_ipi::decode_standard_request(
                                    param[0], param[1],
                                );
                            self.advance_pc(4);
                            return Ok(RiscvVmExit::SendIpi(request));
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    EID_TIME => match function_id {
                        FID_SET_TIMER => {
                            self.sbi.pmu.record_set_timer();
                            self.program_guest_timer(param[0])?;
                            self.sbi_return(RET_SUCCESS, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    // Handle HSM extension
                    hsm::EID_HSM => match function_id {
                        hsm::HART_START => {
                            let hartid = a[0];
                            let start_addr = a[1];
                            let opaque = a[2];
                            self.advance_pc(4);
                            return Ok(RiscvVmExit::CpuUp {
                                target_cpu: hartid as _,
                                entry_point: RiscvGuestPhysAddr::from(start_addr),
                                arg: opaque as _,
                            });
                        }
                        hsm::HART_STOP => {
                            return Ok(RiscvVmExit::CpuDown);
                        }
                        hsm::HART_SUSPEND => {
                            match a[0] {
                                value if value == hsm::suspend_type::RETENTIVE as usize => {
                                    // Retentive suspend preserves register state and resumes
                                    // after this ecall; the task owner commits SBI_SUCCESS.
                                    self.advance_pc(4);
                                    return Ok(RiscvVmExit::SbiStandby);
                                }
                                value if value == hsm::suspend_type::NON_RETENTIVE as usize => {
                                    self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                                }
                                _ => self.sbi_return(SbiRet::invalid_param().error, 0),
                            }
                            return Ok(RiscvVmExit::Nothing);
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    // Handle hypercall
                    EID_HVC => {
                        self.advance_pc(4);
                        return Ok(RiscvVmExit::Hypercall {
                            nr: function_id as _,
                            args: [
                                param[0] as _,
                                param[1] as _,
                                param[2] as _,
                                param[3] as _,
                                param[4] as _,
                                param[5] as _,
                            ],
                        });
                    }
                    // Guest memory and console firmware may block. Capture only
                    // ABI values while pinned; the task-side handler owns the copy.
                    EID_DBCN => {
                        return Ok(self.task_sbi_call(extension_id, function_id, param));
                    }
                    srst::EID_SRST => match function_id {
                        srst::SYSTEM_RESET => {
                            let reset_type = param[0];
                            if reset_type == srst::RESET_TYPE_SHUTDOWN as _ {
                                // Shutdown the system.
                                return Ok(RiscvVmExit::SystemDown);
                            } else {
                                self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                                return Ok(RiscvVmExit::Nothing);
                            }
                        }
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(RiscvVmExit::Nothing);
                        }
                    },
                    _ => {
                        return Ok(self.task_sbi_call(extension_id, function_id, param));
                    }
                };

                self.advance_pc(4);
                Ok(RiscvVmExit::Nothing)
            }
            Trap::Exception(Exception::VirtualInstruction) => self.handle_virtual_instruction(),
            Trap::Interrupt(
                Interrupt::SupervisorTimer
                | Interrupt::SupervisorSoft
                | Interrupt::SupervisorExternal,
            ) => {
                // These sources remain pending until the normal host IRQ
                // entry services them on this hart after IRQ restoration.
                Ok(RiscvVmExit::Nothing)
            }
            Trap::Exception(
                gpf @ (Exception::LoadGuestPageFault | Exception::StoreGuestPageFault),
            ) => self.handle_guest_page_fault(gpf == Exception::StoreGuestPageFault),
            _ => {
                error!(
                    "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}, htval: {:#x}, htinst: \
                     {:#x}, vsepc: {:#x}, vstval: {:#x}, vsatp: {:#x}, hgatp: {:#x}, a0-a3: \
                     [{:#x}, {:#x}, {:#x}, {:#x}]",
                    self.regs.trap_csrs.cause(),
                    self.regs.guest_regs.sepc,
                    self.regs.trap_csrs.stval,
                    self.regs.trap_csrs.htval,
                    self.regs.trap_csrs.htinst,
                    self.regs.vs_csrs.vsepc,
                    self.regs.vs_csrs.vstval,
                    self.regs.vs_csrs.vsatp,
                    self.regs.virtual_hs_csrs.hgatp,
                    self.regs.guest_regs.gprs.reg(GprIndex::A0),
                    self.regs.guest_regs.gprs.reg(GprIndex::A1),
                    self.regs.guest_regs.gprs.reg(GprIndex::A2),
                    self.regs.guest_regs.gprs.reg(GprIndex::A3)
                );
                Err(RiscvVcpuError::Unsupported)
            }
        }
    }

    fn task_sbi_call(
        &mut self,
        extension: usize,
        function: usize,
        arguments: [usize; 6],
    ) -> RiscvVmExit {
        self.advance_pc(4);
        RiscvVmExit::SbiCall(RiscvSbiCall {
            extension,
            function,
            arguments,
        })
    }

    /// Forwards firmware and virtual PMU requests after CPU binding retirement.
    pub(crate) fn forward_task_sbi(&mut self, call: RiscvSbiCall) -> RiscvVcpuResult<SbiRet> {
        if self.binding.is_some() {
            return Err(RiscvVcpuError::BadState);
        }
        if call.extension == rfnc::EID_RFNC {
            match call.function {
                rfnc::REMOTE_FENCE_I => self.sbi.pmu.record_fence_i_sent(),
                rfnc::REMOTE_SFENCE_VMA => self.sbi.pmu.record_sfence_vma_sent(),
                rfnc::REMOTE_SFENCE_VMA_ASID => self.sbi.pmu.record_sfence_vma_asid_sent(),
                rfnc::REMOTE_HFENCE_GVMA => self.sbi.pmu.record_hfence_gvma_sent(),
                rfnc::REMOTE_HFENCE_GVMA_VMID => self.sbi.pmu.record_hfence_gvma_vmid_sent(),
                rfnc::REMOTE_HFENCE_VVMA => self.sbi.pmu.record_hfence_vvma_sent(),
                rfnc::REMOTE_HFENCE_VVMA_ASID => self.sbi.pmu.record_hfence_vvma_asid_sent(),
                _ => {}
            }
        }
        Ok(self
            .sbi
            .handle_ecall(call.extension, call.function, call.arguments))
    }

    #[inline]
    fn sbi_return(&mut self, a0: usize, a1: usize) {
        self.set_sbi_result(SbiRet {
            error: a0,
            value: a1,
        });
        self.advance_pc(4);
    }

    #[inline]
    fn set_sbi_result(&mut self, result: SbiRet) {
        self.set_gpr_from_gpr_index(GprIndex::A0, result.error);
        self.set_gpr_from_gpr_index(GprIndex::A1, result.value);
    }

    fn handle_legacy_send_ipi(
        &mut self,
        hart_mask_ptr: usize,
        copy_from_guest_va: impl FnMut(usize, &mut [u8]) -> usize,
    ) -> RiscvVcpuResult<RiscvVmExit> {
        let request = match crate::arch::riscv64::policy::sbi_ipi::decode_legacy_request(
            hart_mask_ptr,
            copy_from_guest_va,
        ) {
            Ok(request) => request,
            Err(error) => {
                warn!("failed to read legacy SBI IPI hart mask at {hart_mask_ptr:#x}: {error:?}");
                self.sbi_return(RET_ERR_FAILED, 0);
                return Ok(RiscvVmExit::Nothing);
            }
        };

        self.advance_pc(4);
        Ok(RiscvVmExit::SendIpi(request))
    }

    #[cfg(feature = "sstc")]
    fn handle_virtual_instruction(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        let instr = match self.read_virtual_instruction()? {
            VirtualInstructionRead::Instruction(instr) => instr,
            VirtualInstructionRead::Handled(exit_reason) => return Ok(exit_reason),
        };
        let csr = ((instr >> 20) & 0xfff) as u16;

        if csr != CSR_STIMECMP {
            self.sbi.pmu.record_illegal_insn();
            warn!(
                "Unhandled virtual instruction csr={csr:#x}, sepc: {:#x}, stval: {:#x}, htval: \
                 {:#x}, htinst: {:#x}",
                self.regs.guest_regs.sepc,
                self.regs.trap_csrs.stval,
                self.regs.trap_csrs.htval,
                self.regs.trap_csrs.htinst,
            );
            return Err(RiscvVcpuError::Unsupported);
        }

        let funct3 = ((instr >> 12) & 0x7) as u8;
        let rd = ((instr >> 7) & 0x1f) as u8;
        let rs1 = ((instr >> 15) & 0x1f) as u8;
        let old_value = self.regs.vs_csrs.vstimecmp;
        let rs1_value = self.read_gpr_raw(rs1);
        let zimm = rs1 as usize;

        let new_value = match funct3 {
            0b001 => Some(rs1_value),
            0b010 => {
                if rs1 == 0 {
                    None
                } else {
                    Some(old_value | rs1_value)
                }
            }
            0b011 => {
                if rs1 == 0 {
                    None
                } else {
                    Some(old_value & !rs1_value)
                }
            }
            0b101 => Some(zimm),
            0b110 => {
                if zimm == 0 {
                    None
                } else {
                    Some(old_value | zimm)
                }
            }
            0b111 => {
                if zimm == 0 {
                    None
                } else {
                    Some(old_value & !zimm)
                }
            }
            _ => {
                self.sbi.pmu.record_illegal_insn();
                warn!(
                    "Unhandled virtual instruction funct3={funct3:#x} for csr={csr:#x}, sepc: \
                     {:#x}",
                    self.regs.guest_regs.sepc,
                );
                return Err(RiscvVcpuError::Unsupported);
            }
        };

        if rd != 0 {
            self.write_gpr_raw(rd, old_value);
        }

        if let Some(new_value) = new_value {
            // Linux is using the advertised `sstc` path (`csrw stimecmp,...`).
            // Keep the saved vCPU state synchronized with the hardware compare
            // without borrowing the host supervisor timer.
            self.program_guest_timer(new_value)?;
        }

        self.advance_pc(4);
        Ok(RiscvVmExit::Nothing)
    }

    #[cfg(not(feature = "sstc"))]
    fn handle_virtual_instruction(&mut self) -> RiscvVcpuResult<RiscvVmExit> {
        self.sbi.pmu.record_illegal_insn();
        warn!(
            "Unhandled virtual instruction without `sstc` feature, sepc: {:#x}, stval: {:#x}, \
             htval: {:#x}, htinst: {:#x}",
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval,
            self.regs.trap_csrs.htval,
            self.regs.trap_csrs.htinst,
        );
        Err(RiscvVcpuError::Unsupported)
    }

    #[cfg(feature = "sstc")]
    fn read_virtual_instruction(&mut self) -> RiscvVcpuResult<VirtualInstructionRead> {
        let instr = self.regs.trap_csrs.stval as u32;
        if instr & 0x7f == SYSTEM_OPCODE {
            return Ok(VirtualInstructionRead::Instruction(instr));
        }

        let guest_pc = RiscvGuestVirtAddr::from(self.regs.guest_regs.sepc);
        let supervisor = matches!(
            self.regs.guest_privilege(),
            ax_cpu::virtualization::GuestPrivilege::Supervisor
        );
        match guest_mem::fetch_guest_instruction(guest_pc, supervisor) {
            Ok(instr) => Ok(VirtualInstructionRead::Instruction(instr)),
            Err(fault) => self
                .handle_guest_instruction_fetch_fault(fault)
                .map(VirtualInstructionRead::Handled),
        }
    }

    #[cfg(feature = "sstc")]
    fn read_gpr_raw(&self, index: u8) -> usize {
        GprIndex::from_raw(index as u32)
            .map(|gpr| self.get_gpr(gpr))
            .unwrap_or(0)
    }

    #[cfg(feature = "sstc")]
    fn write_gpr_raw(&mut self, index: u8, value: usize) {
        if let Some(gpr) = GprIndex::from_raw(index as u32) {
            self.set_gpr_from_gpr_index(gpr, value);
        }
    }

    /// Decode the instruction at the given virtual address. Return the decoded instruction and its
    /// length in bytes, or an exit reason already produced while fetching it.
    fn decode_instr_at(&mut self, vaddr: RiscvGuestVirtAddr) -> RiscvVcpuResult<InstructionDecode> {
        // Use the value captured together with the guest trap. Reading the
        // live CSR here races with host interrupts, which may overwrite it.
        let mut instr = self.regs.trap_csrs.htinst;
        let instr_len;
        if instr == 0 {
            // Read the instruction from guest memory.
            let supervisor = matches!(
                self.regs.guest_privilege(),
                ax_cpu::virtualization::GuestPrivilege::Supervisor
            );
            instr = match guest_mem::fetch_guest_instruction(vaddr, supervisor) {
                Ok(instr) => instr as _,
                Err(fault) => {
                    return self
                        .handle_guest_instruction_fetch_fault(fault)
                        .map(InstructionDecode::Handled);
                }
            };
            instr_len = riscv_decode::instruction_length(instr as u16);
            instr = match instr_len {
                2 => instr & 0xffff,
                4 => instr,
                _ => return Err(RiscvVcpuError::DecodeFailed),
            };
        } else if instr_is_pseudo(instr as u32) {
            error!("fault on 1st stage page table walk");
            return Err(RiscvVcpuError::Unsupported);
        } else {
            // Transform htinst value to standard instruction.
            // According to RISC-V Spec:
            //      Bits 1:0 of a transformed standard instruction will be binary 01 if
            //      the trapping instruction is compressed and 11 if not.
            instr_len = match (instr as u16) & 0x3 {
                0x1 => 2,
                0x3 => 4,
                _ => return Err(RiscvVcpuError::DecodeFailed),
            };
            instr |= 0x2;
        }

        riscv_decode::decode(instr as u32)
            .map_err(|_| RiscvVcpuError::DecodeFailed)
            .map(|instr| InstructionDecode::Decoded(instr, instr_len))
    }

    /// Handle a guest page fault. Return an exit reason.
    fn handle_guest_page_fault(&mut self, _writing: bool) -> RiscvVcpuResult<RiscvVmExit> {
        let fault_addr = RiscvGuestPhysAddr::from_usize(self.regs.trap_csrs.gpt_page_fault_addr());
        let sepc = self.regs.guest_regs.sepc;
        let sepc_vaddr = RiscvGuestVirtAddr::from(sepc);

        /// Temporary enum to represent the decoded operation.
        enum DecodedOp {
            Read {
                i: IType,
                width: RiscvAccessWidth,
                signed_ext: bool,
            },
            Write {
                s: SType,
                width: RiscvAccessWidth,
            },
        }

        use DecodedOp::*;

        let (decoded_instr, instr_len) = match self.decode_instr_at(sepc_vaddr)? {
            InstructionDecode::Decoded(instr, instr_len) => (instr, instr_len),
            InstructionDecode::Handled(exit_reason) => return Ok(exit_reason),
        };
        let op = match decoded_instr {
            Instruction::Lb(i) => Read {
                i,
                width: RiscvAccessWidth::Byte,
                signed_ext: true,
            },
            Instruction::Lh(i) => Read {
                i,
                width: RiscvAccessWidth::Word,
                signed_ext: true,
            },
            Instruction::Lw(i) => Read {
                i,
                width: RiscvAccessWidth::Dword,
                signed_ext: true,
            },
            Instruction::Ld(i) => Read {
                i,
                width: RiscvAccessWidth::Qword,
                signed_ext: true,
            },
            Instruction::Lbu(i) => Read {
                i,
                width: RiscvAccessWidth::Byte,
                signed_ext: false,
            },
            Instruction::Lhu(i) => Read {
                i,
                width: RiscvAccessWidth::Word,
                signed_ext: false,
            },
            Instruction::Lwu(i) => Read {
                i,
                width: RiscvAccessWidth::Dword,
                signed_ext: false,
            },
            Instruction::Sb(s) => Write {
                s,
                width: RiscvAccessWidth::Byte,
            },
            Instruction::Sh(s) => Write {
                s,
                width: RiscvAccessWidth::Word,
            },
            Instruction::Sw(s) => Write {
                s,
                width: RiscvAccessWidth::Dword,
            },
            Instruction::Sd(s) => Write {
                s,
                width: RiscvAccessWidth::Qword,
            },
            _ => {
                // Not a load or store instruction, so we cannot handle it here, return a nested page fault.
                return Ok(RiscvVmExit::NestedPageFault {
                    addr: fault_addr,
                    access_flags: RiscvAccessFlags::empty(),
                });
            }
        };

        // The instruction pointer is not advanced here. The captured length is
        // retired by the vCPU owner only after the task-layer device access
        // succeeds, so a faulted or unconsumed access leaves the retry state
        // intact.
        Ok(match op {
            Read {
                i,
                width,
                signed_ext,
            } => {
                self.sbi.pmu.record_access_load();
                RiscvVmExit::MmioRead {
                    addr: fault_addr,
                    width,
                    reg: i.rd() as _,
                    reg_width: RiscvAccessWidth::Qword,
                    signed_ext,
                    advance: instr_len,
                }
            }
            Write { s, width } => {
                self.sbi.pmu.record_access_store();
                let source_reg = s.rs2();
                let value = self
                    .get_gpr(GprIndex::from_raw(source_reg).ok_or(RiscvVcpuError::DecodeFailed)?);

                RiscvVmExit::MmioWrite {
                    addr: fault_addr,
                    width,
                    data: value as _,
                    advance: instr_len,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::arch::riscv64::policy::{RiscvHostPhysAddr, RiscvHostVirtAddr};

    struct TestHost;

    impl RiscvHostOps for TestHost {
        fn virt_to_phys(_vaddr: RiscvHostVirtAddr) -> RiscvHostPhysAddr {
            RiscvHostPhysAddr::from_usize(0)
        }
    }

    #[test]
    fn retentive_suspend_retires_the_ecall_and_rejects_unsupported_states() {
        let mut vcpu = RiscvVcpu::<TestHost>::default();
        vcpu.regs.trap_csrs.scause = 10;
        vcpu.regs.guest_regs.sepc = 0x8020_0000;
        vcpu.set_gpr_from_gpr_index(GprIndex::A7, hsm::EID_HSM);
        vcpu.set_gpr_from_gpr_index(GprIndex::A6, hsm::HART_SUSPEND);
        vcpu.set_gpr_from_gpr_index(GprIndex::A0, hsm::suspend_type::RETENTIVE as usize);
        assert!(matches!(
            vcpu.vmexit_handler().unwrap(),
            RiscvVmExit::SbiStandby
        ));
        assert_eq!(vcpu.regs.guest_regs.sepc, 0x8020_0004);

        for (kind, expected) in [
            (
                hsm::suspend_type::NON_RETENTIVE as usize,
                SbiRet::not_supported(),
            ),
            (1, SbiRet::invalid_param()),
        ] {
            vcpu.set_gpr_from_gpr_index(GprIndex::A0, kind);
            assert!(matches!(
                vcpu.vmexit_handler().unwrap(),
                RiscvVmExit::Nothing
            ));
            assert_eq!(vcpu.get_gpr(GprIndex::A0), expected.error);
            assert_eq!(vcpu.get_gpr(GprIndex::A1), expected.value);
        }
        assert_eq!(vcpu.regs.guest_regs.sepc, 0x8020_000c);
    }

    #[test]
    fn console_decode_returns_owned_abi_values_without_firmware_access() {
        let mut vcpu = RiscvVcpu::<TestHost>::default();
        vcpu.regs.trap_csrs.scause = 10;
        vcpu.regs.guest_regs.sepc = 0x8020_0000;
        vcpu.set_gpr_from_gpr_index(GprIndex::A7, EID_DBCN);
        vcpu.set_gpr_from_gpr_index(GprIndex::A6, FID_CONSOLE_WRITE);
        // A zero-sized write also goes through the durable boundary; the old
        // inline implementation returned Nothing and cannot satisfy this test.
        vcpu.set_gpr_from_gpr_index(GprIndex::A0, 0);
        vcpu.set_gpr_from_gpr_index(GprIndex::A1, 0x9000_0000);
        let RiscvVmExit::SbiCall(call) = vcpu.vmexit_handler().unwrap() else {
            panic!("console work must be interpreted after hardware unloading");
        };
        assert_eq!(call.arguments[..3], [0, 0x9000_0000, 0]);
        assert_eq!(vcpu.regs.guest_regs.sepc, 0x8020_0004);
    }

    #[test]
    fn legacy_ipi_completion_updates_only_a0() {
        let mut vcpu = RiscvVcpu::<TestHost>::default();
        let request = RiscvIpiRequest::new(1, 0, RiscvIpiAbi::Legacy);

        for (completion, expected) in [
            (RiscvIpiCompletion::Success, SbiRet::success(0)),
            (
                RiscvIpiCompletion::InvalidParameter,
                SbiRet::invalid_param(),
            ),
            (RiscvIpiCompletion::Failed, SbiRet::failed()),
        ] {
            let preserved_a1 = 0xfeed_face;
            vcpu.set_gpr_from_gpr_index(GprIndex::A1, preserved_a1);

            vcpu.complete_ipi(request, completion);

            assert_eq!(vcpu.get_gpr(GprIndex::A0), expected.error);
            assert_eq!(vcpu.get_gpr(GprIndex::A1), preserved_a1);
        }
    }

    #[test]
    fn sbi_v02_ipi_completion_updates_a0_and_a1() {
        let mut vcpu = RiscvVcpu::<TestHost>::default();
        let request = RiscvIpiRequest::new(1, 0, RiscvIpiAbi::SbiV02);

        for (completion, expected) in [
            (RiscvIpiCompletion::Success, SbiRet::success(0)),
            (
                RiscvIpiCompletion::InvalidParameter,
                SbiRet::invalid_param(),
            ),
            (RiscvIpiCompletion::Failed, SbiRet::failed()),
        ] {
            vcpu.set_gpr_from_gpr_index(GprIndex::A0, usize::MAX);
            vcpu.set_gpr_from_gpr_index(GprIndex::A1, usize::MAX);

            vcpu.complete_ipi(request, completion);

            assert_eq!(vcpu.get_gpr(GprIndex::A0), expected.error);
            assert_eq!(vcpu.get_gpr(GprIndex::A1), expected.value);
        }
    }

    #[test]
    fn unbound_vcpu_rejects_unbind_before_touching_host_csrs() {
        let mut vcpu = RiscvVcpu::<TestHost>::default();

        assert_eq!(vcpu.unbind(), Err(RiscvVcpuError::BadState));
    }
}

fn binding_error(error: VirtualizationError) -> RiscvVcpuError {
    match error {
        VirtualizationError::InvalidRoot | VirtualizationError::InvalidVector => {
            RiscvVcpuError::InvalidInput
        }
        VirtualizationError::Unavailable
        | VirtualizationError::UnsupportedPaging
        | VirtualizationError::UnsupportedTimer => RiscvVcpuError::Unsupported,
        VirtualizationError::AlreadyEnabled | VirtualizationError::NotEnabled => {
            RiscvVcpuError::BadState
        }
    }
}
