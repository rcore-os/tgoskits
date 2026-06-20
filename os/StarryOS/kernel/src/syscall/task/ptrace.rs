use alloc::{sync::Arc, vec, vec::Vec};
use core::mem::{MaybeUninit, size_of};
#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
use core::slice;

use ax_errno::{AxError, AxResult, LinuxError};
#[cfg(target_arch = "x86_64")]
use ax_memory_addr::PAGE_SIZE_4K;
use ax_memory_addr::{MemoryAddr, VirtAddr};
use ax_runtime::hal::paging::MappingFlags;
use ax_task::current;
use starry_process::Pid;
use starry_signal::Signo;
use starry_vm::{VmMutPtr, VmPtr, vm_read_slice, vm_write_slice};

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
))]
use crate::task::PtraceStopFpData;
#[cfg(target_arch = "x86_64")]
use crate::task::PtraceStopFpData;
use crate::{
    mm::{AddrSpace, IoVec},
    task::{AsThread, Cred, ProcessData, get_process_cred, get_process_data, get_task},
};

const PTRACE_TRACEME: u32 = 0;
const PTRACE_PEEKTEXT: u32 = 1;
const PTRACE_PEEKDATA: u32 = 2;
const PTRACE_POKETEXT: u32 = 4;
const PTRACE_POKEDATA: u32 = 5;
const PTRACE_CONT: u32 = 7;
const PTRACE_KILL: u32 = 8;
const PTRACE_SINGLESTEP: u32 = 9;
const PTRACE_PEEKUSER: u32 = 3;
const PTRACE_POKEUSER: u32 = 6;
const PTRACE_GETREGS: u32 = 12;
const PTRACE_SETREGS: u32 = 13;
const PTRACE_GETFPREGS: u32 = 14;
const PTRACE_SETFPREGS: u32 = 15;
const PTRACE_GETFPXREGS: u32 = 18;
const PTRACE_SETFPXREGS: u32 = 19;
const PTRACE_ATTACH: u32 = 16;
const PTRACE_DETACH: u32 = 17;
const PTRACE_SYSCALL: u32 = 24;
const PTRACE_SETOPTIONS: u32 = 0x4200;
const PTRACE_GETEVENTMSG: u32 = 0x4201;
const PTRACE_GETSIGINFO: u32 = 0x4202;
const PTRACE_SETSIGINFO: u32 = 0x4203;
const PTRACE_GETREGSET: u32 = 0x4204;
const PTRACE_SETREGSET: u32 = 0x4205;
const PTRACE_SEIZE: u32 = 0x4206;
const PTRACE_INTERRUPT: u32 = 0x4207;

const NT_PRSTATUS: usize = 1;
#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
))]
const NT_FPREGSET: usize = 2;
#[cfg(target_arch = "x86_64")]
const NT_FPREGSET: usize = 2;

const PTRACE_O_TRACESYSGOOD: usize = 1;
const PTRACE_O_TRACEFORK: usize = 1 << 1;
const PTRACE_O_TRACEVFORK: usize = 1 << 2;
const PTRACE_O_TRACECLONE: usize = 1 << 3;
const PTRACE_O_TRACEEXEC: usize = 1 << 4;
const PTRACE_O_TRACEVFORKDONE: usize = 1 << 5;
const PTRACE_O_TRACEEXIT: usize = 1 << 6;

pub const PTRACE_EVENT_FORK: u32 = 1;
pub const PTRACE_EVENT_VFORK: u32 = 2;
pub const PTRACE_EVENT_CLONE: u32 = 3;
const PTRACE_EVENT_EXEC: u32 = 4;
pub const PTRACE_EVENT_VFORK_DONE: u32 = 5;
const PTRACE_EVENT_EXIT: u32 = 6;

#[cfg(target_arch = "riscv64")]
const EBREAK_INSN: u16 = 0x9002;
#[cfg(target_arch = "aarch64")]
const AARCH64_BRK_INSN: u32 = 0xd4200000;
#[cfg(target_arch = "loongarch64")]
const LOONGARCH_BREAK_INSN: u32 = 0x002a0000;

#[cfg(target_arch = "riscv64")]
type ArchUserRegs = RiscvUserRegs;
#[cfg(target_arch = "aarch64")]
type ArchUserRegs = Aarch64UserRegs;
#[cfg(target_arch = "loongarch64")]
type ArchUserRegs = LoongarchUserRegs;
#[cfg(target_arch = "x86_64")]
type ArchUserRegs = X8664UserRegs;
#[cfg(target_arch = "riscv64")]
type ArchFpRegs = RiscvFpRegs;
#[cfg(target_arch = "aarch64")]
type ArchFpRegs = Aarch64FpRegs;
#[cfg(target_arch = "loongarch64")]
type ArchFpRegs = LoongarchFpRegs;
#[cfg(target_arch = "x86_64")]
type ArchFpRegs = X8664FpRegs;

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct RiscvUserRegs {
    pc: usize,
    ra: usize,
    sp: usize,
    gp: usize,
    tp: usize,
    t0: usize,
    t1: usize,
    t2: usize,
    s0: usize,
    s1: usize,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
    a7: usize,
    s2: usize,
    s3: usize,
    s4: usize,
    s5: usize,
    s6: usize,
    s7: usize,
    s8: usize,
    s9: usize,
    s10: usize,
    s11: usize,
    t3: usize,
    t4: usize,
    t5: usize,
    t6: usize,
}

#[cfg(target_arch = "riscv64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct RiscvFpRegs {
    f: [u64; 32],
    fcsr: usize,
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Aarch64FpRegs {
    vregs: [u128; 32],
    fpsr: u32,
    fpcr: u32,
    __reserved: [u32; 2],
}

#[cfg(target_arch = "aarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct Aarch64UserRegs {
    regs: [u64; 31],
    sp: u64,
    pc: u64,
    pstate: u64,
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct LoongarchUserRegs {
    regs: [u64; 32],
    orig_a0: u64,
    csr_era: u64,
    csr_badv: u64,
    reserved: [u64; 10],
}

#[cfg(target_arch = "loongarch64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct LoongarchFpRegs {
    fpr: [u64; 32],
    fcc: u64,
    fcsr: u32,
}

/// Linux `user_regs_struct` for x86_64 (`arch/x86/include/uapi/asm/user.h`).
#[cfg(target_arch = "x86_64")]
#[repr(C)]
#[derive(Clone, Copy)]
struct X8664UserRegs {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbp: u64,
    rbx: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rax: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    orig_rax: u64,
    rip: u64,
    cs: u64,
    eflags: u64,
    rsp: u64,
    ss: u64,
    fs_base: u64,
    gs_base: u64,
    ds: u64,
    es: u64,
    fs: u64,
    gs: u64,
}

#[cfg(target_arch = "x86_64")]
#[repr(transparent)]
#[derive(Clone, Copy)]
struct X8664FpRegs(ax_cpu::FxsaveArea);

pub fn sys_ptrace(request: u32, pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    info!("sys_ptrace <= request: {request}, pid: {pid}, addr: {addr:#x}, data: {data:#x}");

    match request {
        PTRACE_TRACEME => ptrace_traceme(),
        PTRACE_PEEKTEXT | PTRACE_PEEKDATA => ptrace_peekdata(pid, addr, data),
        PTRACE_POKETEXT | PTRACE_POKEDATA => ptrace_pokedata(pid, addr, data),
        PTRACE_CONT => ptrace_cont(pid, data),
        PTRACE_KILL => ptrace_kill(pid),
        PTRACE_SINGLESTEP => ptrace_singlestep(pid, data),
        PTRACE_GETREGS => ptrace_getregs(pid, data),
        PTRACE_SETREGS => ptrace_setregs(pid, data),
        PTRACE_GETFPREGS => ptrace_getfpregs(pid, data),
        PTRACE_SETFPREGS => ptrace_setfpregs(pid, data),
        PTRACE_GETFPXREGS => ptrace_getfpregs(pid, data),
        PTRACE_SETFPXREGS => ptrace_setfpregs(pid, data),
        PTRACE_PEEKUSER => ptrace_peekuser(pid, addr, data),
        PTRACE_POKEUSER => ptrace_pokeuser(pid, addr, data),
        PTRACE_ATTACH => ptrace_attach(pid),
        PTRACE_DETACH => ptrace_detach(pid, data),
        PTRACE_SYSCALL => ptrace_syscall(pid, data),
        PTRACE_SETOPTIONS => ptrace_setoptions(pid, data),
        PTRACE_GETEVENTMSG => ptrace_geteventmsg(pid, data),
        PTRACE_GETSIGINFO => ptrace_getsiginfo(pid, data),
        PTRACE_SETSIGINFO => ptrace_setsiginfo(pid, data),
        PTRACE_GETREGSET => ptrace_getregset(pid, addr, data),
        PTRACE_SETREGSET => ptrace_setregset(pid, addr, data),
        PTRACE_SEIZE => ptrace_seize(pid, addr),
        PTRACE_INTERRUPT => ptrace_interrupt(pid),
        _ => Err(AxError::Unsupported),
    }
}

fn ptrace_traceme() -> AxResult<isize> {
    let curr = current();
    let proc_data = &curr.as_thread().proc_data;
    if proc_data.proc.parent().is_none()
        || proc_data.is_ptrace_traceme()
        || proc_data.is_ptrace_attached()
        || proc_data.ptrace_tracer_pid().is_some()
    {
        return Err(AxError::from(LinuxError::EPERM));
    }
    proc_data.set_ptrace_traceme();
    Ok(0)
}

