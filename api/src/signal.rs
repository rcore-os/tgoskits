use core::sync::atomic::{AtomicBool, Ordering};

use axerrno::{LinuxError, LinuxResult};
use axhal::{
    arch::TrapFrame,
    trap::{POST_TRAP, PRE_TRAP, register_trap_handler},
};
use axprocess::{Process, ProcessGroup, Thread};
use axsignal::{SignalInfo, SignalOSAction, SignalSet};
use axtask::current;
use starry_core::{
    mm::access_user_memory,
    task::{ProcessData, StarryTaskExt, ThreadData},
    time::TimerState,
};

use crate::do_exit;

pub fn check_signals(tf: &mut TrapFrame, restore_blocked: Option<SignalSet>) -> bool {
    // axsignal may access user memory internally
    let result = access_user_memory(|| {
        StarryTaskExt::of(&current())
            .thread_data()
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
    !StarryTaskExt::of(&current())
        .thread_data()
        .signal
        .pending()
        .is_empty()
}

pub static BLOCK_NEXT_SIGNAL_CHECK: AtomicBool = AtomicBool::new(false);

#[register_trap_handler(PRE_TRAP)]
fn pre_trap_callback(_tf: &mut TrapFrame, from_user: bool) {
    if from_user && let Some(ext) = StarryTaskExt::try_of(&current()) {
        ext.thread_data().set_timer_state(TimerState::Kernel);
    }
}

#[register_trap_handler(POST_TRAP)]
fn post_trap_callback(tf: &mut TrapFrame, from_user: bool) {
    if !from_user {
        if let Some(ext) = StarryTaskExt::try_of(&current()) {
            ext.thread_data().poll_timer();
        }
        return;
    }

    if !BLOCK_NEXT_SIGNAL_CHECK.swap(false, Ordering::SeqCst) {
        check_signals(tf, None);
    }
    let curr = current();
    if let Some(ext) = StarryTaskExt::try_of(&curr) {
        ext.thread_data().set_timer_state(TimerState::User);
    }
    curr.set_interrupted(false);
}

pub fn send_signal_thread(thr: &Thread, sig: SignalInfo) -> LinuxResult<()> {
    info!("Send signal {:?} to thread {}", sig.signo(), thr.tid());
    let Some(thr_data) = thr.data::<ThreadData>() else {
        return Err(LinuxError::EPERM);
    };
    thr_data.signal.send_signal(sig);
    // TODO(mivik): correct task handling
    if let Ok(task) = thr_data.get_task() {
        task.set_interrupted(true);
    }
    Ok(())
}

pub fn send_signal_process(proc: &Process, sig: SignalInfo) -> LinuxResult<()> {
    debug!("Send signal {:?} to process {}", sig.signo(), proc.pid());
    let Some(proc_data) = proc.data::<ProcessData>() else {
        return Err(LinuxError::EPERM);
    };
    proc_data.signal.send_signal(sig);
    for thr in proc.threads() {
        if let Ok(task) = thr.data::<ThreadData>().unwrap().get_task() {
            task.set_interrupted(true);
        }
    }
    Ok(())
}

pub fn send_signal_process_group(pg: &ProcessGroup, sig: SignalInfo) -> LinuxResult<()> {
    info!(
        "Send signal {:?} to process group {}",
        sig.signo(),
        pg.pgid()
    );
    for proc in pg.processes() {
        send_signal_process(&proc, sig.clone())?;
    }
    Ok(())
}
