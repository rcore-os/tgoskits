use axerrno::{LinuxError, LinuxResult};
use axhal::{
    arch::TrapFrame,
    trap::{POST_TRAP, register_trap_handler},
};
use axprocess::{Process, ProcessGroup, Thread};
use axsignal::{SignalInfo, SignalOSAction, SignalSet};
use axtask::current;
use starry_core::task::{ProcessData, StarryTaskExt, ThreadData};

use crate::do_exit;

pub fn check_signals(tf: &mut TrapFrame, restore_blocked: Option<SignalSet>) -> bool {
    let Some((sig, os_action)) = StarryTaskExt::of(&current())
        .thread_data()
        .signal
        .check_signals(tf, restore_blocked)
    else {
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

#[register_trap_handler(POST_TRAP)]
fn post_trap_callback(tf: &mut TrapFrame, from_user: bool) {
    if !from_user {
        return;
    }

    check_signals(tf, None);
}

pub fn send_signal_thread(thr: &Thread, sig: SignalInfo) -> LinuxResult<()> {
    info!("Send signal {:?} to thread {}", sig.signo(), thr.tid());
    let Some(thr) = thr.data::<ThreadData>() else {
        return Err(LinuxError::EPERM);
    };
    thr.signal.send_signal(sig);
    Ok(())
}

pub fn send_signal_process(proc: &Process, sig: SignalInfo) -> LinuxResult<()> {
    info!("Send signal {:?} to process {}", sig.signo(), proc.pid());
    let Some(proc) = proc.data::<ProcessData>() else {
        return Err(LinuxError::EPERM);
    };
    proc.signal.send_signal(sig);
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
