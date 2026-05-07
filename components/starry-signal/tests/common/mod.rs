use std::{
    mem::MaybeUninit,
    sync::{
        Arc, LazyLock, Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use ax_kspin::SpinNoIrq;
use extern_trait::extern_trait;
use starry_signal::api::{ProcessSignalManager, SignalActions, ThreadSignalManager};
use starry_vm::{VmError, VmIo, VmResult};

static POOL: LazyLock<Mutex<Box<[u8]>>> = LazyLock::new(|| {
    let size = 0x0100_0000; // 16 MiB
    Mutex::new(vec![0; size].into_boxed_slice())
});

const TEST_STACK_SIZE: usize = 0x1_0000;
static NEXT_STACK_OFFSET: AtomicUsize = AtomicUsize::new(0);

pub fn initial_sp() -> usize {
    let pool = POOL.lock().unwrap();
    let offset = NEXT_STACK_OFFSET.fetch_add(TEST_STACK_SIZE, Ordering::Relaxed);
    assert!(
        offset + TEST_STACK_SIZE <= pool.len(),
        "starry-signal test VM stack pool exhausted"
    );
    pool.as_ptr() as usize + offset + TEST_STACK_SIZE
}

struct Vm(MutexGuard<'static, Box<[u8]>>);

#[extern_trait]
unsafe impl VmIo for Vm {
    fn new() -> Self {
        let pool = POOL.lock().unwrap();
        Vm(pool)
    }

    fn read(&mut self, start: usize, buf: &mut [MaybeUninit<u8>]) -> VmResult {
        let base = self.0.as_ptr() as usize;
        let offset = start.checked_sub(base).ok_or(VmError::BadAddress)?;
        if offset.checked_add(buf.len()).ok_or(VmError::BadAddress)? > self.0.len() {
            return Err(VmError::BadAddress);
        }
        let slice = &self.0[offset..offset + buf.len()];
        buf.write_copy_of_slice(slice);
        Ok(())
    }

    fn write(&mut self, start: usize, buf: &[u8]) -> VmResult {
        let base = self.0.as_ptr() as usize;
        let offset = start.checked_sub(base).ok_or(VmError::BadAddress)?;
        if offset.checked_add(buf.len()).ok_or(VmError::BadAddress)? > self.0.len() {
            return Err(VmError::BadAddress);
        }
        let slice = &mut self.0[offset..offset + buf.len()];
        slice.copy_from_slice(buf);
        Ok(())
    }
}

pub const TID: u32 = 7;

pub fn new_test_env() -> (Arc<ProcessSignalManager>, Arc<ThreadSignalManager>) {
    let proc = Arc::new(ProcessSignalManager::new(
        Arc::new(SpinNoIrq::new(SignalActions::default())),
        0,
    ));
    let thr = ThreadSignalManager::new(TID, proc.clone());
    (proc, thr)
}
