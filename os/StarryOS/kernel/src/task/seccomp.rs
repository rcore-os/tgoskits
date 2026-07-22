//! Per-thread seccomp state and classic BPF filter evaluation.
//!
//! StarryOS supports the Linux `SECCOMP_SET_MODE_STRICT` and
//! `SECCOMP_SET_MODE_FILTER` paths used by `seccomp(2)` and
//! `prctl(PR_SET_SECCOMP)`.  Filters are stored on each thread, inherited by
//! clone/fork, and evaluated before syscall dispatch.  The filter VM here is a
//! compact classic-BPF interpreter for `struct seccomp_data`; unsupported or
//! malformed programs fail closed by returning a kill decision.

use alloc::vec::Vec;

use ax_errno::{AxError, AxResult};
use ax_runtime::hal::cpu::uspace::UserContext;
use syscalls::Sysno;

const BPF_MAXINSNS: usize = 4096;
const BPF_MEMWORDS: usize = 16;

const BPF_CLASS_MASK: u16 = 0x07;
const BPF_LD: u16 = 0x00;
const BPF_LDX: u16 = 0x01;
const BPF_ST: u16 = 0x02;
const BPF_STX: u16 = 0x03;
const BPF_ALU: u16 = 0x04;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_MISC: u16 = 0x07;

const BPF_SIZE_MASK: u16 = 0x18;
const BPF_W: u16 = 0x00;
const BPF_H: u16 = 0x08;
const BPF_B: u16 = 0x10;

const BPF_MODE_MASK: u16 = 0xe0;
const BPF_IMM: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_MEM: u16 = 0x60;
const BPF_LEN: u16 = 0x80;

const BPF_OP_MASK: u16 = 0xf0;
const BPF_ADD: u16 = 0x00;
const BPF_SUB: u16 = 0x10;
const BPF_MUL: u16 = 0x20;
const BPF_DIV: u16 = 0x30;
const BPF_OR: u16 = 0x40;
const BPF_AND: u16 = 0x50;
const BPF_LSH: u16 = 0x60;
const BPF_RSH: u16 = 0x70;
const BPF_NEG: u16 = 0x80;
const BPF_MOD: u16 = 0x90;
const BPF_XOR: u16 = 0xa0;

const BPF_JA: u16 = 0x00;
const BPF_JEQ: u16 = 0x10;
const BPF_JGT: u16 = 0x20;
const BPF_JGE: u16 = 0x30;
const BPF_JSET: u16 = 0x40;

const BPF_SRC_MASK: u16 = 0x08;
const BPF_X: u16 = 0x08;

const BPF_TAX: u16 = 0x00;
const BPF_TXA: u16 = 0x80;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;
const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ACTION_FULL: u32 = 0xffff_0000;
const SECCOMP_RET_DATA: u32 = 0x0000_ffff;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(target_arch = "riscv64")]
const AUDIT_ARCH: u32 = 0xc000_00f3;
#[cfg(target_arch = "loongarch64")]
const AUDIT_ARCH: u32 = 0xc000_0102;
#[cfg(not(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "riscv64",
    target_arch = "loongarch64"
)))]
const AUDIT_ARCH: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
/// Linux `struct sock_filter` instruction used by classic BPF seccomp filters.
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// Linux `struct sock_fprog` header passed from userspace to install a filter.
pub struct SockFprog {
    pub len: u16,
    pub filter: *const SockFilter,
}

