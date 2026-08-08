//! RT task context storage and CPU-local switch transactions.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

use ax_cpu::{KernelTlsBase, TaskContext};
use cpu_local::{CurrentContext, CurrentThreadHeader, PreviousThreadBinding};

use crate::{MAX_RT_TASKS, executor::rt_task_entry};

const RT_STACK_SIZE: usize = 64 * 1024;
const RT_STACK_ALIGN: usize = 16;
const EXECUTOR_CONTEXT_ID: usize = 1;

pub(crate) static RT_RUNTIME: RtRuntime = RtRuntime::new();

#[repr(align(16))]
struct RtTaskStack {
    bytes: UnsafeCell<[u8; RT_STACK_SIZE]>,
}

impl RtTaskStack {
    const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([0; RT_STACK_SIZE]),
        }
    }

    fn top(&self) -> usize {
        let base = self.bytes.get().cast::<u8>() as usize;
        align_down(base + RT_STACK_SIZE, RT_STACK_ALIGN)
    }
}

unsafe impl Sync for RtTaskStack {}

pub(crate) struct RtContext {
    context: UnsafeCell<MaybeUninit<TaskContext>>,
    header: CurrentThreadHeader,
    stack: RtTaskStack,
}

impl RtContext {
    const fn new(context_id: usize) -> Self {
        Self {
            context: UnsafeCell::new(MaybeUninit::uninit()),
            header: CurrentThreadHeader::new(
                CurrentContext::from_raw(context_id).expect("RT context IDs must be non-zero"),
            ),
            stack: RtTaskStack::new(),
        }
    }

    fn current_header(&self) -> Pin<&CurrentThreadHeader> {
        // SAFETY: RT contexts are stored in a static runtime and never moved.
        unsafe { Pin::new_unchecked(&self.header) }
    }

    fn context(&self) -> &TaskContext {
        // SAFETY: immutable context access is used only for the incoming task
        // while RT scheduling is serialized on the single reserved CPU.
        unsafe { (*self.context.get()).assume_init_ref() }
    }

    fn init_context(&self) {
        // SAFETY: each RT context is initialized exactly once before the RT
        // executor can switch to any task.
        unsafe { (*self.context.get()).write(TaskContext::new()) };
    }

    unsafe fn context_mut_ptr(&self) -> *mut TaskContext {
        // SAFETY: the caller owns the serialized RT scheduler transition that
        // permits mutable access to this initialized context.
        unsafe { (*self.context.get()).assume_init_mut() }
    }
}

pub(crate) struct RtRuntime {
    pub(crate) executor: RtContext,
    pub(crate) tasks: [RtContext; MAX_RT_TASKS],
    current_task: AtomicUsize,
    previous_binding: UnsafeCell<MaybeUninit<PreviousThreadBinding>>,
    has_previous_binding: AtomicUsize,
}

unsafe impl Sync for RtRuntime {}

impl RtRuntime {
    const fn new() -> Self {
        Self {
            executor: RtContext::new(EXECUTOR_CONTEXT_ID),
            tasks: [
                RtContext::new(2),
                RtContext::new(3),
                RtContext::new(4),
                RtContext::new(5),
                RtContext::new(6),
                RtContext::new(7),
                RtContext::new(8),
                RtContext::new(9),
            ],
            current_task: AtomicUsize::new(usize::MAX),
            previous_binding: UnsafeCell::new(MaybeUninit::uninit()),
            has_previous_binding: AtomicUsize::new(0),
        }
    }

    pub(crate) fn init_task_contexts(&self, task_count: usize) {
        self.executor.init_context();
        // SAFETY: initialization runs once before any RT task can execute.
        unsafe { &mut *self.executor.context_mut_ptr() }
            .set_current_header(NonNull::from(&self.executor.header));
        // SAFETY: the RT entry runs after the OS installed this CPU's CPU-local
        // area and before the CPU enters any ordinary scheduler path.
        unsafe {
            cpu_local::with_cpu_pin(|pin| {
                cpu_local::install_bootstrap_thread(pin, self.executor.current_header())
                    .expect("RT executor bootstrap thread install failed")
            })
        }
        .expect("RT bootstrap requires an installed CPU-local area");

        for task_id in 0..task_count {
            let context = &self.tasks[task_id];
            context.init_context();
            let context_pointer = NonNull::from(&context.header);
            // SAFETY: initialization runs once before any RT task can execute.
            let task_context = unsafe { &mut *context.context_mut_ptr() };
            task_context.init(
                rt_task_entry as *const () as usize,
                ax_memory_addr::VirtAddr::from(context.stack.top()),
                KernelTlsBase::new(0),
            );
            task_context.set_current_header(context_pointer);
        }
    }

    pub(crate) fn switch_to_task(&self, task_id: usize) {
        self.current_task.store(task_id, Ordering::Release);
        self.switch_between(&self.executor, &self.tasks[task_id]);
    }

    pub(crate) fn switch_to_executor(&self, task_id: usize) {
        self.current_task.store(usize::MAX, Ordering::Release);
        self.switch_between(&self.tasks[task_id], &self.executor);
    }

    pub(crate) fn current_running_task(&self, task_count: usize) -> usize {
        let task_id = self.current_task.load(Ordering::Acquire);
        assert!(
            task_id < task_count,
            "RT task operation must run in an RT task"
        );
        task_id
    }

    pub(crate) fn finish_previous_binding(&self, previous: &RtContext) {
        if self.has_previous_binding.swap(0, Ordering::AcqRel) == 0 {
            return;
        }

        // SAFETY: only the incoming RT context consumes the one stored previous
        // binding after a completed switch. The previous context is static.
        let binding = unsafe { (*self.previous_binding.get()).assume_init_read() };
        unsafe {
            binding
                .finish(previous.current_header())
                .expect("RT switch tail must match previous context")
        };
    }

    fn switch_between(&self, previous: &RtContext, next: &RtContext) {
        // SAFETY: RT scheduling is serialized on the reserved CPU; no RT task
        // migrates, and this path never enters the ordinary host scheduler.
        unsafe {
            cpu_local::with_cpu_pin(|pin| {
                let (prepared, previous_binding) = cpu_local::prepare_thread_switch(
                    pin,
                    previous.current_header(),
                    next.current_header(),
                )
                .expect("RT context switch preparation failed");
                (&mut *previous.context_mut_ptr()).prepare_switch_to(next.context());

                (*self.previous_binding.get()).write(previous_binding);
                self.has_previous_binding.store(1, Ordering::Release);

                (&mut *previous.context_mut_ptr()).switch_to_prepared(next.context(), prepared);
            })
        }
        .expect("RT context switches require an installed CPU-local area");
    }
}

const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}
