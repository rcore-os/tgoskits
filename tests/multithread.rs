#![feature(maybe_uninit_write_slice)]

use std::{
    mem::{MaybeUninit, zeroed},
    sync::{
        Arc, LazyLock, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use axcpu::uspace::UserContext;
use extern_trait::extern_trait;
use kspin::SpinNoIrq;
use starry_signal::{
    SignalDisposition, SignalInfo, SignalOSAction, SignalSet, Signo,
    api::{ProcessSignalManager, SignalActions, ThreadSignalManager},
};
use starry_vm::{VmError, VmIo, VmResult};

fn wait_until<F>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> bool,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        thread::sleep(Duration::from_millis(1));
    }
    false
}

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
        if start < base {
            return Err(VmError::BadAddress);
        }
        let offset = start - base;
        if offset + buf.len() > self.0.len() {
            return Err(VmError::BadAddress);
        }
        let slice = &self.0[offset..offset + buf.len()];
        buf.write_copy_of_slice(slice);
        Ok(())
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        let base = self.0.as_ptr() as usize;
        if start < base {
            return Err(VmError::BadAddress);
        }
        let offset = start - base;
        if offset + buf.len() > self.0.len() {
            return Err(VmError::BadAddress);
        }
        let slice = &mut self.0[offset..offset + buf.len()];
        slice.copy_from_slice(buf);
        Ok(())
    }
}

#[test]
fn thread_send_signal() {
    let env = TestEnv::new();
    let sig = SignalInfo::new_user(Signo::SIGTERM, 9, 9);

    let thr = env.thr.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let _ = thr.send_signal(sig);
    });

    let res = wait_until(Duration::from_millis(100), || {
        env.thr.pending().has(Signo::SIGTERM) || env.proc.pending().has(Signo::SIGTERM)
    });

    assert!(res);
}

#[test]
fn thread_blocked() {
    let env = TestEnv::new();
    let sig = SignalInfo::new_user(Signo::SIGTERM, 9, 9);

    let mut blocked = SignalSet::default();
    blocked.add(Signo::SIGTERM);
    let prev = env.thr.set_blocked(blocked);
    assert!(!prev.has(Signo::SIGTERM));
    assert!(env.thr.signal_blocked(Signo::SIGTERM));

    let thr = env.thr.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        let _ = thr.send_signal(sig);
    });

    let pending_res = wait_until(Duration::from_millis(100), || {
        env.thr.pending().has(Signo::SIGTERM)
    });
    assert!(pending_res);

    env.thr.set_blocked(SignalSet::default());
    assert!(!env.thr.signal_blocked(Signo::SIGTERM));

    let uctx = Arc::new(SpinNoIrq::new(unsafe { zeroed::<UserContext>() }));
    uctx.lock().set_sp(0x8000_0000);
    let res = wait_until(Duration::from_millis(100), || {
        let mut uctx_ref = uctx.lock().clone();
        if let Some((si, _)) = env.thr.check_signals(&mut uctx_ref, None) {
            assert_eq!(si.signo(), Signo::SIGTERM);
            true
        } else {
            false
        }
    });
    assert!(res);
}

#[test]
fn thread_handler() {
    unsafe extern "C" fn test_handler(_: i32) {}

    let env = TestEnv::new();
    env.actions.lock()[Signo::SIGTERM].disposition = SignalDisposition::Handler(test_handler);

    let uctx = Arc::new(SpinNoIrq::new(unsafe { zeroed::<UserContext>() }));
    let initial_sp = {
        let pool = POOL.lock().unwrap();
        pool.as_ptr() as usize + pool.len()
    };
    uctx.lock().set_sp(initial_sp);

    let first = SignalInfo::new_user(Signo::SIGTERM, 9, 9);
    assert!(env.thr.send_signal(first.clone()));
    let (si, action) = {
        let mut guard = uctx.lock();
        env.thr.check_signals(&mut guard, None).unwrap()
    };
    assert_eq!(si.signo(), Signo::SIGTERM);
    assert!(matches!(action, SignalOSAction::Handler));
    assert!(env.thr.signal_blocked(Signo::SIGTERM));

    let thr = env.thr.clone();
    thread::spawn(move || {
        let _ = thr.send_signal(SignalInfo::new_user(Signo::SIGINT, 2, 2));
        let _ = thr.send_signal(SignalInfo::new_user(Signo::SIGTERM, 3, 3));
    });

    let pending_res = wait_until(Duration::from_millis(200), || {
        env.thr.pending().has(Signo::SIGTERM)
    });
    assert!(pending_res);

    let pending_res = wait_until(Duration::from_millis(200), || {
        env.thr.pending().has(Signo::SIGINT)
    });
    assert!(pending_res);

    let frame_sp = uctx.lock().sp() + 8;
    {
        let mut guard = uctx.lock();
        guard.set_sp(frame_sp);
        env.thr.restore(&mut guard);
    }
    assert!(!env.thr.signal_blocked(Signo::SIGTERM));

    let delivered = Arc::new(AtomicUsize::new(0));
    let delivered_result = wait_until(Duration::from_millis(200), || {
        let thr = env.thr.clone();
        let delivered_ref = delivered.clone();
        let uctx_ref = uctx.clone();
        if let Some((sig, _)) = thr.check_signals(&mut *uctx_ref.lock(), None) {
            assert!(matches!(sig.signo(), Signo::SIGINT | Signo::SIGTERM));
            delivered_ref.fetch_add(1, Ordering::SeqCst);
        }
        delivered_ref.load(Ordering::SeqCst) >= 2
    });

    assert!(delivered_result);
}