#[derive(Clone, Debug, Default)]
/// Seccomp configuration attached to a StarryOS thread.
///
/// A disabled state allows every syscall.  Strict mode admits only the small
/// Linux strict-mode syscall set.  Filter mode runs one or more classic-BPF
/// programs and converts the returned seccomp action into a dispatcher
/// decision.
pub struct SeccompState {
    mode: SeccompMode,
    filters: Vec<SeccompFilter>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// Active seccomp operating mode for a thread.
enum SeccompMode {
    #[default]
    Disabled,
    Strict,
    Filter,
}

#[derive(Clone, Debug)]
/// A validated classic-BPF seccomp program.
pub struct SeccompFilter {
    insns: Vec<SockFilter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Result of evaluating seccomp for one syscall entry.
pub enum SeccompDecision {
    /// Continue normal syscall dispatch.
    Allow,
    /// Return `-errno` to userspace without invoking the syscall.
    Errno(u16),
    /// Terminate every thread in the current process.
    KillProcess,
    /// Terminate only the syscalling thread.
    KillThread,
    /// Reject an action StarryOS does not currently emulate, such as TRAP.
    UnsupportedAction,
}

#[derive(Clone, Copy)]
/// The seccomp data record visible to classic BPF `BPF_ABS` loads.
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

impl SeccompState {
    /// Enable Linux strict seccomp mode for this thread.
    ///
    /// Strict mode can only be installed from the disabled state.  Once a
    /// seccomp mode is active, Linux does not allow returning to disabled mode.
    pub fn install_strict(&mut self) -> AxResult<()> {
        if self.mode != SeccompMode::Disabled {
            return Err(AxError::InvalidInput);
        }
        self.mode = SeccompMode::Strict;
        Ok(())
    }

    /// Append a classic-BPF filter and switch the thread to filter mode.
    ///
    /// Multiple filters are all evaluated, and their raw return actions are
    /// merged using Linux seccomp action precedence.
    pub fn append_filter(&mut self, insns: Vec<SockFilter>) -> AxResult<()> {
        let filter = SeccompFilter::new(insns)?;
        self.mode = SeccompMode::Filter;
        self.filters.push(filter);
        Ok(())
    }

    /// Evaluate this thread's seccomp state against a syscall user context.
    pub fn evaluate(&self, uctx: &UserContext) -> SeccompDecision {
        match self.mode {
            SeccompMode::Disabled => SeccompDecision::Allow,
            SeccompMode::Strict => strict_decision(uctx.sysno()),
            SeccompMode::Filter => self.evaluate_filters(uctx),
        }
    }

    /// Run all installed filters against a constructed `seccomp_data` record.
    fn evaluate_filters(&self, uctx: &UserContext) -> SeccompDecision {
        let data = SeccompData {
            nr: uctx.sysno() as i32,
            arch: AUDIT_ARCH,
            instruction_pointer: uctx.ip() as u64,
            args: [
                uctx.arg0() as u64,
                uctx.arg1() as u64,
                uctx.arg2() as u64,
                uctx.arg3() as u64,
                uctx.arg4() as u64,
                uctx.arg5() as u64,
            ],
        };
        let mut selected = SECCOMP_RET_ALLOW;
        let mut selected_precedence = action_precedence(selected);
        for filter in self.filters.iter().rev() {
            let ret = filter.execute(&data);
            let precedence = action_precedence(ret);
            if precedence > selected_precedence {
                selected = ret;
                selected_precedence = precedence;
            }
        }
        action_to_decision(selected)
    }
}

impl SeccompFilter {
    /// Validate and construct a seccomp filter from userspace BPF instructions.
    pub fn new(insns: Vec<SockFilter>) -> AxResult<Self> {
        if insns.is_empty() || insns.len() > BPF_MAXINSNS {
            return Err(AxError::InvalidInput);
        }
        Ok(Self { insns })
    }

