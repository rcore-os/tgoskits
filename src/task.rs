use core::sync::atomic::AtomicU64;

use alloc::sync::Arc;

use axhal::arch::UspaceContext;
use axmm::AddrSpace;
use axns::{AxNamespace, AxNamespaceIf};
use axsync::Mutex;
use axtask::{AxTaskRef, TaskExtRef, TaskInner};

/// Task extended data for the monolithic kernel.
pub struct TaskExt {
    /// The process ID.
    pub proc_id: usize,
    /// The clear thread tid field
    ///
    /// See <https://manpages.debian.org/unstable/manpages-dev/set_tid_address.2.en.html#clear_child_tid>
    ///
    /// When the thread exits, the kernel clears the word at this address if it is not NULL.
    clear_child_tid: AtomicU64,
    /// The user space context.
    pub uctx: UspaceContext,
    /// The virtual memory address space.
    pub aspace: Arc<Mutex<AddrSpace>>,
    /// The resource namespace
    pub ns: AxNamespace,
}

impl TaskExt {
    pub fn new(uctx: UspaceContext, aspace: Arc<Mutex<AddrSpace>>) -> Self {
        Self {
            proc_id: 233,
            uctx,
            clear_child_tid: AtomicU64::new(0),
            aspace,
            ns: AxNamespace::new_thread_local(),
        }
    }

    pub(crate) fn clear_child_tid(&self) -> u64 {
        self.clear_child_tid
            .load(core::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn set_clear_child_tid(&self, clear_child_tid: u64) {
        self.clear_child_tid
            .store(clear_child_tid, core::sync::atomic::Ordering::Relaxed);
    }
}

struct AxNamespaceImpl;

#[crate_interface::impl_interface]
impl AxNamespaceIf for AxNamespaceImpl {
    fn current_namespace_base() -> *mut u8 {
        let current = axtask::current();
        // Safety: We only check whether the task extended data is null and do not access it.
        if unsafe { current.task_ext_ptr() }.is_null() {
            return axns::AxNamespace::global().base();
        }
        current.task_ext().ns.base()
    }
}

axtask::def_task_ext!(TaskExt);

pub fn spawn_user_task(aspace: Arc<Mutex<AddrSpace>>, uctx: UspaceContext) -> AxTaskRef {
    let mut task = TaskInner::new(
        || {
            let curr = axtask::current();
            let kstack_top = curr.kernel_stack_top().unwrap();
            info!(
                "Enter user space: entry={:#x}, ustack={:#x}, kstack={:#x}",
                curr.task_ext().uctx.get_ip(),
                curr.task_ext().uctx.get_sp(),
                kstack_top,
            );
            unsafe { curr.task_ext().uctx.enter_uspace(kstack_top) };
        },
        "userboot".into(),
        axconfig::plat::KERNEL_STACK_SIZE,
    );
    task.ctx_mut()
        .set_page_table_root(aspace.lock().page_table_root());
    task.init_task_ext(TaskExt::new(uctx, aspace));
    axtask::spawn_task(task)
}
