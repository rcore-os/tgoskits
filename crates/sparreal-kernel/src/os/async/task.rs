//! 异步任务相关结构体
//!
//! 定义了异步执行器使用的任务、句柄和Waker实现。

use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use alloc::boxed::Box;
use alloc::sync::Arc;

use crate::os::sync::spinlock::IrqSpinlock;
use crate::os::time;

/// 任务唯一标识符
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

impl TaskId {
    /// 生成新的任务ID
    pub fn new() -> Self {
        use core::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        TaskId(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// 任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    /// 等待执行
    Pending,
    /// 已被唤醒，准备执行
    Woken,
    /// 正在执行
    Running,
    /// 已完成
    Completed,
}

/// 任务元数据
#[derive(Debug)]
pub struct TaskMetadata {
    /// 任务ID
    pub id: TaskId,
    /// 任务状态
    pub state: TaskState,
    /// 创建时间
    pub created_at: u64,
    /// 最后唤醒时间
    pub last_wake_at: u64,
    /// 最后执行时间
    pub last_run_at: Option<u64>,
    /// 执行次数
    pub run_count: u64,
}

impl TaskMetadata {
    /// 创建新的任务元数据
    pub fn new(id: TaskId) -> Self {
        let now = time::since_boot().as_millis() as u64;
        Self {
            id,
            state: TaskState::Woken, // 新任务默认为 Woken 状态，可以立即执行
            created_at: now,
            last_wake_at: now,
            last_run_at: None,
            run_count: 0,
        }
    }

    /// 检查任务是否超过指定时间未执行
    pub fn is_expired(&self, timeout_ms: u64) -> bool {
        if let Some(last_run) = self.last_run_at {
            time::since_boot().as_millis() as u64 - last_run > timeout_ms
        } else {
            // 从未执行过的任务也检查创建时间
            time::since_boot().as_millis() as u64 - self.created_at > timeout_ms
        }
    }

    /// 更新执行时间
    pub fn update_execution(&mut self) {
        self.last_run_at = Some(time::since_boot().as_millis() as u64);
        self.run_count += 1;
        self.state = TaskState::Running;
    }

    /// 标记任务为已唤醒
    pub fn mark_woken(&mut self) {
        self.last_wake_at = time::since_boot().as_millis() as u64;
        self.state = TaskState::Woken;
    }

    /// 标记任务为已完成
    pub fn mark_completed(&mut self) {
        self.state = TaskState::Completed;
    }
}

/// 任务的优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskPriority {
    /// 是否为唤醒任务（最高优先级）
    pub is_woken: bool,
    /// 时间戳（越小越优先）
    pub timestamp: u64,
    /// 任务ID（作为平局打破器）
    pub task_id: TaskId,
}

impl TaskPriority {
    /// 创建新的任务优先级
    pub fn new(metadata: &TaskMetadata) -> Self {
        Self {
            is_woken: metadata.state == TaskState::Woken,
            timestamp: metadata.last_wake_at,
            task_id: metadata.id,
        }
    }
}

/// 任务引用
pub struct TaskRef {
    /// 任务元数据
    pub metadata: IrqSpinlock<TaskMetadata>,
    /// Future对象的Pin引用
    pub future: IrqSpinlock<Pin<Box<dyn Future<Output = ()> + Send>>>,
}

impl core::fmt::Debug for TaskRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let id = self.metadata.lock().id;
        write!(f, "TaskRef({id:?})")
    }
}

impl TaskRef {
    /// 创建新的任务引用
    pub fn new<F>(future: F, metadata: TaskMetadata) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self {
            metadata: IrqSpinlock::new(metadata),
            future: IrqSpinlock::new(Box::pin(future)),
        }
    }

    /// 获取任务ID
    pub fn id(&self) -> TaskId {
        self.metadata.lock().id
    }

    /// 检查任务是否已完成
    pub fn is_completed(&self) -> bool {
        self.metadata.lock().state == TaskState::Completed
    }

    /// 获取任务优先级
    pub fn priority(&self) -> TaskPriority {
        TaskPriority::new(&self.metadata.lock())
    }

    /// 轮询任务Future
    pub fn poll(&self, waker: &Waker) -> Poll<()> {
        let metadata = &mut self.metadata.lock();
        if metadata.state == TaskState::Completed {
            return Poll::Ready(());
        }

        metadata.update_execution();

        let future = &mut self.future.lock();
        let mut cx = Context::from_waker(waker);

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(()) => {
                metadata.mark_completed();
                Poll::Ready(())
            }
            Poll::Pending => {
                metadata.state = TaskState::Pending;
                Poll::Pending
            }
        }
    }
}

/// 任务句柄，用于与任务交互
#[derive(Debug, Clone)]
pub struct TaskHandle {
    /// 任务ID
    pub id: TaskId,
    /// 内部任务引用（可选，用于唤醒）
    task_ref: Option<Arc<TaskRef>>,
}

impl TaskHandle {
    /// 创建新的任务句柄
    pub fn new(id: TaskId) -> Self {
        Self { id, task_ref: None }
    }

    /// 创建带有任务引用的句柄
    pub fn with_ref(id: TaskId, task_ref: Arc<TaskRef>) -> Self {
        Self {
            id,
            task_ref: Some(task_ref),
        }
    }

    /// 唤醒对应的任务
    pub fn wake(&self) -> bool {
        if let Some(task_ref) = &self.task_ref {
            task_ref.metadata.lock().mark_woken();
            true
        } else {
            false
        }
    }
}
