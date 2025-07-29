use crate::sbi_console::*;
use axaddrspace::device::AccessWidth;
use riscv::register::hstatus;
use riscv::register::{hvip, scause, sie, sstatus};
use riscv_decode::Instruction;
use riscv_decode::types::{IType, SType};
use rustsbi::{Forward, RustSBI};
use sbi_spec::{hsm, legacy};

use crate::regs::*;
use crate::{EID_HVC, RISCVVCpuCreateConfig, guest_mem};
use axaddrspace::{GuestPhysAddr, GuestVirtAddr, HostPhysAddr, MappingFlags};
use axerrno::AxResult;
use axvcpu::{AxVCpuExitReason, AxVCpuHal};

unsafe extern "C" {
    fn _run_guest(state: *mut VmCpuRegisters);
}

/// The architecture dependent configuration of a `AxArchVCpu`.
#[derive(Clone, Copy, Debug, Default)]
pub struct VCpuConfig {}

#[derive(Default)]
/// A virtual CPU within a guest
pub struct RISCVVCpu<H: AxVCpuHal> {
    regs: VmCpuRegisters,
    sbi: RISCVVCpuSbi,
    _marker: core::marker::PhantomData<H>,
}

#[derive(RustSBI)]
struct RISCVVCpuSbi {
    #[rustsbi(console, pmu, fence, reset, info, hsm)]
    forward: Forward,
}

impl Default for RISCVVCpuSbi {
    #[inline]
    fn default() -> Self {
        Self { forward: Forward }
    }
}

impl<H: AxVCpuHal> axvcpu::AxArchVCpu for RISCVVCpu<H> {
    type CreateConfig = RISCVVCpuCreateConfig;

    type SetupConfig = ();

    fn new(_vm_id: usize, _vcpu_id: usize, config: Self::CreateConfig) -> AxResult<Self> {
        let mut regs = VmCpuRegisters::default();
        // Setup the guest's general purpose registers.
        // `a0` is the hartid
        regs.guest_regs.gprs.set_reg(GprIndex::A0, config.hart_id);
        // `a1` is the address of the device tree blob.
        regs.guest_regs
            .gprs
            .set_reg(GprIndex::A1, config.dtb_addr.as_usize());

        Ok(Self {
            regs,
            sbi: RISCVVCpuSbi::default(),
            _marker: core::marker::PhantomData,
        })
    }

    fn setup(&mut self, _config: Self::SetupConfig) -> AxResult {
        // Set sstatus.
        let mut sstatus = sstatus::read();
        sstatus.set_spp(sstatus::SPP::Supervisor);
        self.regs.guest_regs.sstatus = sstatus.bits();

        // Set hstatus.
        let mut hstatus = hstatus::read();
        hstatus.set_spv(true);
        // Set SPVP bit in order to accessing VS-mode memory from HS-mode.
        hstatus.set_spvp(true);
        unsafe {
            hstatus.write();
        }
        self.regs.guest_regs.hstatus = hstatus.bits();
        Ok(())
    }

    fn set_entry(&mut self, entry: GuestPhysAddr) -> AxResult {
        self.regs.guest_regs.sepc = entry.as_usize();
        Ok(())
    }

    fn set_ept_root(&mut self, ept_root: HostPhysAddr) -> AxResult {
        self.regs.virtual_hs_csrs.hgatp = 8usize << 60 | usize::from(ept_root) >> 12;
        Ok(())
    }

    fn run(&mut self) -> AxResult<AxVCpuExitReason> {
        unsafe {
            sstatus::clear_sie();
            sie::set_sext();
            sie::set_ssoft();
            sie::set_stimer();
        }
        unsafe {
            // Safe to run the guest as it only touches memory assigned to it by being owned
            // by its page table
            _run_guest(&mut self.regs);
        }
        unsafe {
            sie::clear_sext();
            sie::clear_ssoft();
            sie::clear_stimer();
            sstatus::set_sie();
        }
        self.vmexit_handler()
    }

    fn bind(&mut self) -> AxResult {
        unsafe {
            core::arch::asm!(
                "csrw hgatp, {hgatp}",
                hgatp = in(reg) self.regs.virtual_hs_csrs.hgatp,
            );
            core::arch::riscv64::hfence_gvma_all();
        }
        Ok(())
    }

    fn unbind(&mut self) -> AxResult {
        Ok(())
    }