    /// Execute this classic-BPF program and return its raw seccomp action.
    ///
    /// The interpreter intentionally rejects unsupported opcodes, out-of-range
    /// memory access, invalid jumps, and divide/modulo by zero by returning
    /// `KILL_THREAD`.  That matches seccomp's fail-closed security posture.
    fn execute(&self, data: &SeccompData) -> u32 {
        let mut a = 0u32;
        let mut x = 0u32;
        let mut mem = [0u32; BPF_MEMWORDS];
        let mut pc = 0usize;

        while pc < self.insns.len() {
            let insn = self.insns[pc];
            pc += 1;

            match insn.code & BPF_CLASS_MASK {
                BPF_LD => match (insn.code & BPF_MODE_MASK, insn.code & BPF_SIZE_MASK) {
                    (BPF_IMM, BPF_W) => a = insn.k,
                    (BPF_ABS, size) => {
                        let Some(value) = load_seccomp_data(data, insn.k, size) else {
                            return SECCOMP_RET_KILL_THREAD;
                        };
                        a = value;
                    }
                    (BPF_MEM, BPF_W) => {
                        let Some(value) = mem.get(insn.k as usize) else {
                            return SECCOMP_RET_KILL_THREAD;
                        };
                        a = *value;
                    }
                    (BPF_LEN, BPF_W) => a = core::mem::size_of::<SeccompData>() as u32,
                    _ => return SECCOMP_RET_KILL_THREAD,
                },
                BPF_LDX => match (insn.code & BPF_MODE_MASK, insn.code & BPF_SIZE_MASK) {
                    (BPF_IMM, BPF_W) => x = insn.k,
                    (BPF_MEM, BPF_W) => {
                        let Some(value) = mem.get(insn.k as usize) else {
                            return SECCOMP_RET_KILL_THREAD;
                        };
                        x = *value;
                    }
                    (BPF_LEN, BPF_W) => x = core::mem::size_of::<SeccompData>() as u32,
                    _ => return SECCOMP_RET_KILL_THREAD,
                },
                BPF_ST => {
                    let Some(slot) = mem.get_mut(insn.k as usize) else {
                        return SECCOMP_RET_KILL_THREAD;
                    };
                    *slot = a;
                }
                BPF_STX => {
                    let Some(slot) = mem.get_mut(insn.k as usize) else {
                        return SECCOMP_RET_KILL_THREAD;
                    };
                    *slot = x;
                }
                BPF_ALU => {
                    let rhs = if insn.code & BPF_SRC_MASK == BPF_X {
                        x
                    } else {
                        insn.k
                    };
                    match insn.code & BPF_OP_MASK {
                        BPF_ADD => a = a.wrapping_add(rhs),
                        BPF_SUB => a = a.wrapping_sub(rhs),
                        BPF_MUL => a = a.wrapping_mul(rhs),
                        BPF_DIV => {
                            if rhs == 0 {
                                return SECCOMP_RET_KILL_THREAD;
                            }
                            a /= rhs;
                        }
                        BPF_OR => a |= rhs,
                        BPF_AND => a &= rhs,
                        BPF_LSH => a = a.wrapping_shl(rhs),
                        BPF_RSH => a = a.wrapping_shr(rhs),
                        BPF_NEG => a = a.wrapping_neg(),
                        BPF_MOD => {
                            if rhs == 0 {
                                return SECCOMP_RET_KILL_THREAD;
                            }
                            a %= rhs;
                        }
                        BPF_XOR => a ^= rhs,
                        _ => return SECCOMP_RET_KILL_THREAD,
                    }
                }
                BPF_JMP => {
                    let next = match insn.code & BPF_OP_MASK {
                        BPF_JA => Some(pc.saturating_add(insn.k as usize)),
                        BPF_JEQ => jump_target(pc, insn, a == jump_rhs(insn, x)),
                        BPF_JGT => jump_target(pc, insn, a > jump_rhs(insn, x)),
                        BPF_JGE => jump_target(pc, insn, a >= jump_rhs(insn, x)),
                        BPF_JSET => jump_target(pc, insn, a & jump_rhs(insn, x) != 0),
                        _ => None,
                    };
                    let Some(next) = next else {
                        return SECCOMP_RET_KILL_THREAD;
                    };
                    if next > self.insns.len() {
                        return SECCOMP_RET_KILL_THREAD;
                    }
                    pc = next;
                }
                BPF_RET => {
                    return if insn.code & BPF_SRC_MASK == BPF_X {
                        x
                    } else {
                        insn.k
                    };
                }
                BPF_MISC => match insn.code & BPF_OP_MASK {
                    BPF_TAX => x = a,
                    BPF_TXA => a = x,
                    _ => return SECCOMP_RET_KILL_THREAD,
                },
                _ => return SECCOMP_RET_KILL_THREAD,
            }
        }

        SECCOMP_RET_KILL_THREAD
    }
}

/// Return the strict-mode decision for a syscall number.
fn strict_decision(sysno: usize) -> SeccompDecision {
    match Sysno::new(sysno) {
        Some(
            Sysno::read | Sysno::write | Sysno::exit | Sysno::exit_group | Sysno::rt_sigreturn,
        ) => SeccompDecision::Allow,
        _ => SeccompDecision::KillProcess,
    }
}

/// Convert a raw seccomp return value into the syscall dispatch decision.
fn action_to_decision(ret: u32) -> SeccompDecision {
    match ret & SECCOMP_RET_ACTION_FULL {
        SECCOMP_RET_ALLOW | SECCOMP_RET_LOG => SeccompDecision::Allow,
        SECCOMP_RET_ERRNO => SeccompDecision::Errno((ret & SECCOMP_RET_DATA) as u16),
        SECCOMP_RET_KILL_PROCESS => SeccompDecision::KillProcess,
        SECCOMP_RET_KILL_THREAD => SeccompDecision::KillThread,
        SECCOMP_RET_TRAP | SECCOMP_RET_TRACE => SeccompDecision::UnsupportedAction,
        _ => SeccompDecision::KillProcess,
    }
}

/// Return Linux seccomp precedence for a raw filter action.
///
/// Newer filters are evaluated first by `evaluate_filters`; when two filters
/// return the same precedence, the first selected action is kept so the newer
/// filter supplies the action data.
fn action_precedence(ret: u32) -> u8 {
    match ret & SECCOMP_RET_ACTION_FULL {
        SECCOMP_RET_KILL_PROCESS => 7,
        SECCOMP_RET_KILL_THREAD => 6,
        SECCOMP_RET_TRAP => 5,
        SECCOMP_RET_ERRNO => 4,
        SECCOMP_RET_TRACE => 3,
        SECCOMP_RET_LOG => 2,
        SECCOMP_RET_ALLOW => 1,
        _ => 7,
    }
}

/// Select the right-hand side operand for a classic-BPF jump instruction.
fn jump_rhs(insn: SockFilter, x: u32) -> u32 {
    if insn.code & BPF_SRC_MASK == BPF_X {
        x
    } else {
        insn.k
    }
}

/// Compute the next program counter for a conditional classic-BPF jump.
fn jump_target(pc: usize, insn: SockFilter, condition: bool) -> Option<usize> {
    let offset = if condition { insn.jt } else { insn.jf };
    pc.checked_add(offset as usize)
}

/// Load a field from the emulated Linux `struct seccomp_data`.
fn load_seccomp_data(data: &SeccompData, offset: u32, size: u16) -> Option<u32> {
    let value = match offset {
        0 => data.nr as u32,
        4 => data.arch,
        8 => data.instruction_pointer as u32,
        12 => (data.instruction_pointer >> 32) as u32,
        16 | 24 | 32 | 40 | 48 | 56 => {
            let index = ((offset - 16) / 8) as usize;
            data.args[index] as u32
        }
        20 | 28 | 36 | 44 | 52 | 60 => {
            let index = ((offset - 20) / 8) as usize;
            (data.args[index] >> 32) as u32
        }
        _ => return None,
    };

    match size {
        BPF_W => Some(value),
        BPF_H => Some(value & 0xffff),
        BPF_B => Some(value & 0xff),
        _ => None,
    }
}

/// Convert a `SECCOMP_RET_ERRNO` payload into the syscall return value.
pub fn seccomp_errno(errno: u16) -> usize {
    if errno == 0 {
        0
    } else {
        -(errno as i32) as usize
    }
}

#[cfg(axtest)]
pub(crate) fn seccomp_filter_rules_hold_for_test() -> bool {
    let allow = SockFilter {
        code: BPF_RET,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    };
    let errno = SockFilter {
        code: BPF_RET,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ERRNO | 13,
    };
    let data = SeccompData {
        nr: Sysno::read as i32,
        arch: AUDIT_ARCH,
        instruction_pointer: 0x1234_5678_9abc_def0,
        args: [0x1122_3344_5566_7788, 1, 2, 3, 4, 5],
    };
    let syscall_errno_filter = SeccompFilter::new(alloc::vec![
        SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_JMP | BPF_JEQ,
            jt: 0,
            jf: 1,
            k: Sysno::read as u32,
        },
        errno,
        allow,
    ]);
    let alu_filter = SeccompFilter::new(alloc::vec![
        SockFilter {
            code: BPF_LD | BPF_W | BPF_IMM,
            jt: 0,
            jf: 0,
            k: 7,
        },
        SockFilter {
            code: BPF_ST,
            jt: 0,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_LDX | BPF_W | BPF_MEM,
            jt: 0,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_ALU | BPF_ADD | BPF_X,
            jt: 0,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_ALU | BPF_MOD,
            jt: 0,
            jf: 0,
            k: 5,
        },
        SockFilter {
            code: BPF_MISC | BPF_TAX,
            jt: 0,
            jf: 0,
            k: 0,
        },
        SockFilter {
            code: BPF_RET | BPF_X,
            jt: 0,
            jf: 0,
            k: 0,
        },
    ]);
    let invalid_div_filter = SeccompFilter::new(alloc::vec![
        SockFilter {
            code: BPF_LD | BPF_W | BPF_IMM,
            jt: 0,
            jf: 0,
            k: 1,
        },
        SockFilter {
            code: BPF_ALU | BPF_DIV,
            jt: 0,
            jf: 0,
            k: 0,
        },
    ]);

