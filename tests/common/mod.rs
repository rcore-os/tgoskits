use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use axprocess::{Process, ProcessBuilder};

static PID: AtomicU32 = AtomicU32::new(0);

fn new_pid() -> u32 {
    PID.fetch_add(1, Ordering::SeqCst)
}

pub fn new_init() -> Arc<Process> {
    ProcessBuilder::new(new_pid()).build()
}

pub fn fork(parent: &Arc<Process>) -> Arc<Process> {
    ProcessBuilder::new(new_pid())
        .parent(parent.clone())
        .build()
}
