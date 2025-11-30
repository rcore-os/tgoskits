//! 单CPU异步执行器
//!
//! 基于embassy设计的单CPU异步执行器，支持任务优先级调度。
//! 特性：
//! - Wake任务优先执行
//! - 超过1秒未执行的任务获得优先级提升
//! - 使用alloc::进行动态内存分配
//! - 使用IrqSpinlock保证中断安全

use core::future::Future;
use core::time::Duration;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::collections::BinaryHeap;
use alloc::collections::VecDeque;
use alloc::sync::Arc;

use crate::os::sync::spinlock::IrqSpinlock;

use super::task::{TaskHandle, TaskId, TaskMetadata, TaskPriority, TaskRef};

/// 全局唤醒队列
static WAKE_QUEUE: IrqSpinlock<VecDeque<TaskId>> = IrqSpinlock::new(VecDeque::new());

/// 将任务ID添加到唤醒队列
pub fn enqueue_wakeup(task_id: TaskId) {
    let mut queue = WAKE_QUEUE.lock();
    if !queue.contains(&task_id) {
        queue.push_back(task_id);
    }
}

/// 单CPU异步执行器
pub struct SingleCpuExecutor {
    /// 任务优先级队列
    task_queue: IrqSpinlock<BinaryHeap<OrderedTask>>,
    /// 任务注册表（ID -> TaskRef）
    task_registry: IrqSpinlock<alloc::collections::BTreeMap<TaskId, Arc<TaskRef>>>,
    /// 是否正在运行
    is_running: IrqSpinlock<bool>,
    /// 超时阈值（毫秒）
    timeout_ms: u64,
}

/// 有序任务包装器，用于优先级队列
#[derive(Debug)]
struct OrderedTask {
    /// 任务优先级
    priority: TaskPriority,
    /// 任务引用
    task_ref: Arc<TaskRef>,
}

impl OrderedTask {
    /// 创建新的有序任务
    fn new(task_ref: Arc<TaskRef>) -> Self {
        Self {
            priority: task_ref.priority(),
            task_ref,
        }
    }
}

// 实现排序：优先级高的在前，时间戳小的在前，ID小的在前
impl PartialEq for OrderedTask {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority
    }
}

impl Eq for OrderedTask {}

impl PartialOrd for OrderedTask {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedTask {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // 注意：BinaryHeap是最大堆，所以需要反向比较
        other.priority.cmp(&self.priority)
    }
}

