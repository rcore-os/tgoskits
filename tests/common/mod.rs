use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use axprocess::Process;
use ctor::ctor;

static PID: AtomicU32 = AtomicU32::new(0);

fn alloc_pid() -> u32 {
    PID.fetch_add(1, Ordering::SeqCst)
}

#[ctor]
fn init() {
    Process::new_init(alloc_pid()).build();
}

pub trait ProcessExt {
    fn new_child(&self) -> Self;
}

impl ProcessExt for Arc<Process> {
    fn new_child(&self) -> Self {
        self.fork(alloc_pid()).build()
    }
}