fn ptrace_resume_signo(data: usize) -> AxResult<u32> {
    if data == 0 {
        return Ok(0);
    }
    let signo = u8::try_from(data).map_err(|_| AxError::from(LinuxError::EIO))?;
    Signo::from_repr(signo).ok_or_else(|| AxError::from(LinuxError::EIO))?;
    Ok(signo as u32)
}

fn ptrace_cont(pid: usize, data: usize) -> AxResult<isize> {
    let signo = ptrace_resume_signo(data)?;
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    tracee.set_ptrace_singlestep_for(tid, false);
    tracee.set_ptrace_syscall_trace_for(tid, false);
    tracee.resume_ptrace_stop_with_signal_for(tid, signo);
    ax_task::yield_now();
    Ok(0)
}

fn ptrace_kill(pid: usize) -> AxResult<isize> {
    let tracee_pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    let tracee = get_process_data(tracee_pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if tracee.is_ptrace_traceme() || tracee.is_ptrace_attached() {
        tracee.clear_ptrace_stop();
        tracee.clear_ptrace_traceme();
        tracee.clear_ptrace_attached();
    }
    use starry_signal::SignalInfo;

    use crate::task::send_signal_to_process;
    let _ = send_signal_to_process(tracee_pid, Some(SignalInfo::new_kernel(Signo::SIGKILL)));
    Ok(0)
}

fn ptrace_singlestep(pid: usize, data: usize) -> AxResult<isize> {
    let signo = ptrace_resume_signo(data)?;
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    tracee.set_ptrace_singlestep_for(tid, true);
    tracee.set_ptrace_syscall_trace_for(tid, false);
    tracee.resume_ptrace_stop_with_signal_for(tid, signo);
    Ok(0)
}

fn ptrace_attach(pid: usize) -> AxResult<isize> {
    let tracer_pid = current().as_thread().proc_data.proc.pid();
    let tracee_pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if tracee_pid == tracer_pid {
        return Err(AxError::from(LinuxError::EPERM));
    }
    let tracee = get_process_data(tracee_pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if tracee.is_ptrace_traceme() || tracee.is_ptrace_attached() {
        return Err(AxError::from(LinuxError::EPERM));
    }
    if !ptrace_may_attach(tracer_pid, tracee_pid, &tracee)? {
        return Err(AxError::from(LinuxError::EPERM));
    }
    tracee.set_ptrace_tracer_pid(tracer_pid);
    tracee.set_ptrace_attached();
    use starry_signal::SignalInfo;
    let _ = crate::task::send_signal_to_process(
        tracee_pid,
        Some(SignalInfo::new_kernel(Signo::SIGSTOP)),
    );
    Ok(0)
}

fn ptrace_detach(pid: usize, data: usize) -> AxResult<isize> {
    let signo = ptrace_resume_signo(data)?;
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    tracee.clear_ptrace_traceme();
    tracee.clear_ptrace_attached();
    tracee.clear_ptrace_tracer_pid();
    tracee.set_ptrace_singlestep_for(tid, false);
    tracee.set_ptrace_syscall_trace_for(tid, false);
    tracee.set_ptrace_options(0);
    tracee.resume_ptrace_stop_with_signal_for(tid, signo);
    Ok(0)
}

fn ptrace_syscall(pid: usize, data: usize) -> AxResult<isize> {
    let signo = ptrace_resume_signo(data)?;
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    tracee.set_ptrace_singlestep_for(tid, false);
    tracee.set_ptrace_syscall_trace_for(tid, true);
    tracee.resume_ptrace_stop_with_signal_for(tid, signo);
    Ok(0)
}

fn ptrace_setoptions(pid: usize, options: usize) -> AxResult<isize> {
    let tracee = ptrace_stopped_tracee(pid)?;
    let valid_mask = PTRACE_O_TRACESYSGOOD
        | PTRACE_O_TRACEFORK
        | PTRACE_O_TRACEVFORK
        | PTRACE_O_TRACECLONE
        | PTRACE_O_TRACEEXEC
        | PTRACE_O_TRACEVFORKDONE
        | PTRACE_O_TRACEEXIT;
    if options & !valid_mask != 0 {
        return Err(AxError::InvalidInput);
    }
    tracee.set_ptrace_options(options);
    Ok(0)
}

fn ptrace_geteventmsg(pid: usize, data: usize) -> AxResult<isize> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let msg = tracee.ptrace_event_msg_for(tid);
    (data as *mut usize).vm_write(msg)?;
    Ok(0)
}

fn ptrace_getsiginfo(pid: usize, data: usize) -> AxResult<isize> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let siginfo = tracee
        .ptrace_stop_siginfo_for(tid)
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;

    #[cfg(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    ))]
    {
        let bytes = unsafe {
            slice::from_raw_parts(
                (&siginfo.0 as *const linux_raw_sys::general::siginfo_t).cast::<u8>(),
                size_of::<starry_signal::SignalInfo>(),
            )
        };
        vm_write_slice(data as *mut u8, bytes)?;
        Ok(0)
    }

    #[cfg(not(any(
        target_arch = "riscv64",
        target_arch = "aarch64",
        target_arch = "loongarch64",
        target_arch = "x86_64"
    )))]
    {
        let _ = (data, siginfo);
        Err(AxError::Unsupported)
    }
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_setsiginfo(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let siginfo = ptrace_read_user_siginfo(data)?;
    let signo = ptrace_siginfo_signo(&siginfo)?;
    if !tracee.set_ptrace_stop_siginfo_for(tid, signo, starry_signal::SignalInfo(siginfo)) {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    Ok(0)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_setsiginfo(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

fn ptrace_getregset(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    match addr {
        NT_PRSTATUS => ptrace_getregset_prstatus(pid, data),
        #[cfg(any(
            target_arch = "riscv64",
            target_arch = "aarch64",
            target_arch = "loongarch64",
            target_arch = "x86_64"
        ))]
        NT_FPREGSET => ptrace_getregset_fpregset(pid, data),
        _ => Err(AxError::Unsupported),
    }
}