impl SingleCpuExecutor {
    /// 创建新的执行器实例
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(1))
    }

    /// 使用自定义超时时间创建执行器
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            task_queue: IrqSpinlock::new(BinaryHeap::new()),
            task_registry: IrqSpinlock::new(BTreeMap::new()),
            is_running: IrqSpinlock::new(false),
            timeout_ms: timeout.as_millis() as u64,
        }
    }

    /// 获取全局执行器实例
    pub fn global() -> &'static Self {
        use core::sync::atomic::{AtomicPtr, Ordering};

        static EXECUTOR_PTR: AtomicPtr<SingleCpuExecutor> = AtomicPtr::new(core::ptr::null_mut());

        let ptr = EXECUTOR_PTR.load(Ordering::Acquire);
        if ptr.is_null() {
            // 创建新的执行器实例
            let executor = Box::leak(Box::new(SingleCpuExecutor::new()));
            match EXECUTOR_PTR.compare_exchange(
                core::ptr::null_mut(),
                executor,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => executor,
                Err(existing) => {
                    // 其他线程已经创建了执行器，使用现有的
                    unsafe {
                        // 需要处理内存泄漏问题，这里简化处理
                        let _ = Box::from_raw(executor);
                    }
                    unsafe { &*existing }
                }
            }
        } else {
            unsafe { &*ptr }
        }
    }

    /// 生成异步任务
    pub fn spawn<F, T>(&self, future: F) -> TaskHandle
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let task_id = TaskId::new();
        let metadata = TaskMetadata::new(task_id);

        // 转换Future为返回()的Future
        let wrapped_future = async move {
            let _ = future.await;
        };

        let task_ref = Arc::new(TaskRef::new(wrapped_future, metadata));
        let handle = TaskHandle::with_ref(task_id, task_ref.clone());

        // 注册任务
        {
            let mut registry = self.task_registry.lock();
            registry.insert(task_id, task_ref);
        }

        // 添加到任务队列
        self.add_task_to_queue(task_id);

        debug!("Spawned task {task_id:?}",);
        handle
    }

    /// 添加任务到调度队列
    fn add_task_to_queue(&self, task_id: TaskId) {
        let registry = self.task_registry.lock();
        if let Some(task_ref) = registry.get(&task_id) {
            let ordered_task = OrderedTask::new(task_ref.clone());
            let mut queue = self.task_queue.lock();
            queue.push(ordered_task);
        }
    }

    /// 唤醒指定任务
    pub fn wake_by_id(&self, task_id: TaskId) -> bool {
        let registry = self.task_registry.lock();
        if let Some(task_ref) = registry.get(&task_id) {
            task_ref.metadata.lock().mark_woken();
            self.add_task_to_queue(task_id);
            true
        } else {
            false
        }
    }

    /// 处理单个任务
    fn process_one_task(&self) -> bool {
        let ordered_task = {
            let mut queue = self.task_queue.lock();
            queue.pop()
        };

        if let Some(ordered_task) = ordered_task {
            let task_ref = ordered_task.task_ref.clone();
            let task_id = task_ref.id();

            // 检查任务是否已完成
            if task_ref.is_completed() {
                // 清理已完成的任务
                let mut registry = self.task_registry.lock();
                registry.remove(&task_id);
                debug!("Task {task_id:?} completed and cleaned up");
                return true;
            }

            // 检查任务状态和是否需要执行
            let should_execute = {
                let mut metadata = task_ref.metadata.lock();

                // 唤醒状态的任务直接执行
                if metadata.state == super::task::TaskState::Woken {
                    true
                } else if metadata.is_expired(self.timeout_ms) {
                    // 超时的任务提升优先级并执行
                    metadata.mark_woken();
                    debug!("Task {task_id:?} expired, promoting priority");
                    true
                } else {
                    // Pending 状态且未超时的任务，不执行
                    false
                }
            };

            if !should_execute {
                // 不执行的任务，不重新排队（等待被 wake 唤醒）
                // 直接返回 true 表示处理了一个任务（跳过它）
                return true;
            }

            // 创建Waker并轮询任务
            let waker = ExecutorWaker::new(task_id);
            let waker = waker.into_waker();

            match task_ref.poll(&waker) {
                core::task::Poll::Ready(()) => {
                    // 任务已完成，清理注册表
                    let mut registry = self.task_registry.lock();
                    registry.remove(&task_id);
                    debug!("Task {task_id:?} completed");
                    true
                }
                core::task::Poll::Pending => {
                    // 任务返回Pending，不需要重新排队
                    // 等待Waker被调用后再加入队列
                    debug!("Task {task_id:?} pending, waiting for wake");
                    true
                }
            }
        } else {
            false // 没有任务可处理
        }
    }

    /// 运行一次任务调度
    pub fn tick(&self) {
        // 首先处理唤醒队列中的任务
        self.process_wake_queue();

        // 处理多个任务，直到队列为空或达到合理限制
        let mut processed = 0;
        const MAX_TASKS_PER_TICK: usize = 10;

        while processed < MAX_TASKS_PER_TICK && self.process_one_task() {
            processed += 1;
        }

        if processed == 0 {
            log::debug!("No tasks to process in this tick");
        }
    }

    /// 处理唤醒队列
    fn process_wake_queue(&self) {
        loop {
            let task_id = {
                let mut queue = WAKE_QUEUE.lock();
                queue.pop_front()
            };

            if let Some(task_id) = task_id {
                // 标记任务为唤醒状态并加入调度队列
                let registry = self.task_registry.lock();
                if let Some(task_ref) = registry.get(&task_id) {
                    task_ref.metadata.lock().mark_woken();
                    let ordered_task = OrderedTask::new(task_ref.clone());
                    let mut queue = self.task_queue.lock();
                    queue.push(ordered_task);
                }
            } else {
                break;
            }
        }
    }

    /// 运行直到所有任务完成
    pub fn run_until_completion(&self) {
        *self.is_running.lock() = true;

        debug!("Executor started, running until completion");

        while self.has_pending_tasks() {
            self.tick();

            // 简单的CPU让步，避免过度占用
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }

        *self.is_running.lock() = false;
        debug!("Executor finished, all tasks completed");
    }

    /// 检查是否有待处理的任务
    pub fn has_pending_tasks(&self) -> bool {
        // 检查唤醒队列
        if !WAKE_QUEUE.lock().is_empty() {
            return true;
        }
        // 检查任务队列
        if !self.task_queue.lock().is_empty() {
            return true;
        }
        // 检查是否有未完成的任务（可能在等待被唤醒）
        let registry = self.task_registry.lock();
        !registry.is_empty()
    }

    /// 获取当前任务数量
    pub fn task_count(&self) -> usize {
        self.task_registry.lock().len()
    }

    /// 获取队列中的任务数量
    pub fn queued_task_count(&self) -> usize {
        self.task_queue.lock().len()
    }

    /// 检查执行器是否正在运行
    pub fn is_running(&self) -> bool {
        *self.is_running.lock()
    }
}

impl Default for SingleCpuExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// 生成异步任务的便捷函数
pub fn spawn<F, T>(future: F) -> TaskHandle
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    SingleCpuExecutor::global().spawn(future)
}

/// 阻塞等待异步任务完成
/// 注意：当前简化实现，仅支持()返回类型
pub fn block_on<F>(future: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let executor = SingleCpuExecutor::global();
    executor.spawn(future);
    executor.run_until_completion();
}

/// 执行一次任务调度
pub fn tick() {
    SingleCpuExecutor::global().tick();
}

/// 检查是否有待处理的任务
pub fn has_pending_tasks() -> bool {
    SingleCpuExecutor::global().has_pending_tasks()
}

/// 获取当前任务数量
pub fn task_count() -> usize {
    SingleCpuExecutor::global().task_count()
}

/// Executor专用的Waker实现
#[derive(Debug)]
struct ExecutorWaker {
    /// 任务ID
    task_id: TaskId,
}

impl ExecutorWaker {
    /// 创建新的执行器Waker
    fn new(task_id: TaskId) -> Self {
        Self { task_id }
    }

    /// 转换为标准库Waker
    fn into_waker(self) -> core::task::Waker {
        let arc = Arc::new(self);
        core::task::Waker::from(arc)
    }
}

impl alloc::task::Wake for ExecutorWaker {
    fn wake(self: Arc<Self>) {
        enqueue_wakeup(self.task_id);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        enqueue_wakeup(self.task_id);
    }
}
