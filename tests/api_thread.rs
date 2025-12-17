#![feature(maybe_uninit_write_slice)]

use std::{
    mem::{MaybeUninit, zeroed},
    sync::{Arc, LazyLock, Mutex, MutexGuard},
};

use axcpu::uspace::UserContext;
use extern_trait::extern_trait;
use kspin::SpinNoIrq;
use starry_signal::{
    SignalDisposition, SignalInfo, SignalOSAction, SignalSet, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};
use starry_vm::{VmIo, VmResult};

struct TestEnv {
    actions: Arc<SpinNoIrq<SignalActions>>,
    proc: Arc<ProcessSignalManager>,
    thr: Arc<ThreadSignalManager>,
}

impl TestEnv {
    fn new() -> Self {
        let actions = Arc::new(SpinNoIrq::new(SignalActions::default()));
        let proc = Arc::new(ProcessSignalManager::new(actions.clone(), 0));
        let thr = ThreadSignalManager::new(7, proc.clone());
        Self { actions, proc, thr }
    }
}

static POOL: LazyLock<Mutex<Box<[u8]>>> = LazyLock::new(|| {
    let size = 0x0100_0000; // 1 MiB
    Mutex::new(vec![0; size].into_boxed_slice())
});

struct Vm(MutexGuard<'static, Box<[u8]>>);

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        let pool = POOL.lock().unwrap();
        Vm(pool)
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        let base = self.0.as_ptr() as usize;
        let offset = start - base;
        let slice = &self.0[offset..offset + buf.len()];
        buf.write_copy_of_slice(slice);
        Ok(())
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        let base = self.0.as_ptr() as usize;
        let offset = start - base;
        let slice = &mut self.0[offset..offset + buf.len()];
        slice.copy_from_slice(buf);
        Ok(())
    }
}

#[test]
fn block_ignore_send_signal() {
    let env = TestEnv::new();
    let actions = env.actions.clone();
    let sig = SignalInfo::new_user(Signo::SIGINT, 0, 1);
    assert!(env.thr.send_signal(sig.clone()));

    actions.lock()[Signo::SIGINT].disposition = SignalDisposition::Ignore;
    let proc_ignore = Arc::new(ProcessSignalManager::new(actions.clone(), 0));
    let thr_ignore = ThreadSignalManager::new(7, proc_ignore.clone());
    assert!(!thr_ignore.send_signal(sig.clone()));

    let mut set = SignalSet::default();
    set.add(Signo::SIGINT);
    env.thr.set_blocked(set);
    assert!(!env.thr.send_signal(sig.clone()));
    assert!(env.thr.pending().has(Signo::SIGINT));
    assert!(env.thr.signal_blocked(Signo::SIGINT));

    let empty = SignalSet::default();
    env.thr.set_blocked(empty);
    assert!(!env.thr.signal_blocked(Signo::SIGINT));
}

#[test]
fn handle_signal() {
    unsafe extern "C" fn test_handler(_: i32) {}
    let env = TestEnv::new();
    let actions = env.actions.clone();
    actions.lock()[Signo::SIGTERM].disposition = SignalDisposition::Handler(test_handler);
    let sig = SignalInfo::new_user(Signo::SIGTERM, 9, 9);

    let mut uctx: UserContext = unsafe { zeroed() };
    let initial_sp = {
        let pool = POOL.lock().unwrap();
        pool.as_ptr() as usize + pool.len()
    };
    uctx.set_sp(initial_sp);

    let restore_blocked = env.thr.blocked();
    let action = env.actions.lock()[sig.signo()].clone();
    let result = env
        .thr
        .handle_signal(&mut uctx, restore_blocked, &sig, &action);

    assert!(matches!(result, Some(SignalOSAction::Handler)));
    assert_eq!(uctx.ip(), test_handler as *const () as usize);
    assert!(uctx.sp() < initial_sp);
    assert_eq!(uctx.arg0(), Signo::SIGTERM as usize);
}

#[test]
fn dequeue_signal() {
    let env = TestEnv::new();
    let sig1 = SignalInfo::new_user(Signo::SIGINT, 9, 9);
    let sig2 = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    let mask = SignalSet::default();
    let allowed_mask = !mask;
    assert!(env.thr.send_signal(sig1.clone()));
    assert_eq!(env.proc.send_signal(sig2), Some(7));
    assert_eq!(
        env.thr.dequeue_signal(&allowed_mask).unwrap().signo(),
        Signo::SIGINT
    );
    assert_eq!(
        env.thr.dequeue_signal(&allowed_mask).unwrap().signo(),
        Signo::SIGTERM
    );
    assert!(env.thr.dequeue_signal(&allowed_mask).is_none());
}

#[test]
fn check_signals() {
    let env = TestEnv::new();
    let mut uctx: UserContext = unsafe { zeroed() };
    uctx.set_sp(0x8000_0000);

    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 1);
    assert_eq!(env.proc.send_signal(sig.clone()), Some(7));

    let (si, _os_action) = env.thr.check_signals(&mut uctx, None).unwrap();
    assert_eq!(si.signo(), Signo::SIGTERM);

    assert!(env.thr.send_signal(sig.clone()));
    let (si, _os_action) = env.thr.check_signals(&mut uctx, None).unwrap();
    assert_eq!(si.signo(), Signo::SIGTERM);
}

#[test]
fn restore() {
    unsafe extern "C" fn test_handler(_: i32) {}
    let env = TestEnv::new();
    let sig = SignalInfo::new_user(Signo::SIGTERM, 0, 1);
    env.actions.lock()[Signo::SIGTERM].disposition = SignalDisposition::Handler(test_handler);

    let sp = {
        let pool = POOL.lock().unwrap();
        pool.as_ptr() as usize + pool.len()
    };
    let mut initial: UserContext = unsafe { zeroed() };
    initial.set_sp(sp);
    initial.set_ip(0x219);
    let mut uctx_user = initial.clone();

    let restore_blocked = env.thr.blocked();
    let action = env.actions.lock()[sig.signo()].clone();
    env.thr
        .handle_signal(&mut uctx_user, restore_blocked, &sig, &action);

    let new_sp = uctx_user.sp() + 8;
    uctx_user.set_sp(new_sp);
    env.thr.restore(&mut uctx_user);

    assert_eq!(uctx_user.ip(), initial.ip());
    assert_eq!(uctx_user.sp(), initial.sp());
}