    SeccompFilter::new(alloc::vec![]).is_err()
        && strict_decision(Sysno::read as usize) == SeccompDecision::Allow
        && strict_decision(Sysno::openat as usize) == SeccompDecision::KillProcess
        // Cover every strict_decision branch (exit/exit_group/rt_sigreturn allow).
        && strict_decision(Sysno::write as usize) == SeccompDecision::Allow
        && strict_decision(Sysno::exit as usize) == SeccompDecision::Allow
        && strict_decision(Sysno::exit_group as usize) == SeccompDecision::Allow
        && strict_decision(Sysno::rt_sigreturn as usize) == SeccompDecision::Allow
        // Cover every action_to_decision arm.
        && action_to_decision(SECCOMP_RET_ALLOW) == SeccompDecision::Allow
        && action_to_decision(SECCOMP_RET_LOG) == SeccompDecision::Allow
        && action_to_decision(SECCOMP_RET_ERRNO | 9) == SeccompDecision::Errno(9)
        && action_to_decision(SECCOMP_RET_KILL_PROCESS) == SeccompDecision::KillProcess
        && action_to_decision(SECCOMP_RET_KILL_THREAD) == SeccompDecision::KillThread
        && action_to_decision(SECCOMP_RET_TRAP) == SeccompDecision::UnsupportedAction
        && action_to_decision(SECCOMP_RET_TRACE) == SeccompDecision::UnsupportedAction
        && action_to_decision(0xffff_ffff) == SeccompDecision::KillProcess
        && action_precedence(SECCOMP_RET_KILL_PROCESS) > action_precedence(SECCOMP_RET_ERRNO)
        && action_precedence(SECCOMP_RET_KILL_THREAD) > action_precedence(SECCOMP_RET_TRAP)
        && action_precedence(SECCOMP_RET_TRAP) > action_precedence(SECCOMP_RET_ERRNO)
        && action_precedence(SECCOMP_RET_ERRNO) > action_precedence(SECCOMP_RET_TRACE)
        && action_precedence(SECCOMP_RET_TRACE) > action_precedence(SECCOMP_RET_LOG)
        && action_precedence(SECCOMP_RET_LOG) > action_precedence(SECCOMP_RET_ALLOW)
        // Cover jump_rhs both BPF_X (use register) and non-BPF_X (use k) arms.
        && jump_rhs(
            SockFilter {
                code: BPF_JMP | BPF_JEQ | BPF_X,
                k: 99,
                ..allow
            },
            7,
        ) == 7
        && jump_rhs(
            SockFilter {
                code: BPF_JMP | BPF_JEQ,
                k: 99,
                ..allow
            },
            7,
        ) == 99
        && jump_target(
            3,
            SockFilter {
                jt: 2,
                jf: 4,
                ..allow
            },
            true,
        ) == Some(5)
        && jump_target(
            3,
            SockFilter {
                jt: 2,
                jf: 4,
                ..allow
            },
            false,
        ) == Some(7)
        && load_seccomp_data(&data, 0, BPF_W) == Some(Sysno::read as u32)
        && load_seccomp_data(&data, 4, BPF_W) == Some(AUDIT_ARCH)
        && load_seccomp_data(&data, 8, BPF_W) == Some(0x9abc_def0)
        && load_seccomp_data(&data, 12, BPF_W) == Some(0x1234_5678)
        && load_seccomp_data(&data, 16, BPF_W) == Some(0x5566_7788)
        && load_seccomp_data(&data, 20, BPF_W) == Some(0x1122_3344)
        && load_seccomp_data(&data, 24, BPF_W) == Some(1)
        && load_seccomp_data(&data, 48, BPF_W) == Some(4)
        && load_seccomp_data(&data, 52, BPF_W) == Some(0)
        && load_seccomp_data(&data, 16, BPF_H) == Some(0x7788)
        && load_seccomp_data(&data, 16, BPF_B) == Some(0x88)
        // Unknown size code (none of BPF_W=0, BPF_H=0x08, BPF_B=0x10) returns None.
        && load_seccomp_data(&data, 0, 0x20).is_none()
        // Unknown offset returns None.
        && load_seccomp_data(&data, 64, BPF_W).is_none()
        && syscall_errno_filter
            .as_ref()
            .is_ok_and(|filter| filter.execute(&data) == (SECCOMP_RET_ERRNO | 13))
        && alu_filter
            .as_ref()
            .is_ok_and(|filter| filter.execute(&data) == 4)
        && invalid_div_filter
            .as_ref()
            .is_ok_and(|filter| filter.execute(&data) == SECCOMP_RET_KILL_THREAD)
        && seccomp_errno(0) == 0
        && seccomp_errno(13) == (-13i32 as usize)
}

#[cfg(axtest)]
pub(crate) fn seccomp_filter_construction_rules_hold_for_test() -> bool {
    use alloc::vec;

    // Empty instruction list is rejected.
    SeccompFilter::new(vec![]).is_err()
        // A single return-instruction filter is accepted.
        && SeccompFilter::new(vec![SockFilter {
            code: BPF_RET,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        }])
        .is_ok()
        // Exactly BPF_MAXINSNS instructions is the boundary and is accepted.
        && SeccompFilter::new(vec![
            SockFilter {
                code: BPF_RET,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ALLOW,
            };
            BPF_MAXINSNS
        ])
        .is_ok()
        // One instruction above BPF_MAXINSNS is rejected.
        && SeccompFilter::new(vec![
            SockFilter {
                code: BPF_RET,
                jt: 0,
                jf: 0,
                k: SECCOMP_RET_ALLOW,
            };
            BPF_MAXINSNS + 1
        ])
        .is_err()
}
