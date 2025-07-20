use core::sync::atomic::{AtomicI32, Ordering};

static IPC_ID: AtomicI32 = AtomicI32::new(0);

fn next_ipc_id() -> i32 {
    IPC_ID.fetch_add(1, Ordering::Relaxed)
}

mod shm;

pub use self::shm::*;
