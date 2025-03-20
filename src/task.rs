use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use arceos_posix_api::FD_TABLE;
use axerrno::{AxError, AxResult};
use axfs::{CURRENT_DIR, CURRENT_DIR_PATH};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, Ordering},
};
use memory_addr::VirtAddrRange;
use spin::Once;

use crate::{
    copy_from_kernel,
    ctypes::{CloneFlags, TimeStat, WaitStatus},
};
use axhal::{
    arch::{TrapFrame, UspaceContext},
    time::{NANOS_PER_MICROS, NANOS_PER_SEC, monotonic_time_nanos},
};
use axmm::{AddrSpace, kernel_aspace};
use axns::{AxNamespace, AxNamespaceIf};
use axsync::Mutex;
use axtask::{AxTaskRef, TaskExtRef, TaskInner, current};

/// Task extended data for the monolithic kernel.
pub struct TaskExt {
    /// The process ID.
    pub proc_id: usize,
    /// The parent process ID.
    pub parent_id: AtomicU64,
    /// children process
    pub children: Mutex<Vec<AxTaskRef>>,
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
    /// The time statistics
    pub time: UnsafeCell<TimeStat>,
    /// The user heap bottom
    pub heap_bottom: AtomicU64,
    /// The user heap top
    pub heap_top: AtomicU64,
}

impl TaskExt {
    pub fn new(
        proc_id: usize,
        uctx: UspaceContext,
        aspace: Arc<Mutex<AddrSpace>>,
        heap_bottom: u64,
    ) -> Self {
        Self {
            proc_id,
            parent_id: AtomicU64::new(1),
            children: Mutex::new(Vec::new()),
            uctx,
            clear_child_tid: AtomicU64::new(0),
            aspace,
            ns: AxNamespace::new_thread_local(),
            time: TimeStat::new().into(),
            heap_bottom: AtomicU64::new(heap_bottom),
            heap_top: AtomicU64::new(heap_bottom),
        }
    }

    pub fn clone_task(
        &self,
        flags: usize,
        stack: Option<usize>,
        _ptid: usize,
        _tls: usize,
        _ctid: usize,
    ) -> AxResult<u64> {
        let _clone_flags = CloneFlags::from_bits((flags & !0x3f) as u32).unwrap();

        let mut new_task = TaskInner::new(
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
            current().id_name(),
            axconfig::plat::KERNEL_STACK_SIZE,
        );

        let current_task = current();
        let mut current_aspace = current_task.task_ext().aspace.lock();
        let mut new_aspace = current_aspace.clone_or_err()?;
        copy_from_kernel(&mut new_aspace)?;
        new_task
            .ctx_mut()
            .set_page_table_root(new_aspace.page_table_root());

        let trap_frame = read_trapframe_from_kstack(current_task.get_kernel_stack_top().unwrap());
        let mut new_uctx = UspaceContext::from(&trap_frame);
        if let Some(stack) = stack {
            new_uctx.set_sp(stack);
        }
        // Skip current instruction
        #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
        new_uctx.set_ip(new_uctx.get_ip() + 4);
        new_uctx.set_retval(0);
        let return_id: u64 = new_task.id().as_u64();
        let new_task_ext = TaskExt::new(
            return_id as usize,
            new_uctx,
            Arc::new(Mutex::new(new_aspace)),
            axconfig::plat::USER_HEAP_BASE as _,
        );
        new_task_ext.ns_init_new();
        new_task.init_task_ext(new_task_ext);
        let new_task_ref = axtask::spawn_task(new_task);
        current_task.task_ext().children.lock().push(new_task_ref);
        Ok(return_id)
    }

