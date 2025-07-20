use core::sync::atomic::{AtomicBool, Ordering};

use axerrno::{LinuxError, LinuxResult};
use axhal::{
    context::TrapFrame,
    trap::{POST_TRAP, PRE_TRAP, register_trap_handler},
};
use axprocess::Pid;
use axsignal::{SignalInfo, SignalOSAction, SignalSet};
use axtask::current;
use starry_core::{
    task::{AsThread, get_process_data, get_process_group, get_task, set_timer_state},
    time::TimerState,
};

use crate::{mm::access_user_memory, task::do_exit};

pub fn check_signals(tf: &mut TrapFrame, restore_blocked: Option<SignalSet>) -> bool {
    // axsignal may access user memory internally
    let result = access_user_memory(|| {
        current()
            .as_thread()
            .signal
            .check_signals(tf, restore_blocked)
    });
    let Some((sig, os_action)) = result else {
        return false;
    };

    let signo = sig.signo();
    match os_action {
        SignalOSAction::Terminate => {
            do_exit(128 + signo as i32, true);
        }
        SignalOSAction::CoreDump => {
            // TODO: implement core dump
            do_exit(128 + signo as i32, true);
        }
        SignalOSAction::Stop => {
            // TODO: implement stop
            do_exit(1, true);
        }
        SignalOSAction::Continue => {
            // TODO: implement continue
        }
        SignalOSAction::Handler => {
            // do nothing
        }
    }
    true
}

pub fn have_signals() -> bool {
    !current().as_thread().signal.pending().is_empty()
}

pub static BLOCK_NEXT_SIGNAL_CHECK: AtomicBool = AtomicBool::new(false);

#[register_trap_handler(PRE_TRAP)]
fn pre_trap_callback(_tf: &mut TrapFrame, from_user: bool) {
    if from_user {
        set_timer_state(&current(), TimerState::Kernel);
    }
}

#[register_trap_handler(POST_TRAP)]
fn post_trap_callback(tf: &mut TrapFrame, from_user: bool) {
    if !from_user {
        return;
    }

    if !BLOCK_NEXT_SIGNAL_CHECK.swap(false, Ordering::SeqCst) {
        check_signals(tf, None);
    }
    let curr = current();
    set_timer_state(&curr, TimerState::User);
    curr.set_interrupted(false);

    if curr.as_thread().pending_exit() {
        axtask::exit(0);
    }
}

/// Sends a signal to a thread.
pub fn send_signal_to_thread(
    tgid: Option<Pid>,
    tid: Pid,
    sig: Option<SignalInfo>,
) -> LinuxResult<()> {
    let task = get_task(tid)?;
    let thread = task.try_as_thread().ok_or(LinuxError::EPERM)?;
    if tgid.is_some_and(|tgid| thread.proc_data.proc.pid() != tgid) {
        return Err(LinuxError::ESRCH);
    }

    if let Some(sig) = sig {
        info!("Send signal {:?} to thread {}", sig.signo(), tid);
        thread.signal.send_signal(sig);
        task.set_interrupted(true);
    }

    Ok(())
}

/// Sends a signal to a process.
pub fn send_signal_to_process(pid: Pid, sig: Option<SignalInfo>) -> LinuxResult<()> {
    let proc_data = get_process_data(pid)?;

    if let Some(sig) = sig {
        info!("Send signal {:?} to process {}", sig.signo(), pid);
        proc_data.signal.send_signal(sig);
        for tid in proc_data.proc.threads() {
            if let Ok(task) = get_task(tid) {
                task.set_interrupted(true);
            }
        }
    }

    Ok(())
}

/// Sends a signal to a process group.
pub fn send_signal_to_process_group(pgid: Pid, sig: Option<SignalInfo>) -> LinuxResult<()> {
    let pg = get_process_group(pgid)?;

    if let Some(sig) = sig {
        info!("Send signal {:?} to process group {}", sig.signo(), pgid);
        for proc in pg.processes() {
            send_signal_to_process(proc.pid(), Some(sig.clone()))?;
        }
    }

    Ok(())
}