    /// Set one of the vCPU's general purpose register.
    fn set_gpr(&mut self, index: usize, val: usize) {
        match index {
            0..=7 => {
                self.set_gpr_from_gpr_index(GprIndex::from_raw(index as u32 + 10).unwrap(), val);
            }
            _ => {
                warn!(
                    "RISCVVCpu: Unsupported general purpose register index: {}",
                    index
                );
            }
        }
    }

    fn inject_interrupt(&mut self, _vector: usize) -> AxResult {
        unimplemented!("RISCVVCpu::inject_interrupt is not implemented yet");
    }

    fn set_return_value(&mut self, val: usize) {
        self.set_gpr_from_gpr_index(GprIndex::A0, val);
    }
}

impl<H: AxVCpuHal> RISCVVCpu<H> {
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

    /// Gets the vCPU's registers.
    pub fn regs(&mut self) -> &mut VmCpuRegisters {
        &mut self.regs
    }
}

impl<H: AxVCpuHal> RISCVVCpu<H> {
    fn vmexit_handler(&mut self) -> AxResult<AxVCpuExitReason> {
        self.regs.trap_csrs.load_from_hw();

        let scause = scause::read();
        use scause::{Exception, Interrupt, Trap};

        trace!(
            "vmexit_handler: {:?}, sepc: {:#x}, stval: {:#x}",
            scause.cause(),
            self.regs.guest_regs.sepc,
            self.regs.trap_csrs.stval
        );

        match scause.cause() {
            Trap::Exception(Exception::VirtualSupervisorEnvCall) => {
                let a = self.regs.guest_regs.gprs.a_regs();
                let param = [a[0], a[1], a[2], a[3], a[4], a[5]];
                let extension_id = a[7];
                let function_id = a[6];

                trace!(
                    "sbi_call: eid {:#x} ('{}') fid {:#x} param {:?}",
                    extension_id,
                    alloc::string::String::from_utf8_lossy(&(extension_id as u32).to_be_bytes()),
                    function_id,
                    param
                );
                match extension_id {
                    // Compatibility with Legacy Extensions.
                    legacy::LEGACY_SET_TIMER..=legacy::LEGACY_SHUTDOWN => match extension_id {
                        legacy::LEGACY_SET_TIMER => {
                            // info!("set timer: {}", param[0]);
                            sbi_rt::set_timer((param[0]) as u64);
                            unsafe {
                                // Clear guest timer interrupt
                                hvip::clear_vstip();
                            }

                            self.set_gpr_from_gpr_index(GprIndex::A0, 0);
                        }
                        legacy::LEGACY_CONSOLE_PUTCHAR => {
                            sbi_call_legacy_1(legacy::LEGACY_CONSOLE_PUTCHAR, param[0]);
                        }
                        legacy::LEGACY_CONSOLE_GETCHAR => {
                            let c = sbi_call_legacy_0(legacy::LEGACY_CONSOLE_GETCHAR);
                            self.set_gpr_from_gpr_index(GprIndex::A0, c);
                        }
                        legacy::LEGACY_SHUTDOWN => {
                            // sbi_call_legacy_0(LEGACY_SHUTDOWN)
                            return Ok(AxVCpuExitReason::SystemDown);
                        }
                        _ => {
                            warn!(
                                "Unsupported SBI legacy extension id {:#x} function id {:#x}",
                                extension_id, function_id
                            );
                        }
                    },
                    // Handle HSM extension
                    hsm::EID_HSM => match function_id {
                        hsm::HART_START => {
                            let hartid = a[0];
                            let start_addr = a[1];
                            let opaque = a[2];
                            self.advance_pc(4);
                            return Ok(AxVCpuExitReason::CpuUp {
                                target_cpu: hartid as _,
                                entry_point: GuestPhysAddr::from(start_addr),
                                arg: opaque as _,
                            });
                        }
                        hsm::HART_STOP => {
                            return Ok(AxVCpuExitReason::CpuDown { _state: 0 });
                        }
                        hsm::HART_SUSPEND => {
                            // Todo: support these parameters.
                            let _suspend_type = a[0];
                            let _resume_addr = a[1];
                            let _opaque = a[2];
                            return Ok(AxVCpuExitReason::Halt);
                        }
                        _ => todo!(),
                    },
                    // Handle hypercall
                    EID_HVC => {
                        self.advance_pc(4);
                        return Ok(AxVCpuExitReason::Hypercall {
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
                    // Debug Console Extension
                    EID_DBCN => match function_id {
                        // Write from memory region to debug console.
                        FID_CONSOLE_WRITE => {
                            let num_bytes = param[0];
                            let gpa = join_u64(param[1], param[2]);

                            if num_bytes == 0 {
                                self.sbi_return(RET_SUCCESS, 0);
                                return Ok(AxVCpuExitReason::Nothing);
                            }

                            let mut buf = alloc::vec![0u8; num_bytes as usize];
                            let copied = guest_mem::copy_from_guest(
                                &mut *buf,
                                GuestPhysAddr::from(gpa as usize),
                            );

                            if copied == buf.len() {
                                let ret = console_write(&buf);
                                self.sbi_return(ret.error, ret.value);
                            } else {
                                self.sbi_return(RET_ERR_FAILED, 0);
                            }

                            return Ok(AxVCpuExitReason::Nothing);
                        }
                        // Read to memory region from debug console.
                        FID_CONSOLE_READ => {
                            let num_bytes = param[0];
                            let gpa = join_u64(param[1], param[2]);

                            if num_bytes == 0 {
                                self.sbi_return(RET_SUCCESS, 0);
                                return Ok(AxVCpuExitReason::Nothing);
                            }

                            let mut buf = alloc::vec![0u8; num_bytes as usize];
                            let ret = console_read(&mut buf);

                            if ret.is_ok() && ret.value <= buf.len() {
                                let copied = guest_mem::copy_to_guest(
                                    &buf[..ret.value],
                                    GuestPhysAddr::from(gpa as usize),
                                );
                                if copied == ret.value {
                                    self.sbi_return(RET_SUCCESS, ret.value);
                                } else {
                                    self.sbi_return(RET_ERR_FAILED, 0);
                                }
                            } else {
                                self.sbi_return(ret.error, ret.value);
                            }

                            return Ok(AxVCpuExitReason::Nothing);
                        }
                        // Write a single byte to debug console.
                        FID_CONSOLE_WRITE_BYTE => {
                            let byte = (param[0] & 0xff) as u8;
                            print_byte(byte);
                            self.sbi_return(RET_SUCCESS, 0);
                            return Ok(AxVCpuExitReason::Nothing);
                        }
                        // Unknown FID.
                        _ => {
                            self.sbi_return(RET_ERR_NOT_SUPPORTED, 0);
                            return Ok(AxVCpuExitReason::Nothing);
                        }
                    },
                    // By default, forward the SBI call to the RustSBI implementation.
                    // See [`RISCVVCpuSbi`].
                    _ => {
                        let ret = self.sbi.handle_ecall(extension_id, function_id, param);
                        if ret.is_err() {
                            warn!(
                                "forward ecall eid {:#x} fid {:#x} param {:#x?} err {:#x} value {:#x}",
                                extension_id, function_id, param, ret.error, ret.value
                            );
                        }
                        self.set_gpr_from_gpr_index(GprIndex::A0, ret.error);
                        self.set_gpr_from_gpr_index(GprIndex::A1, ret.value);
                    }
                };

                self.advance_pc(4);
                Ok(AxVCpuExitReason::Nothing)
            }
            Trap::Interrupt(Interrupt::SupervisorTimer) => {
                // Enable guest timer interrupt
                unsafe {
                    hvip::set_vstip();
                    sie::set_stimer();
                }

                Ok(AxVCpuExitReason::Nothing)
            }
            Trap::Interrupt(Interrupt::SupervisorExternal) => {
                // 9 == Interrupt::SupervisorExternal
                //
                // It's a great fault in the `riscv` crate that `Interrupt` and `Exception` are not
                // explicitly numbered, and they provide no way to convert them to a number. Also,
                // `as usize` will give use a wrong value.
                Ok(AxVCpuExitReason::ExternalInterrupt { vector: 9 })
            }
            Trap::Exception(
                gpf @ (Exception::LoadGuestPageFault | Exception::StoreGuestPageFault),
            ) => self.handle_guest_page_fault(gpf == Exception::StoreGuestPageFault),
            _ => {
                panic!(
                    "Unhandled trap: {:?}, sepc: {:#x}, stval: {:#x}",
                    scause.cause(),
                    self.regs.guest_regs.sepc,
                    self.regs.trap_csrs.stval
                );
            }
        }
    }

    #[inline]
    fn sbi_return(&mut self, a0: usize, a1: usize) {
        self.set_gpr_from_gpr_index(GprIndex::A0, a0);
        self.set_gpr_from_gpr_index(GprIndex::A1, a1);
        self.advance_pc(4);
    }

    /// Decode the instruction at the given virtual address. Return the decoded instruction and its
    /// length in bytes.
    fn decode_instr_at(&self, vaddr: GuestVirtAddr) -> AxResult<(Instruction, usize)> {
        // The htinst CSR contains "transformed instruction" that caused the page fault. We
        // can use it but we use the sepc to fetch the original instruction instead for now.
        let instr = guest_mem::fetch_guest_instruction(vaddr);
        let instr_len = riscv_decode::instruction_length(instr as u16);
        let instr = match instr_len {
            2 => instr & 0xffff,
            4 => instr,
            _ => unreachable!("Unsupported instruction length: {}", instr_len),
        };

        riscv_decode::decode(instr as u32)
            .map_err(|_| {
                axerrno::ax_err_type!(
                    Unsupported,
                    "risc-v vcpu guest pf handler decoding instruction failed"
                )
            })
            .map(|instr| (instr, instr_len))
    }

    /// Handle a guest page fault. Return an exit reason.
    fn handle_guest_page_fault(&mut self, _writing: bool) -> AxResult<AxVCpuExitReason> {
        let fault_addr = self.regs.trap_csrs.gpt_page_fault_addr();
        let sepc = self.regs.guest_regs.sepc;
        let sepc_vaddr = GuestVirtAddr::from(sepc);

        /// Temporary enum to represent the decoded operation.
        enum DecodedOp {
            Read {
                i: IType,
                width: AccessWidth,
                signed_ext: bool,
            },
            Write {
                s: SType,
                width: AccessWidth,
            },
        }

        use DecodedOp::*;

        let (decoded_instr, instr_len) = self.decode_instr_at(sepc_vaddr)?;
        let op = match decoded_instr {
            Instruction::Lb(i) => Read {
                i,
                width: AccessWidth::Byte,
                signed_ext: true,
            },
            Instruction::Lh(i) => Read {
                i,
                width: AccessWidth::Word,
                signed_ext: true,
            },
            Instruction::Lw(i) => Read {
                i,
                width: AccessWidth::Dword,
                signed_ext: true,
            },
            Instruction::Ld(i) => Read {
                i,
                width: AccessWidth::Qword,
                signed_ext: true,
            },
            Instruction::Lbu(i) => Read {
                i,
                width: AccessWidth::Byte,
                signed_ext: false,
            },
            Instruction::Lhu(i) => Read {
                i,
                width: AccessWidth::Word,
                signed_ext: false,
            },
            Instruction::Lwu(i) => Read {
                i,
                width: AccessWidth::Dword,
                signed_ext: false,
            },
            Instruction::Sb(s) => Write {
                s,
                width: AccessWidth::Byte,
            },
            Instruction::Sh(s) => Write {
                s,
                width: AccessWidth::Word,
            },
            Instruction::Sw(s) => Write {
                s,
                width: AccessWidth::Dword,
            },
            Instruction::Sd(s) => Write {
                s,
                width: AccessWidth::Qword,
            },
            _ => {
                // Not a load or store instruction, so we cannot handle it here, return a nested page fault.
                return Ok(AxVCpuExitReason::NestedPageFault {
                    addr: fault_addr,
                    access_flags: MappingFlags::empty(),
                });
            }
        };

        // WARN: This is a temporary place to add the instruction length to the guest's sepc.
        self.advance_pc(instr_len);

        Ok(match op {
            Read {
                i,
                width,
                signed_ext,
            } => AxVCpuExitReason::MmioRead {
                addr: fault_addr,
                width,
                reg: i.rd() as _,
                reg_width: AccessWidth::Qword,
                signed_ext,
            },
            Write { s, width } => {
                let source_reg = s.rs2();
                let value = self.get_gpr(unsafe {
                    // SAFETY: `source_reg` is guaranteed to be in [0, 31]
                    GprIndex::from_raw(source_reg).unwrap_unchecked()
                });

                AxVCpuExitReason::MmioWrite {
                    addr: fault_addr,
                    width,
                    data: value as _,
                }
            }
        })
    }
}

#[inline(always)]
fn sbi_call_legacy_0(eid: usize) -> usize {
    let error;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") eid,
            lateout("a0") error,
        );
    }
    error
}

#[inline(always)]
fn sbi_call_legacy_1(eid: usize, arg0: usize) -> usize {
    let error;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") eid,
            inlateout("a0") arg0 => error,
        );
    }
    error
}