    pub(crate) fn clear_child_tid(&self) -> u64 {
        self.clear_child_tid
            .load(core::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn set_clear_child_tid(&self, clear_child_tid: u64) {
        self.clear_child_tid
            .store(clear_child_tid, core::sync::atomic::Ordering::Relaxed);
    }

    pub(crate) fn get_parent(&self) -> u64 {
        self.parent_id.load(Ordering::Acquire)
    }

    #[allow(unused)]
    pub(crate) fn set_parent(&self, parent_id: u64) {
        self.parent_id.store(parent_id, Ordering::Release);
    }

    pub(crate) fn ns_init_new(&self) {
        FD_TABLE
            .deref_from(&self.ns)
            .init_new(FD_TABLE.copy_inner());
        CURRENT_DIR
            .deref_from(&self.ns)
            .init_new(CURRENT_DIR.copy_inner());
        CURRENT_DIR_PATH
            .deref_from(&self.ns)
            .init_new(CURRENT_DIR_PATH.copy_inner());
    }

    pub(crate) fn time_stat_from_kernel_to_user(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).switch_into_user_mode(current_tick);
        }
    }

    pub(crate) fn time_stat_from_user_to_kernel(&self, current_tick: usize) {
        let time = self.time.get();
        unsafe {
            (*time).switch_into_kernel_mode(current_tick);
        }
    }

    pub(crate) fn time_stat_output(&self) -> (usize, usize) {
        let time = self.time.get();
        unsafe { (*time).output() }
    }

    pub(crate) fn get_heap_bottom(&self) -> u64 {
        self.heap_bottom.load(Ordering::Acquire)
    }

    #[allow(unused)]
    pub(crate) fn set_heap_bottom(&self, bottom: u64) {
        self.heap_bottom.store(bottom, Ordering::Release)
    }

    pub(crate) fn get_heap_top(&self) -> u64 {
        self.heap_top.load(Ordering::Acquire)
    }

    pub(crate) fn set_heap_top(&self, top: u64) {
        self.heap_top.store(top, Ordering::Release)
    }
}

struct AxNamespaceImpl;
#[crate_interface::impl_interface]
impl AxNamespaceIf for AxNamespaceImpl {
    fn current_namespace_base() -> *mut u8 {
        // Namespace for kernel task
        static KERNEL_NS_BASE: Once<usize> = Once::new();
        let current = axtask::current();
        // Safety: We only check whether the task extended data is null and do not access it.
        if unsafe { current.task_ext_ptr() }.is_null() {
            return *(KERNEL_NS_BASE.call_once(|| {
                let global_ns = AxNamespace::global();
                let layout = Layout::from_size_align(global_ns.size(), 64).unwrap();
                // Safety: The global namespace is a static readonly variable and will not be dropped.
                let dst = unsafe { alloc::alloc::alloc(layout) };
                let src = global_ns.base();
                unsafe { core::ptr::copy_nonoverlapping(src, dst, global_ns.size()) };
                dst as usize
            })) as *mut u8;
        }
        current.task_ext().ns.base()
    }
}

impl Drop for TaskExt {
    fn drop(&mut self) {
        if !cfg!(target_arch = "aarch64") && !cfg!(target_arch = "loongarch64") {
            // See [`crate::new_user_aspace`]
            let kernel = kernel_aspace().lock();
            self.aspace
                .lock()
                .clear_mappings(VirtAddrRange::from_start_size(kernel.base(), kernel.size()));
        }
    }
}

axtask::def_task_ext!(TaskExt);

pub fn spawn_user_task(
    aspace: Arc<Mutex<AddrSpace>>,
    uctx: UspaceContext,
    heap_bottom: u64,
) -> AxTaskRef {
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
    task.init_task_ext(TaskExt::new(
        task.id().as_u64() as usize,
        uctx,
        aspace,
        heap_bottom,
    ));
    task.task_ext().ns_init_new();
    axtask::spawn_task(task)
}

#[allow(unused)]
pub fn write_trapframe_to_kstack(kstack_top: usize, trap_frame: &TrapFrame) {
    let trap_frame_size = core::mem::size_of::<TrapFrame>();
    let trap_frame_ptr = (kstack_top - trap_frame_size) as *mut TrapFrame;
    unsafe {
        *trap_frame_ptr = *trap_frame;
    }
}