fn ptrace_setregset(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    match addr {
        NT_PRSTATUS => ptrace_setregset_prstatus(pid, data),
        #[cfg(any(
            target_arch = "riscv64",
            target_arch = "aarch64",
            target_arch = "loongarch64",
            target_arch = "x86_64"
        ))]
        NT_FPREGSET => ptrace_setregset_fpregset(pid, data),
        _ => Err(AxError::Unsupported),
    }
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_getregs(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_stopped_user_regs(pid)?;
    let bytes = unsafe {
        slice::from_raw_parts(
            (&regs as *const ArchUserRegs).cast::<u8>(),
            size_of::<ArchUserRegs>(),
        )
    };
    vm_write_slice(data as *mut u8, bytes)?;
    Ok(0)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_getregs(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_setregs(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_user_regs(data)?;
    ptrace_write_stopped_user_regs(pid, regs)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_setregs(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_getfpregs(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_stopped_fp_regs(pid)?;
    let bytes = unsafe {
        slice::from_raw_parts(
            (&regs as *const ArchFpRegs).cast::<u8>(),
            size_of::<ArchFpRegs>(),
        )
    };
    vm_write_slice(data as *mut u8, bytes)?;
    Ok(0)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_getfpregs(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_setfpregs(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_user_fpregs(data)?;
    ptrace_write_stopped_fp_regs(pid, regs)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_setfpregs(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

fn ptrace_seize(pid: usize, _addr: usize) -> AxResult<isize> {
    let tracer_pid = current().as_thread().proc_data.proc.pid();
    let tracee_pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if tracee_pid == tracer_pid {
        return Err(AxError::from(LinuxError::EIO));
    }
    let tracee = get_process_data(tracee_pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if tracee.is_ptrace_traceme() || tracee.is_ptrace_attached() {
        return Err(AxError::from(LinuxError::EPERM));
    }
    if !ptrace_may_attach(tracer_pid, tracee_pid, &tracee)? {
        return Err(AxError::from(LinuxError::EPERM));
    }
    tracee.set_ptrace_tracer_pid(tracer_pid);
    tracee.set_ptrace_attached();
    Ok(0)
}

fn ptrace_may_attach(tracer_pid: Pid, tracee_pid: Pid, tracee: &ProcessData) -> AxResult<bool> {
    if tracee.proc.parent().is_some_and(|p| p.pid() == tracer_pid) {
        return Ok(true);
    }

    let tracer_cred = get_process_cred(tracer_pid)?;
    if tracer_cred.has_cap_sys_ptrace() {
        return Ok(true);
    }

    if tracee.dumpable() != 1 {
        return Ok(false);
    }

    let tracee_cred = get_process_cred(tracee_pid)?;
    Ok(ptrace_creds_match_for_attach(&tracer_cred, &tracee_cred))
}

fn ptrace_creds_match_for_attach(tracer: &Cred, tracee: &Cred) -> bool {
    tracer.uid == tracee.uid
        && tracer.euid == tracee.euid
        && tracer.suid == tracee.suid
        && tracer.fsuid == tracee.fsuid
        && tracer.uid == tracer.euid
        && tracer.uid == tracer.suid
        && tracer.uid == tracer.fsuid
}

fn ptrace_interrupt(pid: usize) -> AxResult<isize> {
    let tracee_pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    let tracee = get_process_data(tracee_pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    if !tracee.is_ptrace_attached() && !tracee.is_ptrace_traceme() {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    if tracee.ptrace_stop_signo().is_some() {
        return Ok(0);
    }
    use starry_signal::SignalInfo;
    let _ = crate::task::send_signal_to_process(
        tracee_pid,
        Some(SignalInfo::new_kernel(Signo::SIGSTOP)),
    );
    Ok(0)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_getregset_prstatus(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_stopped_user_regs(pid)?;
    let reg_bytes = unsafe {
        slice::from_raw_parts(
            (&regs as *const ArchUserRegs).cast::<u8>(),
            size_of::<ArchUserRegs>(),
        )
    };

    let mut iov = (data as *const IoVec).vm_read()?;
    if iov.iov_len < 0 {
        return Err(AxError::InvalidInput);
    }

    let copy_len = (iov.iov_len as usize).min(reg_bytes.len());
    vm_write_slice(iov.iov_base, &reg_bytes[..copy_len])?;
    iov.iov_len = copy_len as isize;
    (data as *mut IoVec).vm_write(iov)?;
    Ok(0)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_getregset_prstatus(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_setregset_prstatus(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }

    let reg_size = size_of::<ArchUserRegs>() as isize;

    let iov = (data as *const IoVec).vm_read()?;
    if iov.iov_len < reg_size {
        return Err(AxError::InvalidInput);
    }

    let regs = ptrace_read_user_regs(iov.iov_base as usize)?;
    ptrace_write_stopped_user_regs(pid, regs)
}

#[cfg(not(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
)))]
fn ptrace_setregset_prstatus(pid: usize, data: usize) -> AxResult<isize> {
    let _ = (pid, data);
    Err(AxError::Unsupported)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_read_stopped_user_regs(pid: usize) -> AxResult<ArchUserRegs> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let uctx = tracee
        .ptrace_stop_user_context_for(tid)
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
    Ok(ArchUserRegs::from(&uctx))
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_read_user_regs(data: usize) -> AxResult<ArchUserRegs> {
    let mut regs = MaybeUninit::<ArchUserRegs>::uninit();
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            regs.as_mut_ptr().cast::<MaybeUninit<u8>>(),
            size_of::<ArchUserRegs>(),
        )
    };
    starry_vm::vm_read_slice(data as *const u8, bytes)?;
    Ok(unsafe { regs.assume_init() })
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_write_stopped_user_regs(pid: usize, regs: ArchUserRegs) -> AxResult<isize> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let mut uctx = tracee
        .ptrace_stop_user_context_for(tid)
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
    regs.write_to(&mut uctx)?;
    if !tracee.set_ptrace_stop_user_context_for(tid, uctx) {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    Ok(0)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_getregset_fpregset(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_stopped_fp_regs(pid)?;
    let mut iov = (data as *const IoVec).vm_read()?;
    if iov.iov_len < 0 {
        return Err(AxError::InvalidInput);
    }
    let bytes = unsafe {
        slice::from_raw_parts(
            (&regs as *const ArchFpRegs).cast::<u8>(),
            size_of::<ArchFpRegs>(),
        )
    };
    let copy_len = (iov.iov_len as usize).min(bytes.len());
    vm_write_slice(iov.iov_base, &bytes[..copy_len])?;
    iov.iov_len = copy_len as isize;
    (data as *mut IoVec).vm_write(iov)?;
    Ok(0)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_setregset_fpregset(pid: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let iov = (data as *const IoVec).vm_read()?;
    if iov.iov_len < size_of::<ArchFpRegs>() as isize {
        return Err(AxError::InvalidInput);
    }
    let regs = ptrace_read_user_fpregs(iov.iov_base as usize)?;
    ptrace_write_stopped_fp_regs(pid, regs)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_read_stopped_fp_regs(pid: usize) -> AxResult<ArchFpRegs> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let fp_data = tracee
        .ptrace_stop_fp_data_for(tid)
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
    Ok(ArchFpRegs::from(fp_data))
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_read_user_fpregs(data: usize) -> AxResult<ArchFpRegs> {
    let mut regs = MaybeUninit::<ArchFpRegs>::uninit();
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            regs.as_mut_ptr().cast::<MaybeUninit<u8>>(),
            size_of::<ArchFpRegs>(),
        )
    };
    starry_vm::vm_read_slice(data as *const u8, bytes)?;
    Ok(unsafe { regs.assume_init() })
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_read_user_siginfo(data: usize) -> AxResult<linux_raw_sys::general::siginfo_t> {
    let mut siginfo = MaybeUninit::<linux_raw_sys::general::siginfo_t>::uninit();
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            siginfo.as_mut_ptr().cast::<MaybeUninit<u8>>(),
            size_of::<linux_raw_sys::general::siginfo_t>(),
        )
    };
    starry_vm::vm_read_slice(data as *const u8, bytes)?;
    Ok(unsafe { siginfo.assume_init() })
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_siginfo_signo(siginfo: &linux_raw_sys::general::siginfo_t) -> AxResult<Signo> {
    let signo = unsafe { siginfo.__bindgen_anon_1.__bindgen_anon_1.si_signo };
    Signo::from_repr(signo as u8).ok_or(AxError::InvalidInput)
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64",
    target_arch = "x86_64"
))]
fn ptrace_write_stopped_fp_regs(pid: usize, regs: ArchFpRegs) -> AxResult<isize> {
    let (tracee, tid) = ptrace_stopped_tracee_with_tid(pid)?;
    #[cfg(target_arch = "loongarch64")]
    let fp_data = {
        let mut fp_data = tracee
            .ptrace_stop_fp_data_for(tid)
            .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
        fp_data.regs = regs.fpr;
        fp_data.fcc = loongarch_unpack_fcc(regs.fcc);
        fp_data.fcsr = regs.fcsr;
        fp_data
    };
    #[cfg(not(target_arch = "loongarch64"))]
    let fp_data = PtraceStopFpData::from(regs);

    if !tracee.set_ptrace_stop_fp_data_for(tid, fp_data) {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    Ok(0)
}

fn ptrace_peekdata(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let tracee = ptrace_stopped_tracee(pid)?;
    (data as *mut usize).vm_write(ptrace_read_word(&tracee, addr)?)?;
    Ok(0)
}

fn ptrace_pokedata(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    let tracee = ptrace_stopped_tracee(pid)?;
    ptrace_write_word(&tracee, addr, data)?;
    Ok(0)
}

fn ptrace_read_word(tracee: &ProcessData, addr: usize) -> AxResult<usize> {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    ptrace_populate_remote_range(&mut aspace, addr, size_of::<usize>(), MappingFlags::READ)?;
    let mut bytes = [0u8; size_of::<usize>()];
    aspace.read(VirtAddr::from_usize(addr), &mut bytes)?;
    Ok(usize::from_ne_bytes(bytes))
}

fn ptrace_write_word(tracee: &ProcessData, addr: usize, data: usize) -> AxResult {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    ptrace_populate_remote_range(&mut aspace, addr, size_of::<usize>(), MappingFlags::WRITE)?;
    aspace.write(VirtAddr::from_usize(addr), &data.to_ne_bytes())?;
    ax_runtime::hal::cpu::asm::flush_icache_all();
    Ok(())
}

fn ptrace_populate_remote_range(
    aspace: &mut AddrSpace,
    addr: usize,
    len: usize,
    access_flags: MappingFlags,
) -> AxResult {
    let start = VirtAddr::from_usize(addr);
    let end = VirtAddr::from_usize(addr.checked_add(len).ok_or(AxError::BadAddress)?);
    let page_start = start.align_down_4k();
    let page_end = end.align_up_4k();
    aspace.populate_area(page_start, page_end - page_start, access_flags)
}

pub fn sys_process_vm_readv(
    pid: usize,
    local_iov: *const IoVec,
    liovcnt: usize,
    remote_iov: *const IoVec,
    riovcnt: usize,
    flags: usize,
) -> AxResult<isize> {
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    process_vm_copy(pid, local_iov, liovcnt, remote_iov, riovcnt, false)
}

pub fn sys_process_vm_writev(
    pid: usize,
    local_iov: *const IoVec,
    liovcnt: usize,
    remote_iov: *const IoVec,
    riovcnt: usize,
    flags: usize,
) -> AxResult<isize> {
    if flags != 0 {
        return Err(AxError::InvalidInput);
    }

    process_vm_copy(pid, local_iov, liovcnt, remote_iov, riovcnt, true)
}

fn process_vm_copy(
    pid: usize,
    local_iov: *const IoVec,
    liovcnt: usize,
    remote_iov: *const IoVec,
    riovcnt: usize,
    write_remote: bool,
) -> AxResult<isize> {
    let tracee = process_vm_tracee(pid)?;
    let local = read_iovecs(local_iov, liovcnt)?;
    let remote = read_iovecs(remote_iov, riovcnt)?;

    let mut local_idx = 0;
    let mut remote_idx = 0;
    let mut local_off = 0;
    let mut remote_off = 0;
    let mut copied = 0usize;

    while local_idx < local.len() && remote_idx < remote.len() {
        skip_empty_iovecs(&local, &mut local_idx, &mut local_off);
        skip_empty_iovecs(&remote, &mut remote_idx, &mut remote_off);
        if local_idx >= local.len() || remote_idx >= remote.len() {
            break;
        }

        let local_len = local[local_idx].iov_len as usize - local_off;
        let remote_len = remote[remote_idx].iov_len as usize - remote_off;
        let chunk_len = local_len.min(remote_len);
        if chunk_len == 0 {
            break;
        }

        let local_addr = local[local_idx].iov_base.wrapping_add(local_off);
        let remote_addr = (remote[remote_idx].iov_base as usize)
            .checked_add(remote_off)
            .ok_or(AxError::BadAddress)?;
        let result: AxResult<()> = if write_remote {
            let mut data = vec![0; chunk_len];
            let bytes = unsafe {
                core::slice::from_raw_parts_mut(
                    data.as_mut_ptr().cast::<MaybeUninit<u8>>(),
                    data.len(),
                )
            };
            vm_read_slice(local_addr, bytes)?;
            remote_write(&tracee, remote_addr, &data)
        } else {
            let data = remote_read(&tracee, remote_addr, chunk_len)?;
            vm_write_slice(local_addr, &data)?;
            Ok(())
        };

        if let Err(err) = result {
            return if copied == 0 {
                Err(err)
            } else {
                Ok(copied as isize)
            };
        }

        copied = copied.checked_add(chunk_len).ok_or(AxError::InvalidInput)?;
        local_off += chunk_len;
        remote_off += chunk_len;
    }

    Ok(copied as isize)
}

fn process_vm_tracee(pid: usize) -> AxResult<Arc<ProcessData>> {
    let tracee_pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    let current_pid = current().as_thread().proc_data.proc.pid();
    if tracee_pid == current_pid {
        get_process_data(tracee_pid).map_err(|_| AxError::from(LinuxError::ESRCH))
    } else {
        ptrace_stopped_tracee(pid)
    }
}

fn ptrace_tracee_by_pid_or_tid(pid: Pid) -> AxResult<Arc<ProcessData>> {
    get_process_data(pid)
        .or_else(|_| get_task(pid).map(|task| task.as_thread().proc_data.clone()))
        .map_err(|_| AxError::from(LinuxError::ESRCH))
}

fn read_iovecs(iov: *const IoVec, iovcnt: usize) -> AxResult<Vec<IoVec>> {
    if iovcnt > 1024 {
        return Err(AxError::InvalidInput);
    }

    let mut iovecs = Vec::with_capacity(iovcnt);
    let mut total = 0usize;
    for idx in 0..iovcnt {
        let iov = iov.wrapping_add(idx).vm_read()?;
        if iov.iov_len < 0 {
            return Err(AxError::InvalidInput);
        }
        total = total
            .checked_add(iov.iov_len as usize)
            .filter(|len| *len <= isize::MAX as usize)
            .ok_or(AxError::InvalidInput)?;
        iovecs.push(iov);
    }
    Ok(iovecs)
}

fn skip_empty_iovecs(iovecs: &[IoVec], idx: &mut usize, offset: &mut usize) {
    while *idx < iovecs.len() && *offset >= iovecs[*idx].iov_len as usize {
        *idx += 1;
        *offset = 0;
    }
}

fn remote_read(tracee: &ProcessData, addr: usize, len: usize) -> AxResult<Vec<u8>> {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    ptrace_populate_remote_range(&mut aspace, addr, len, MappingFlags::READ)?;
    let mut data = vec![0; len];
    aspace.read(VirtAddr::from_usize(addr), &mut data)?;
    Ok(data)
}

fn remote_write(tracee: &ProcessData, addr: usize, data: &[u8]) -> AxResult {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    ptrace_populate_remote_range(&mut aspace, addr, data.len(), MappingFlags::WRITE)?;
    aspace.write(VirtAddr::from_usize(addr), data)?;
    ax_runtime::hal::cpu::asm::flush_icache_all();
    Ok(())
}

fn ptrace_stopped_tracee(pid: usize) -> AxResult<Arc<ProcessData>> {
    ptrace_stopped_tracee_with_tid(pid).map(|(tracee, _tid)| tracee)
}

fn ptrace_stopped_tracee_with_tid(pid: usize) -> AxResult<(Arc<ProcessData>, u32)> {
    let pid = Pid::try_from(pid).map_err(|_| AxError::from(LinuxError::ESRCH))?;
    let tracer_pid = current().as_thread().proc_data.proc.pid();
    let tracee = ptrace_tracee_by_pid_or_tid(pid)?;
    let is_tracer = (tracee.is_ptrace_traceme() || tracee.is_ptrace_attached())
        && tracee
            .ptrace_tracer_pid()
            .is_some_and(|pid| pid == tracer_pid);
    if !is_tracer || tracee.ptrace_stop_signo().is_none() {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    if pid == tracee.proc.pid() {
        if tracee.ptrace_stop_signo_for(pid).is_some() {
            tracee.select_ptrace_stop(pid);
        }
    } else if !tracee.select_ptrace_stop(pid) {
        return Err(AxError::from(LinuxError::ESRCH));
    }
    let tid = tracee
        .selected_ptrace_stop_tid()
        .ok_or_else(|| AxError::from(LinuxError::ESRCH))?;
    Ok((tracee, tid))
}

#[cfg(target_arch = "x86_64")]
pub fn ptrace_setup_singlestep(
    _tracee: &ProcessData,
    _tid: Pid,
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
) {
    // Set Trap Flag (TF, bit 8) in RFLAGS.
    // The CPU will generate a #DB debug exception after executing one
    // instruction. The CPU clears TF in the active RFLAGS register but
    // preserves TF=1 in the RFLAGS saved on the exception stack frame
    // (Intel SDM Vol 3A §17.3.2). The Debug handler in user.rs must
    // clear TF in the saved frame to avoid tainting GDB's single-step
    // sequence (e.g. ret_to_nx probe → wrong i386 arch detection).
    uctx.rflags |= 1 << 8;
}

#[cfg(target_arch = "riscv64")]
pub fn ptrace_setup_singlestep(
    tracee: &ProcessData,
    tid: Pid,
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
) {
    let pc = uctx.ip();
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();

    let saved = tracee.take_ptrace_ss_saved_insn_for(tid);
    if let Some((saved_addr, saved_insn)) = saved {
        let _ = ptrace_write_u16_unlocked(&mut aspace, saved_addr, saved_insn as u16);
    }

    let first_half = match ptrace_read_u16_unlocked(&aspace, pc) {
        Ok(half) => half,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    let insn_len = riscv_insn_len(first_half);
    let current_insn = if insn_len == 2 {
        first_half as u32
    } else {
        match ptrace_read_u32_unlocked(&aspace, pc) {
            Ok(word) => word,
            Err(_) => {
                tracee.set_ptrace_ss_saved_insn_for(tid, None);
                return;
            }
        }
    };
    let next_insn_addr = riscv_next_pc(current_insn, insn_len, pc, uctx);
    if next_insn_addr == pc {
        tracee.set_ptrace_ss_saved_insn_for(tid, None);
        return;
    }
    let orig_insn = match ptrace_read_u16_unlocked(&aspace, next_insn_addr) {
        Ok(half) => half,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    if orig_insn == EBREAK_INSN {
        tracee.set_ptrace_ss_saved_insn_for(tid, None);
        ax_runtime::hal::cpu::asm::flush_icache_all();
        return;
    }

    let _ = ptrace_write_u16_unlocked(&mut aspace, next_insn_addr, EBREAK_INSN);
    tracee.set_ptrace_ss_saved_insn_for(tid, Some((next_insn_addr, orig_insn as usize)));
    ax_runtime::hal::cpu::asm::flush_icache_all();
}

#[cfg(target_arch = "aarch64")]
pub fn ptrace_setup_singlestep(
    tracee: &ProcessData,
    tid: Pid,
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
) {
    let pc = uctx.ip();
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();

    let saved = tracee.take_ptrace_ss_saved_insn_for(tid);
    if let Some((saved_addr, saved_insn)) = saved {
        let _ = ptrace_write_u32_unlocked(&mut aspace, saved_addr, saved_insn as u32);
    }

    let current_insn = match ptrace_read_u32_unlocked(&aspace, pc) {
        Ok(insn) => insn,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    let next_insn_addr = aarch64_next_pc(current_insn, pc, uctx);
    let orig_insn = match ptrace_read_u32_unlocked(&aspace, next_insn_addr) {
        Ok(insn) => insn,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    if orig_insn == AARCH64_BRK_INSN {
        tracee.set_ptrace_ss_saved_insn_for(tid, None);
        ax_runtime::hal::cpu::asm::flush_icache_all();
        return;
    }

    let _ = ptrace_write_u32_unlocked(&mut aspace, next_insn_addr, AARCH64_BRK_INSN);
    tracee.set_ptrace_ss_saved_insn_for(tid, Some((next_insn_addr, orig_insn as usize)));
    ax_runtime::hal::cpu::asm::flush_icache_all();
}

#[cfg(target_arch = "loongarch64")]
pub fn ptrace_setup_singlestep(
    tracee: &ProcessData,
    tid: Pid,
    uctx: &mut ax_runtime::hal::cpu::uspace::UserContext,
) {
    let pc = uctx.ip();
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();

    let saved = tracee.take_ptrace_ss_saved_insn_for(tid);
    if let Some((saved_addr, saved_insn)) = saved {
        let _ = ptrace_write_u32_unlocked(&mut aspace, saved_addr, saved_insn as u32);
    }

    let current_insn = match ptrace_read_u32_unlocked(&aspace, pc) {
        Ok(insn) => insn,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    let next_insn_addr = loongarch_next_pc(current_insn, pc, uctx);
    let orig_insn = match ptrace_read_u32_unlocked(&aspace, next_insn_addr) {
        Ok(insn) => insn,
        Err(_) => {
            tracee.set_ptrace_ss_saved_insn_for(tid, None);
            return;
        }
    };
    if orig_insn == LOONGARCH_BREAK_INSN {
        tracee.set_ptrace_ss_saved_insn_for(tid, None);
        ax_runtime::hal::cpu::asm::flush_icache_all();
        return;
    }

    let _ = ptrace_write_u32_unlocked(&mut aspace, next_insn_addr, LOONGARCH_BREAK_INSN);
    tracee.set_ptrace_ss_saved_insn_for(tid, Some((next_insn_addr, orig_insn as usize)));
    ax_runtime::hal::cpu::asm::flush_icache_all();
}

#[cfg(target_arch = "riscv64")]
pub fn ptrace_restore_singlestep_insn(
    tracee: &ProcessData,
    tid: Pid,
    addr: usize,
    insn: usize,
) -> bool {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    let restored = ptrace_write_u16_unlocked(&mut aspace, addr, insn as u16).is_ok();
    ax_runtime::hal::cpu::asm::flush_icache_all();
    if !restored {
        tracee.set_ptrace_ss_saved_insn_for(tid, Some((addr, insn)));
    }
    restored
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
pub fn ptrace_restore_singlestep_insn(
    tracee: &ProcessData,
    tid: Pid,
    addr: usize,
    insn: usize,
) -> bool {
    let aspace = tracee.aspace();
    let mut aspace = aspace.lock();
    let restored = ptrace_write_u32_unlocked(&mut aspace, addr, insn as u32).is_ok();
    ax_runtime::hal::cpu::asm::flush_icache_all();
    if !restored {
        tracee.set_ptrace_ss_saved_insn_for(tid, Some((addr, insn)));
    }
    restored
}

#[cfg(target_arch = "riscv64")]
fn riscv_insn_len(first_half: u16) -> usize {
    if first_half & 0x3 != 0x3 { 2 } else { 4 }
}

#[cfg(target_arch = "riscv64")]
fn riscv_next_pc(
    insn: u32,
    insn_len: usize,
    pc: usize,
    uctx: &ax_runtime::hal::cpu::uspace::UserContext,
) -> usize {
    if insn_len == 2 {
        return riscv_compressed_next_pc(insn as u16, pc, uctx);
    }

    match insn & 0x7f {
        0x63 => {
            let rs1 = ((insn >> 15) & 0x1f) as usize;
            let rs2 = ((insn >> 20) & 0x1f) as usize;
            let lhs = riscv_reg(uctx, rs1);
            let rhs = riscv_reg(uctx, rs2);
            let take = match (insn >> 12) & 0x7 {
                0x0 => lhs == rhs,
                0x1 => lhs != rhs,
                0x4 => (lhs as isize) < (rhs as isize),
                0x5 => (lhs as isize) >= (rhs as isize),
                0x6 => lhs < rhs,
                0x7 => lhs >= rhs,
                _ => false,
            };
            if take {
                riscv_add_pc(
                    pc,
                    riscv_sign_extend(
                        (((insn >> 31) & 0x1) << 12)
                            | (((insn >> 7) & 0x1) << 11)
                            | (((insn >> 25) & 0x3f) << 5)
                            | (((insn >> 8) & 0xf) << 1),
                        13,
                    ),
                )
            } else {
                pc + 4
            }
        }
        0x67 => {
            let rs1 = ((insn >> 15) & 0x1f) as usize;
            let imm = riscv_sign_extend((insn >> 20) & 0xfff, 12);
            riscv_add_pc(riscv_reg(uctx, rs1), imm) & !0x1
        }
        0x6f => riscv_add_pc(
            pc,
            riscv_sign_extend(
                (((insn >> 31) & 0x1) << 20)
                    | (((insn >> 12) & 0xff) << 12)
                    | (((insn >> 20) & 0x1) << 11)
                    | (((insn >> 21) & 0x3ff) << 1),
                21,
            ),
        ),
        _ => pc + 4,
    }
}

#[cfg(target_arch = "riscv64")]
fn riscv_compressed_next_pc(
    insn: u16,
    pc: usize,
    uctx: &ax_runtime::hal::cpu::uspace::UserContext,
) -> usize {
    let quadrant = insn & 0x3;
    let funct3 = (insn >> 13) & 0x7;

    if quadrant == 0x1 {
        match funct3 {
            0x1 | 0x5 => {
                return riscv_add_pc(pc, riscv_sign_extend(riscv_cj_imm(insn) as u32, 12));
            }
            0x6 | 0x7 => {
                let rs1 = 8 + (((insn >> 7) & 0x7) as usize);
                let take = if funct3 == 0x6 {
                    riscv_reg(uctx, rs1) == 0
                } else {
                    riscv_reg(uctx, rs1) != 0
                };
                return if take {
                    riscv_add_pc(pc, riscv_sign_extend(riscv_cb_imm(insn) as u32, 9))
                } else {
                    pc + 2
                };
            }
            _ => {}
        }
    }

    if quadrant == 0x2 && funct3 == 0x4 {
        let rs1 = ((insn >> 7) & 0x1f) as usize;
        let rs2 = ((insn >> 2) & 0x1f) as usize;
        if rs1 != 0 && rs2 == 0 {
            return riscv_reg(uctx, rs1);
        }
    }

    pc + 2
}

#[cfg(target_arch = "riscv64")]
fn riscv_cj_imm(insn: u16) -> u16 {
    (((insn >> 12) & 0x1) << 11)
        | (((insn >> 11) & 0x1) << 4)
        | (((insn >> 9) & 0x3) << 8)
        | (((insn >> 8) & 0x1) << 10)
        | (((insn >> 7) & 0x1) << 6)
        | (((insn >> 6) & 0x1) << 7)
        | (((insn >> 3) & 0x7) << 1)
        | (((insn >> 2) & 0x1) << 5)
}

#[cfg(target_arch = "riscv64")]
fn riscv_cb_imm(insn: u16) -> u16 {
    (((insn >> 12) & 0x1) << 8)
        | (((insn >> 10) & 0x3) << 3)
        | (((insn >> 5) & 0x3) << 6)
        | (((insn >> 3) & 0x3) << 1)
        | (((insn >> 2) & 0x1) << 5)
}

#[cfg(target_arch = "riscv64")]
fn riscv_sign_extend(value: u32, bits: u32) -> isize {
    let shift = usize::BITS - bits;
    ((value as usize) << shift) as isize >> shift
}

#[cfg(target_arch = "riscv64")]
fn riscv_add_pc(base: usize, offset: isize) -> usize {
    if offset >= 0 {
        base.wrapping_add(offset as usize)
    } else {
        base.wrapping_sub((-offset) as usize)
    }
}

#[cfg(target_arch = "riscv64")]
fn riscv_reg(uctx: &ax_runtime::hal::cpu::uspace::UserContext, index: usize) -> usize {
    match index {
        0 => 0,
        1 => uctx.regs.ra,
        2 => uctx.regs.sp,
        3 => uctx.regs.gp,
        4 => uctx.regs.tp,
        5 => uctx.regs.t0,
        6 => uctx.regs.t1,
        7 => uctx.regs.t2,
        8 => uctx.regs.s0,
        9 => uctx.regs.s1,
        10 => uctx.regs.a0,
        11 => uctx.regs.a1,
        12 => uctx.regs.a2,
        13 => uctx.regs.a3,
        14 => uctx.regs.a4,
        15 => uctx.regs.a5,
        16 => uctx.regs.a6,
        17 => uctx.regs.a7,
        18 => uctx.regs.s2,
        19 => uctx.regs.s3,
        20 => uctx.regs.s4,
        21 => uctx.regs.s5,
        22 => uctx.regs.s6,
        23 => uctx.regs.s7,
        24 => uctx.regs.s8,
        25 => uctx.regs.s9,
        26 => uctx.regs.s10,
        27 => uctx.regs.s11,
        28 => uctx.regs.t3,
        29 => uctx.regs.t4,
        30 => uctx.regs.t5,
        31 => uctx.regs.t6,
        _ => 0,
    }
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
fn ptrace_sign_extend(value: u32, bits: u32) -> isize {
    let shift = usize::BITS - bits;
    ((value as usize) << shift) as isize >> shift
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
fn ptrace_add_offset(base: usize, offset: isize) -> usize {
    if offset >= 0 {
        base.wrapping_add(offset as usize)
    } else {
        base.wrapping_sub((-offset) as usize)
    }
}

#[cfg(target_arch = "aarch64")]
fn aarch64_next_pc(
    insn: u32,
    pc: usize,
    uctx: &ax_runtime::hal::cpu::uspace::UserContext,
) -> usize {
    if insn & 0x7c00_0000 == 0x1400_0000 {
        return ptrace_add_offset(pc, ptrace_sign_extend(insn & 0x03ff_ffff, 26) << 2);
    }

    if insn & 0xff00_0010 == 0x5400_0000 {
        let cond = (insn & 0xf) as u8;
        let offset = ptrace_sign_extend((insn >> 5) & 0x7ffff, 19) << 2;
        return if aarch64_condition_holds(cond, uctx.spsr) {
            ptrace_add_offset(pc, offset)
        } else {
            pc.wrapping_add(4)
        };
    }

    if insn & 0x7e00_0000 == 0x3400_0000 {
        let rt = (insn & 0x1f) as usize;
        let value = aarch64_reg(uctx, rt);
        let is_64bit = insn & (1 << 31) != 0;
        let is_nonzero = insn & (1 << 24) != 0;
        let value_is_zero = if is_64bit {
            value == 0
        } else {
            (value as u32) == 0
        };
        let take = value_is_zero != is_nonzero;
        let offset = ptrace_sign_extend((insn >> 5) & 0x7ffff, 19) << 2;
        return if take {
            ptrace_add_offset(pc, offset)
        } else {
            pc.wrapping_add(4)
        };
    }

    if insn & 0x7e00_0000 == 0x3600_0000 {
        let rt = (insn & 0x1f) as usize;
        let bit_pos = (((insn >> 31) & 0x1) << 5) | ((insn >> 19) & 0x1f);
        let bit_is_set = ((aarch64_reg(uctx, rt) >> bit_pos) & 1) != 0;
        let take_if_set = insn & (1 << 24) != 0;
        let offset = ptrace_sign_extend((insn >> 5) & 0x3fff, 14) << 2;
        return if bit_is_set == take_if_set {
            ptrace_add_offset(pc, offset)
        } else {
            pc.wrapping_add(4)
        };
    }

    if insn & 0xffff_fc1f == 0xd61f_0000
        || insn & 0xffff_fc1f == 0xd63f_0000
        || insn & 0xffff_fc1f == 0xd65f_0000
    {
        return aarch64_reg(uctx, ((insn >> 5) & 0x1f) as usize);
    }

    pc.wrapping_add(4)
}

#[cfg(target_arch = "aarch64")]
fn aarch64_condition_holds(cond: u8, pstate: u64) -> bool {
    let n = pstate & (1 << 31) != 0;
    let z = pstate & (1 << 30) != 0;
    let c = pstate & (1 << 29) != 0;
    let v = pstate & (1 << 28) != 0;

    match cond {
        0x0 => z,
        0x1 => !z,
        0x2 => c,
        0x3 => !c,
        0x4 => n,
        0x5 => !n,
        0x6 => v,
        0x7 => !v,
        0x8 => c && !z,
        0x9 => !c || z,
        0xa => n == v,
        0xb => n != v,
        0xc => !z && n == v,
        0xd => z || n != v,
        _ => true,
    }
}

#[cfg(target_arch = "aarch64")]
fn aarch64_reg(uctx: &ax_runtime::hal::cpu::uspace::UserContext, index: usize) -> usize {
    if index < 31 {
        uctx.x[index] as usize
    } else {
        0
    }
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_next_pc(
    insn: u32,
    pc: usize,
    uctx: &ax_runtime::hal::cpu::uspace::UserContext,
) -> usize {
    match insn >> 26 {
        0x10 | 0x11 => {
            let rj = ((insn >> 5) & 0x1f) as usize;
            let imm = ((insn & 0x1f) << 16) | ((insn >> 10) & 0xffff);
            let is_nonzero = insn >> 26 == 0x11;
            let take = (loongarch_reg(uctx, rj) != 0) == is_nonzero;
            if take {
                ptrace_add_offset(pc, ptrace_sign_extend(imm, 21) << 2)
            } else {
                pc.wrapping_add(4)
            }
        }
        0x13 => {
            let rj = ((insn >> 5) & 0x1f) as usize;
            let offset = ptrace_sign_extend((insn >> 10) & 0xffff, 16) << 2;
            ptrace_add_offset(loongarch_reg(uctx, rj), offset)
        }
        0x14 | 0x15 => {
            let imm = ((insn & 0x3ff) << 16) | ((insn >> 10) & 0xffff);
            ptrace_add_offset(pc, ptrace_sign_extend(imm, 26) << 2)
        }
        0x16..=0x1b => {
            let rj = ((insn >> 5) & 0x1f) as usize;
            let rd = (insn & 0x1f) as usize;
            let lhs = loongarch_reg(uctx, rj);
            let rhs = loongarch_reg(uctx, rd);
            let take = match insn >> 26 {
                0x16 => lhs == rhs,
                0x17 => lhs != rhs,
                0x18 => (lhs as isize) < (rhs as isize),
                0x19 => (lhs as isize) >= (rhs as isize),
                0x1a => lhs < rhs,
                0x1b => lhs >= rhs,
                _ => false,
            };
            let offset = ptrace_sign_extend((insn >> 10) & 0xffff, 16) << 2;
            if take {
                ptrace_add_offset(pc, offset)
            } else {
                pc.wrapping_add(4)
            }
        }
        _ => pc.wrapping_add(4),
    }
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_reg(uctx: &ax_runtime::hal::cpu::uspace::UserContext, index: usize) -> usize {
    match index {
        0 => 0,
        1 => uctx.regs.ra,
        2 => uctx.regs.tp,
        3 => uctx.regs.sp,
        4 => uctx.regs.a0,
        5 => uctx.regs.a1,
        6 => uctx.regs.a2,
        7 => uctx.regs.a3,
        8 => uctx.regs.a4,
        9 => uctx.regs.a5,
        10 => uctx.regs.a6,
        11 => uctx.regs.a7,
        12 => uctx.regs.t0,
        13 => uctx.regs.t1,
        14 => uctx.regs.t2,
        15 => uctx.regs.t3,
        16 => uctx.regs.t4,
        17 => uctx.regs.t5,
        18 => uctx.regs.t6,
        19 => uctx.regs.t7,
        20 => uctx.regs.t8,
        21 => uctx.regs.u0,
        22 => uctx.regs.fp,
        23 => uctx.regs.s0,
        24 => uctx.regs.s1,
        25 => uctx.regs.s2,
        26 => uctx.regs.s3,
        27 => uctx.regs.s4,
        28 => uctx.regs.s5,
        29 => uctx.regs.s6,
        30 => uctx.regs.s7,
        31 => uctx.regs.s8,
        _ => 0,
    }
}

#[cfg(target_arch = "riscv64")]
fn ptrace_read_u16_unlocked(aspace: &AddrSpace, addr: usize) -> AxResult<u16> {
    let mut bytes = [0u8; size_of::<u16>()];
    aspace.read(VirtAddr::from_usize(addr), &mut bytes)?;
    Ok(u16::from_ne_bytes(bytes))
}

#[cfg(any(
    target_arch = "riscv64",
    target_arch = "aarch64",
    target_arch = "loongarch64"
))]
fn ptrace_read_u32_unlocked(aspace: &AddrSpace, addr: usize) -> AxResult<u32> {
    let mut bytes = [0u8; size_of::<u32>()];
    aspace.read(VirtAddr::from_usize(addr), &mut bytes)?;
    Ok(u32::from_ne_bytes(bytes))
}

#[cfg(target_arch = "riscv64")]
fn ptrace_write_u16_unlocked(aspace: &mut AddrSpace, addr: usize, data: u16) -> AxResult {
    aspace.write(VirtAddr::from_usize(addr), &data.to_ne_bytes())?;
    Ok(())
}

#[cfg(any(target_arch = "aarch64", target_arch = "loongarch64"))]
fn ptrace_write_u32_unlocked(aspace: &mut AddrSpace, addr: usize, data: u32) -> AxResult {
    aspace.write(VirtAddr::from_usize(addr), &data.to_ne_bytes())?;
    Ok(())
}

pub fn ptrace_notify_clone(parent_pid: Pid, parent_tid: Pid, child_pid: Pid, event: u32) -> bool {
    let Ok(parent) = get_process_data(parent_pid) else {
        return false;
    };
    if !parent.is_ptrace_traceme() && !parent.is_ptrace_attached() {
        return false;
    }
    let options = parent.ptrace_options();
    let option_flag = match event {
        PTRACE_EVENT_FORK => PTRACE_O_TRACEFORK,
        PTRACE_EVENT_VFORK => PTRACE_O_TRACEVFORK,
        PTRACE_EVENT_CLONE => PTRACE_O_TRACECLONE,
        _ => return false,
    };
    if options & option_flag == 0 {
        return false;
    }
    parent.set_ptrace_pending_event(parent_tid, event, child_pid as usize);
    true
}

pub fn ptrace_notify_exec(tracee_pid: Pid) -> bool {
    let Ok(tracee) = get_process_data(tracee_pid) else {
        return false;
    };
    let options = tracee.ptrace_options();
    if options & PTRACE_O_TRACEEXEC == 0 {
        return false;
    }
    tracee.set_ptrace_pending_event(tracee_pid, PTRACE_EVENT_EXEC, tracee_pid as usize);
    true
}

pub fn ptrace_notify_exit(tracee_pid: Pid, exit_code: i32) -> bool {
    let Ok(tracee) = get_process_data(tracee_pid) else {
        return false;
    };
    if !tracee.is_ptrace_traceme() && !tracee.is_ptrace_attached() {
        return false;
    }
    let options = tracee.ptrace_options();
    if options & PTRACE_O_TRACEEXIT == 0 {
        return false;
    }
    tracee.set_ptrace_pending_event(tracee_pid, PTRACE_EVENT_EXIT, exit_code as usize);
    true
}

pub fn ptrace_notify_vfork_done(parent_pid: Pid, parent_tid: Pid, child_pid: Pid) -> bool {
    let Ok(parent) = get_process_data(parent_pid) else {
        return false;
    };
    if !parent.is_ptrace_traceme() && !parent.is_ptrace_attached() {
        return false;
    }
    let options = parent.ptrace_options();
    if options & PTRACE_O_TRACEVFORKDONE == 0 {
        return false;
    }
    parent.set_ptrace_pending_event(parent_tid, PTRACE_EVENT_VFORK_DONE, child_pid as usize);
    true
}

#[cfg(target_arch = "riscv64")]
impl From<&ax_runtime::hal::cpu::uspace::UserContext> for RiscvUserRegs {
    fn from(uctx: &ax_runtime::hal::cpu::uspace::UserContext) -> Self {
        let r = &uctx.regs;
        Self {
            pc: uctx.sepc,
            ra: r.ra,
            sp: r.sp,
            gp: r.gp,
            tp: r.tp,
            t0: r.t0,
            t1: r.t1,
            t2: r.t2,
            s0: r.s0,
            s1: r.s1,
            a0: r.a0,
            a1: r.a1,
            a2: r.a2,
            a3: r.a3,
            a4: r.a4,
            a5: r.a5,
            a6: r.a6,
            a7: r.a7,
            s2: r.s2,
            s3: r.s3,
            s4: r.s4,
            s5: r.s5,
            s6: r.s6,
            s7: r.s7,
            s8: r.s8,
            s9: r.s9,
            s10: r.s10,
            s11: r.s11,
            t3: r.t3,
            t4: r.t4,
            t5: r.t5,
            t6: r.t6,
        }
    }
}

#[cfg(target_arch = "riscv64")]
impl RiscvUserRegs {
    fn write_to(&self, uctx: &mut ax_runtime::hal::cpu::uspace::UserContext) -> AxResult<()> {
        uctx.sepc = self.pc;
        let r = &mut uctx.regs;
        r.ra = self.ra;
        r.sp = self.sp;
        r.gp = self.gp;
        r.tp = self.tp;
        r.t0 = self.t0;
        r.t1 = self.t1;
        r.t2 = self.t2;
        r.s0 = self.s0;
        r.s1 = self.s1;
        r.a0 = self.a0;
        r.a1 = self.a1;
        r.a2 = self.a2;
        r.a3 = self.a3;
        r.a4 = self.a4;
        r.a5 = self.a5;
        r.a6 = self.a6;
        r.a7 = self.a7;
        r.s2 = self.s2;
        r.s3 = self.s3;
        r.s4 = self.s4;
        r.s5 = self.s5;
        r.s6 = self.s6;
        r.s7 = self.s7;
        r.s8 = self.s8;
        r.s9 = self.s9;
        r.s10 = self.s10;
        r.s11 = self.s11;
        r.t3 = self.t3;
        r.t4 = self.t4;
        r.t5 = self.t5;
        r.t6 = self.t6;
        Ok(())
    }
}

#[cfg(target_arch = "riscv64")]
impl From<PtraceStopFpData> for RiscvFpRegs {
    fn from(data: PtraceStopFpData) -> Self {
        Self {
            f: data.regs,
            fcsr: data.fcsr,
        }
    }
}

#[cfg(target_arch = "riscv64")]
impl From<RiscvFpRegs> for PtraceStopFpData {
    fn from(regs: RiscvFpRegs) -> Self {
        Self {
            regs: regs.f,
            fcsr: regs.fcsr,
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl From<&ax_runtime::hal::cpu::uspace::UserContext> for Aarch64UserRegs {
    fn from(uctx: &ax_runtime::hal::cpu::uspace::UserContext) -> Self {
        Self {
            regs: uctx.x,
            sp: uctx.sp,
            pc: uctx.elr,
            pstate: uctx.spsr,
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Aarch64UserRegs {
    fn write_to(&self, uctx: &mut ax_runtime::hal::cpu::uspace::UserContext) -> AxResult<()> {
        uctx.x = self.regs;
        uctx.sp = self.sp;
        uctx.elr = self.pc;
        uctx.spsr = self.pstate;
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
impl From<PtraceStopFpData> for Aarch64FpRegs {
    fn from(data: PtraceStopFpData) -> Self {
        Self {
            vregs: data.regs,
            fpsr: data.fpsr,
            fpcr: data.fpcr,
            __reserved: [0; 2],
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl From<Aarch64FpRegs> for PtraceStopFpData {
    fn from(regs: Aarch64FpRegs) -> Self {
        Self {
            regs: regs.vregs,
            fpcr: regs.fpcr,
            fpsr: regs.fpsr,
        }
    }
}

#[cfg(target_arch = "loongarch64")]
impl From<&ax_runtime::hal::cpu::uspace::UserContext> for LoongarchUserRegs {
    fn from(uctx: &ax_runtime::hal::cpu::uspace::UserContext) -> Self {
        let r = &uctx.regs;
        Self {
            regs: [
                r.zero as u64,
                r.ra as u64,
                r.tp as u64,
                r.sp as u64,
                r.a0 as u64,
                r.a1 as u64,
                r.a2 as u64,
                r.a3 as u64,
                r.a4 as u64,
                r.a5 as u64,
                r.a6 as u64,
                r.a7 as u64,
                r.t0 as u64,
                r.t1 as u64,
                r.t2 as u64,
                r.t3 as u64,
                r.t4 as u64,
                r.t5 as u64,
                r.t6 as u64,
                r.t7 as u64,
                r.t8 as u64,
                r.u0 as u64,
                r.fp as u64,
                r.s0 as u64,
                r.s1 as u64,
                r.s2 as u64,
                r.s3 as u64,
                r.s4 as u64,
                r.s5 as u64,
                r.s6 as u64,
                r.s7 as u64,
                r.s8 as u64,
            ],
            orig_a0: r.a0 as u64,
            csr_era: uctx.era as u64,
            csr_badv: 0,
            reserved: [0; 10],
        }
    }
}

#[cfg(target_arch = "loongarch64")]
impl LoongarchUserRegs {
    fn write_to(&self, uctx: &mut ax_runtime::hal::cpu::uspace::UserContext) -> AxResult<()> {
        let r = &mut uctx.regs;
        r.zero = 0;
        r.ra = self.regs[1] as usize;
        r.tp = self.regs[2] as usize;
        r.sp = self.regs[3] as usize;
        r.a0 = self.regs[4] as usize;
        r.a1 = self.regs[5] as usize;
        r.a2 = self.regs[6] as usize;
        r.a3 = self.regs[7] as usize;
        r.a4 = self.regs[8] as usize;
        r.a5 = self.regs[9] as usize;
        r.a6 = self.regs[10] as usize;
        r.a7 = self.regs[11] as usize;
        r.t0 = self.regs[12] as usize;
        r.t1 = self.regs[13] as usize;
        r.t2 = self.regs[14] as usize;
        r.t3 = self.regs[15] as usize;
        r.t4 = self.regs[16] as usize;
        r.t5 = self.regs[17] as usize;
        r.t6 = self.regs[18] as usize;
        r.t7 = self.regs[19] as usize;
        r.t8 = self.regs[20] as usize;
        r.u0 = self.regs[21] as usize;
        r.fp = self.regs[22] as usize;
        r.s0 = self.regs[23] as usize;
        r.s1 = self.regs[24] as usize;
        r.s2 = self.regs[25] as usize;
        r.s3 = self.regs[26] as usize;
        r.s4 = self.regs[27] as usize;
        r.s5 = self.regs[28] as usize;
        r.s6 = self.regs[29] as usize;
        r.s7 = self.regs[30] as usize;
        r.s8 = self.regs[31] as usize;
        uctx.era = self.csr_era as usize;
        Ok(())
    }
}

#[cfg(target_arch = "loongarch64")]
impl From<PtraceStopFpData> for LoongarchFpRegs {
    fn from(data: PtraceStopFpData) -> Self {
        Self {
            fpr: data.regs,
            fcc: loongarch_pack_fcc(data.fcc),
            fcsr: data.fcsr,
        }
    }
}

#[cfg(target_arch = "loongarch64")]
impl From<LoongarchFpRegs> for PtraceStopFpData {
    fn from(regs: LoongarchFpRegs) -> Self {
        Self {
            regs: regs.fpr,
            fp_high: [0; 32],
            fp_lasx_hi0: [0; 32],
            fp_lasx_hi1: [0; 32],
            fcc: loongarch_unpack_fcc(regs.fcc),
            fcsr: regs.fcsr,
        }
    }
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_pack_fcc(fcc: [u8; 8]) -> u64 {
    let mut packed = 0u64;
    for (idx, value) in fcc.into_iter().enumerate() {
        packed |= (value as u64) << (idx * 8);
    }
    packed
}

#[cfg(target_arch = "loongarch64")]
fn loongarch_unpack_fcc(packed: u64) -> [u8; 8] {
    let mut fcc = [0u8; 8];
    for (idx, value) in fcc.iter_mut().enumerate() {
        *value = ((packed >> (idx * 8)) & 0xff) as u8;
    }
    fcc
}

#[cfg(target_arch = "x86_64")]
impl From<PtraceStopFpData> for X8664FpRegs {
    fn from(data: PtraceStopFpData) -> Self {
        Self(data.0)
    }
}

#[cfg(target_arch = "x86_64")]
impl From<X8664FpRegs> for PtraceStopFpData {
    fn from(regs: X8664FpRegs) -> Self {
        Self(regs.0)
    }
}

// ---------------------------------------------------------------------------
// x86_64 user-area constants and PEEKUSER/POKEUSER helpers
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
const X86_64_USER_DEBUGREG_OFFSET: usize = 848;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_DEBUGREG_COUNT: usize = 8;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_DEBUGREG_END: usize =
    X86_64_USER_DEBUGREG_OFFSET + X86_64_USER_DEBUGREG_COUNT * size_of::<u64>();
#[cfg(target_arch = "x86_64")]
const X86_64_USER_AREA_SIZE: usize = X86_64_USER_DEBUGREG_END;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_FPVALID_OFFSET: usize = size_of::<X8664UserRegs>();
#[cfg(target_arch = "x86_64")]
const X86_64_USER_I387_OFFSET: usize = 224;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_TSIZE_OFFSET: usize = 736;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_DSIZE_OFFSET: usize = 744;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_SSIZE_OFFSET: usize = 752;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_START_CODE_OFFSET: usize = 760;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_START_STACK_OFFSET: usize = 768;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_SIGNAL_OFFSET: usize = 776;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_RESERVED_OFFSET: usize = 784;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_AR0_OFFSET: usize = 792;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_FPSTATE_OFFSET: usize = 800;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_MAGIC_OFFSET: usize = 808;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_COMM_OFFSET: usize = 816;
#[cfg(target_arch = "x86_64")]
const X86_64_USER_COMM_SIZE: usize = 32;

#[cfg(target_arch = "x86_64")]
fn ptrace_user_word_range_x86_64(offset: usize) -> AxResult<core::ops::Range<usize>> {
    let word_size = size_of::<u64>();
    let end = offset
        .checked_add(word_size)
        .ok_or_else(|| AxError::from(LinuxError::EIO))?;
    if !offset.is_multiple_of(word_size) || end > X86_64_USER_AREA_SIZE {
        return Err(AxError::from(LinuxError::EIO));
    }
    Ok(offset..end)
}

#[cfg(target_arch = "x86_64")]
fn ptrace_peekuser(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    if data == 0 {
        return Err(AxError::InvalidInput);
    }
    let range = ptrace_user_word_range_x86_64(addr)?;
    let user = ptrace_read_stopped_user_area_x86_64(pid)?;
    let value = u64::from_ne_bytes(user[range].try_into().unwrap()) as usize;
    (data as *mut usize).vm_write(value)?;
    Ok(0)
}

#[cfg(target_arch = "x86_64")]
fn ptrace_pokeuser(pid: usize, addr: usize, data: usize) -> AxResult<isize> {
    let range = ptrace_user_word_range_x86_64(addr)?;
    if range.start >= X86_64_USER_DEBUGREG_OFFSET && range.end <= X86_64_USER_DEBUGREG_END {
        let _ = (pid, data);
        return Err(AxError::from(LinuxError::EIO));
    }
    if range.end > size_of::<X8664UserRegs>() {
        return Err(AxError::from(LinuxError::EIO));
    }
    let mut regs = ptrace_read_stopped_user_regs(pid)?;
    let bytes = unsafe {
        slice::from_raw_parts_mut(
            (&mut regs as *mut ArchUserRegs).cast::<u8>(),
            size_of::<ArchUserRegs>(),
        )
    };
    bytes[range].copy_from_slice(&(data as u64).to_ne_bytes());
    ptrace_write_stopped_user_regs(pid, regs)
}

#[cfg(target_arch = "x86_64")]
fn ptrace_read_stopped_user_area_x86_64(pid: usize) -> AxResult<[u8; X86_64_USER_AREA_SIZE]> {
    let regs = ptrace_read_stopped_user_regs(pid)?;
    let (tracee, _tid) = ptrace_stopped_tracee_with_tid(pid)?;
    let mut user = [0u8; X86_64_USER_AREA_SIZE];
    let regs_bytes = unsafe {
        slice::from_raw_parts(
            (&regs as *const ArchUserRegs).cast::<u8>(),
            size_of::<ArchUserRegs>(),
        )
    };
    user[..size_of::<ArchUserRegs>()].copy_from_slice(regs_bytes);

    user[X86_64_USER_FPVALID_OFFSET..X86_64_USER_FPVALID_OFFSET + size_of::<u32>()]
        .copy_from_slice(&1u32.to_ne_bytes());

    let mut start_code = usize::MAX;
    let mut end_code = 0usize;
    let mut data_size = 0usize;
    let mut stack_size = 0usize;
    let mut start_stack = regs.rsp as usize;
    let aspace = tracee.aspace();
    let mm = aspace.lock();
    for area in mm.areas() {
        let flags = area.flags();
        if flags.contains(MappingFlags::EXECUTE) {
            start_code = start_code.min(area.start().as_usize());
            end_code = end_code.max(area.end().as_usize());
        } else if flags.contains(MappingFlags::WRITE) {
            data_size = data_size.saturating_add(area.size());
        }
        if area
            .backend()
            .file_info()
            .ok()
            .is_some_and(|info| info.path == "[stack]")
        {
            stack_size = area.size();
            start_stack = area.end().as_usize();
        }
    }
    let text_size = if start_code == usize::MAX {
        0
    } else {
        end_code.saturating_sub(start_code)
    };

    let write_u64 = |buf: &mut [u8; X86_64_USER_AREA_SIZE], offset: usize, value: u64| {
        buf[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_ne_bytes());
    };
    let write_i32 = |buf: &mut [u8; X86_64_USER_AREA_SIZE], offset: usize, value: i32| {
        buf[offset..offset + size_of::<i32>()].copy_from_slice(&value.to_ne_bytes());
    };

    write_u64(
        &mut user,
        X86_64_USER_TSIZE_OFFSET,
        (text_size / PAGE_SIZE_4K) as u64,
    );
    write_u64(
        &mut user,
        X86_64_USER_DSIZE_OFFSET,
        (data_size / PAGE_SIZE_4K) as u64,
    );
    write_u64(
        &mut user,
        X86_64_USER_SSIZE_OFFSET,
        (stack_size / PAGE_SIZE_4K) as u64,
    );
    write_u64(
        &mut user,
        X86_64_USER_START_CODE_OFFSET,
        if start_code == usize::MAX {
            0
        } else {
            start_code as u64
        },
    );
    write_u64(
        &mut user,
        X86_64_USER_START_STACK_OFFSET,
        start_stack as u64,
    );
    write_u64(&mut user, X86_64_USER_AR0_OFFSET, 0);
    write_u64(
        &mut user,
        X86_64_USER_FPSTATE_OFFSET,
        X86_64_USER_I387_OFFSET as u64,
    );
    write_u64(&mut user, X86_64_USER_MAGIC_OFFSET, 0);
    write_i32(&mut user, X86_64_USER_RESERVED_OFFSET, 0);
    write_u64(&mut user, X86_64_USER_SIGNAL_OFFSET, 0);

    if let Some(tid) = tracee.proc.threads().into_iter().next()
        && let Ok(task) = crate::task::get_task(tid)
    {
        let name = task.name();
        let copy_len = name.len().min(X86_64_USER_COMM_SIZE.saturating_sub(1));
        user[X86_64_USER_COMM_OFFSET..X86_64_USER_COMM_OFFSET + copy_len]
            .copy_from_slice(&name.as_bytes()[..copy_len]);
    }

    Ok(user)
}

#[cfg(not(target_arch = "x86_64"))]
fn ptrace_peekuser(_pid: usize, _addr: usize, _data: usize) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

#[cfg(not(target_arch = "x86_64"))]
fn ptrace_pokeuser(_pid: usize, _addr: usize, _data: usize) -> AxResult<isize> {
    Err(AxError::Unsupported)
}

// ---------------------------------------------------------------------------
// x86_64 X8664UserRegs ↔ UserContext conversions
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
fn sanitize_ptrace_x86_64_eflags(new_eflags: u64, current_eflags: u64) -> u64 {
    const CF: u64 = 1 << 0;
    const PF: u64 = 1 << 2;
    const AF: u64 = 1 << 4;
    const ZF: u64 = 1 << 6;
    const SF: u64 = 1 << 7;
    const TF: u64 = 1 << 8;
    const DF: u64 = 1 << 10;
    const OF: u64 = 1 << 11;
    const RF: u64 = 1 << 16;
    const AC: u64 = 1 << 18;
    const ID: u64 = 1 << 21;
    const USER_WRITABLE_MASK: u64 = CF | PF | AF | ZF | SF | TF | DF | OF | RF | AC | ID;
    const RESERVED_BIT1: u64 = 1 << 1;

    (current_eflags & !USER_WRITABLE_MASK) | (new_eflags & USER_WRITABLE_MASK) | RESERVED_BIT1
}

#[cfg(target_arch = "x86_64")]
impl From<&ax_runtime::hal::cpu::uspace::UserContext> for X8664UserRegs {
    fn from(uctx: &ax_runtime::hal::cpu::uspace::UserContext) -> Self {
        Self {
            r15: uctx.r15,
            r14: uctx.r14,
            r13: uctx.r13,
            r12: uctx.r12,
            rbp: uctx.rbp,
            rbx: uctx.rbx,
            r11: uctx.r11,
            r10: uctx.r10,
            r9: uctx.r9,
            r8: uctx.r8,
            rax: uctx.rax,
            rcx: uctx.rcx,
            rdx: uctx.rdx,
            rsi: uctx.rsi,
            rdi: uctx.rdi,
            orig_rax: uctx.rax,
            rip: uctx.rip,
            cs: uctx.cs,
            eflags: uctx.rflags,
            rsp: uctx.rsp,
            ss: uctx.ss,
            fs_base: uctx.fs_base,
            gs_base: uctx.gs_base,
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl X8664UserRegs {
    fn write_to(&self, uctx: &mut ax_runtime::hal::cpu::uspace::UserContext) -> AxResult<()> {
        if self.cs != uctx.cs || self.ss != uctx.ss {
            return Err(AxError::from(LinuxError::EINVAL));
        }

        uctx.r15 = self.r15;
        uctx.r14 = self.r14;
        uctx.r13 = self.r13;
        uctx.r12 = self.r12;
        uctx.rbp = self.rbp;
        uctx.rbx = self.rbx;
        uctx.r11 = self.r11;
        uctx.r10 = self.r10;
        uctx.r9 = self.r9;
        uctx.r8 = self.r8;
        uctx.rax = self.rax;
        uctx.rcx = self.rcx;
        uctx.rdx = self.rdx;
        uctx.rsi = self.rsi;
        uctx.rdi = self.rdi;
        uctx.rip = self.rip;
        uctx.rflags = sanitize_ptrace_x86_64_eflags(self.eflags, uctx.rflags);
        uctx.rsp = self.rsp;
        uctx.fs_base = self.fs_base;
        uctx.gs_base = self.gs_base;
        Ok(())
    }
}