pub fn read_trapframe_from_kstack(kstack_top: usize) -> TrapFrame {
    let trap_frame_size = core::mem::size_of::<TrapFrame>();
    let trap_frame_ptr = (kstack_top - trap_frame_size) as *mut TrapFrame;
    unsafe { *trap_frame_ptr }
}

pub fn wait_pid(pid: i32, exit_code_ptr: *mut i32) -> Result<u64, WaitStatus> {
    let curr_task = current();
    let mut exit_task_id: usize = 0;
    let mut answer_id: u64 = 0;
    let mut answer_status = WaitStatus::NotExist;

    for (index, child) in curr_task.task_ext().children.lock().iter().enumerate() {
        if pid <= 0 {
            if pid == 0 {
                warn!("Don't support for process group.");
            }

            answer_status = WaitStatus::Running;
            if child.state() == axtask::TaskState::Exited {
                let exit_code = child.exit_code();
                answer_status = WaitStatus::Exited;
                info!(
                    "wait pid _{}_ with code _{}_",
                    child.id().as_u64(),
                    exit_code
                );
                exit_task_id = index;
                if !exit_code_ptr.is_null() {
                    unsafe {
                        *exit_code_ptr = exit_code << 8;
                    }
                }
                answer_id = child.id().as_u64();
                break;
            }
        } else if child.id().as_u64() == pid as u64 {
            if let Some(exit_code) = child.join() {
                answer_status = WaitStatus::Exited;
                info!(
                    "wait pid _{}_ with code _{:?}_",
                    child.id().as_u64(),
                    exit_code
                );
                exit_task_id = index;
                if !exit_code_ptr.is_null() {
                    unsafe {
                        *exit_code_ptr = exit_code << 8;
                    }
                }
                answer_id = child.id().as_u64();
            } else {
                answer_status = WaitStatus::Running;
            }
            break;
        }
    }

    if answer_status == WaitStatus::Running {
        axtask::yield_now();
    }

    if answer_status == WaitStatus::Exited {
        curr_task.task_ext().children.lock().remove(exit_task_id);
        return Ok(answer_id);
    }
    Err(answer_status)
}

pub fn exec(name: &str, args: &[String], envs: &[String]) -> AxResult<()> {
    let current_task = current();

    let program_name = name.to_string();

    let mut aspace = current_task.task_ext().aspace.lock();
    if Arc::strong_count(&current_task.task_ext().aspace) != 1 {
        warn!("Address space is shared by multiple tasks, exec is not supported.");
        return Err(AxError::Unsupported);
    }

    aspace.unmap_user_areas()?;
    axhal::arch::flush_tlb(None);

    let (entry_point, user_stack_base) = crate::mm::load_user_app(&mut aspace, args, envs)
        .map_err(|_| {
            error!("Failed to load app {}", program_name);
            AxError::NotFound
        })?;
    current_task.set_name(&program_name);
    drop(aspace);

    let task_ext = unsafe { &mut *(current_task.task_ext_ptr() as *mut TaskExt) };
    task_ext.uctx = UspaceContext::new(entry_point.as_usize(), user_stack_base, 0);

    unsafe {
        task_ext.uctx.enter_uspace(
            current_task
                .kernel_stack_top()
                .expect("No kernel stack top"),
        );
    }
}

pub fn time_stat_from_kernel_to_user() {
    let curr_task = current();
    curr_task
        .task_ext()
        .time_stat_from_kernel_to_user(monotonic_time_nanos() as usize);
}

pub fn time_stat_from_user_to_kernel() {
    let curr_task = current();
    curr_task
        .task_ext()
        .time_stat_from_user_to_kernel(monotonic_time_nanos() as usize);
}

pub fn time_stat_output() -> (usize, usize, usize, usize) {
    let curr_task = current();
    let (utime_ns, stime_ns) = curr_task.task_ext().time_stat_output();
    (
        utime_ns / NANOS_PER_SEC as usize,
        utime_ns / NANOS_PER_MICROS as usize,
        stime_ns / NANOS_PER_SEC as usize,
        stime_ns / NANOS_PER_MICROS as usize,
    )
}
