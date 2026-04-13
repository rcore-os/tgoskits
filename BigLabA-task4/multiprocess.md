# 一、多核支持现状分析

## 1.1 当前多核支持能力

**支持情况：**
- StarryOS 通过 `ax-feat/smp` 特性提供基础的多核支持。
- **配置位置：** `starryos/Cargo.toml:27`
  ```toml
  smp = ["ax-feat/smp", "axplat-riscv64-visionfive2?/smp"]
  ```
- 依赖 ArceOS 的 `ax-task` 模块提供多核任务调度能力。

**已实现的多核功能：**

1. **CPU 亲和性支持** (`kernel/src/syscall/task/schedule.rs:91-128`)
   - `sys_sched_getaffinity`: 获取进程的 CPU 亲和性掩码。
   - `sys_sched_setaffinity`: 设置进程的 CPU 亲和性。
   - 使用 `AxCpuMask` 管理 CPU 掩码。
   - **当前限制：** 仅支持当前线程（pid=0），不支持其他线程。

2. **内存屏障支持** (`kernel/src/syscall/sync/membarrier.rs`)
   - 实现了 `sys_membarrier` 系统调用。
   - 支持全局和私有内存屏障命令。
   - **当前实现：** 使用编译器屏障 `compiler_fence(Ordering::SeqCst)`。
   - **问题：** 没有真正的跨核心内存屏障，仅使用编译器屏障不足以保证多核一致性。

## 1.2 多核支持的关键缺陷

- **缺陷 1：缺少 Per-CPU 数据结构**
  - **搜索结果：** 整个内核代码中没有找到 `percpu`、`per_cpu` 或 `PerCpu` 相关代码。
  - **影响：** 无法高效实现无锁的 per-CPU 变量，增加锁竞争。

- **缺陷 2：同步原语粒度过粗**
  - 大量使用全局锁保护共享数据结构。
  - **例如：** `ProcessData` 中的 `aspace: Arc<Mutex<AddrSpace>>` 是全局锁。
  - 文件系统操作、内存管理都存在全局锁瓶颈。

- **缺陷 3：内存屏障实现不完整**
  - `membarrier` 系统调用仅使用编译器屏障。
  - 缺少真正的 CPU 内存屏障指令（如 RISC-V 的 `fence`）。
  - 多核环境下可能导致内存可见性问题。

- **缺陷 4：调度器多核优化不足**
  - CPU 亲和性功能不完整（TODO 注释：仅支持当前线程）。
  - 缺少负载均衡机制。
  - 缺少 CPU 本地运行队列。

---

# 二、最值得改进的 10 个关键问题

## 问题 1：内存屏障实现不完整（严重性：高）

**涉及文件：**
- `kernel/src/syscall/sync/membarrier.rs`

**具体问题：**
```rust
// 第 29 行
_ => {
    compiler_fence(Ordering::SeqCst);  // 仅编译器屏障
    Ok(0)
}
```

**问题分析：**
- `compiler_fence` 只防止编译器重排，不能防止 CPU 乱序执行。
- 在多核系统中，其他 CPU 核心可能看不到内存修改。
- 可能导致数据竞争和内存一致性问题。

**改进计划：**

1. **短期方案（1-2 周）：**
   - 添加架构相关的内存屏障指令。
   - **RISC-V:** 使用 `fence` 指令。
   - **AArch64:** 使用 `dmb`/`dsb` 指令。
   - **x86_64:** 使用 `mfence` 指令。

2. **实现代码框架：**
   ```rust
   // kernel/src/syscall/sync/membarrier.rs
   use core::sync::atomic::{Ordering, compiler_fence};

   #[cfg(target_arch = "riscv64")]
   fn memory_barrier_full() {
     unsafe {
         core::arch::asm!("fence rw, rw");
     }
   }

   #[cfg(target_arch = "aarch64")]
   fn memory_barrier_full() {
     unsafe {
         core::arch::asm!("dmb ish");
     }
   }

   #[cfg(target_arch = "x86_64")]
   fn memory_barrier_full() {
     unsafe {
         core::arch::asm!("mfence");
     }
   }

   pub fn sys_membarrier(cmd: i32, flags: u32, cpu_id: i32) -> AxResult<isize> {
     if flags != 0 {
         return Err(AxError::InvalidInput);
     }

     match cmd {
         MEMBARRIER_CMD_QUERY => Ok(SUPPORTED_COMMANDS as isize),
         MEMBARRIER_CMD_GLOBAL | MEMBARRIER_CMD_GLOBAL_EXPEDITED => {
             // 需要在所有 CPU 上执行屏障
             memory_barrier_full();
             // TODO: 发送 IPI 到其他核心
             Ok(0)
         }
         MEMBARRIER_CMD_PRIVATE_EXPEDITED => {
             // 仅当前 CPU
             memory_barrier_full();
             Ok(0)
         }
         _ => {
             compiler_fence(Ordering::SeqCst);
             memory_barrier_full();
             Ok(0)
         }
     }
   }
   ```

3. **长期方案（1-2 月）：**
   - 实现 IPI（Inter-Processor Interrupt）机制。
   - 支持向其他 CPU 核心发送内存屏障请求。
   - 实现 `MEMBARRIER_CMD_GLOBAL` 的真正跨核心屏障。

---

## 问题 2：地址空间锁竞争严重（严重性：高）

**涉及文件：**
- `kernel/src/task/mod.rs:197`
- `kernel/src/mm/aspace/mod.rs`
- `kernel/src/mm/access.rs:45-53`

**具体问题：**
```rust
// kernel/src/task/mod.rs:197
pub struct ProcessData {
    pub aspace: Arc<Mutex<AddrSpace>>,  // 全局互斥锁
    // ...
}

// kernel/src/mm/access.rs:45
let mut aspace = curr.as_thread().proc_data.aspace.lock();
```

**问题分析：**
- 每次访问用户内存都需要获取全局 `aspace` 锁。
- 多线程并发访问用户内存时会产生严重锁竞争。
- 页表查询、内存映射、缺页处理都需要持有此锁。
- 在多核系统中，这是性能瓶颈。

**改进计划：**

1. **短期方案（2-3 周）：**
   - 将 `AddrSpace` 的锁拆分为多个细粒度锁。
   - 页表操作和区域管理使用不同的锁。

   ```rust
   // kernel/src/mm/aspace/mod.rs
   pub struct AddrSpace {
       va_range: VirtAddrRange,
       areas: RwLock<MemorySet<Backend>>,  // 读写锁，支持并发读
       pt: Mutex<PageTable>,                // 页表单独锁
   }

   impl AddrSpace {
       // 只读操作使用读锁
       pub fn can_access_range(&self, start: VirtAddr, size: usize, flags: MappingFlags) -> bool {
           let areas = self.areas.read();
           // 查询操作，允许并发
           areas.find(start).map_or(false, |area| {
               area.contains(start, size) && area.flags().contains(flags)
           })
       }

       // 修改操作使用写锁
       pub fn map_region(&self, start: VirtAddr, size: usize, backend: Backend) -> AxResult {
           let mut areas = self.areas.write();
           let mut pt = self.pt.lock();
           // 修改操作
       }
   }
   ```

2. **中期方案（1-2 月）：**
   - 实现 RCU（Read-Copy-Update）机制用于页表查询。
   - 使用无锁数据结构管理内存区域。
   - 参考 Linux 的 `mmap_lock` 设计，使用读写信号量。

3. **长期方案（2-3 月）：**
   - 实现 per-CPU 页表缓存（TLB shootdown 优化）。
   - 使用 `seqlock` 保护热路径的页表查询。
   - 实现延迟 TLB 刷新机制。

---

## 问题 3：缺少 Per-CPU 数据结构（严重性：高）

**涉及文件：**
- 全局搜索无 per-CPU 相关代码。
- 需要新增 `kernel/src/percpu.rs`。

**具体问题：**
- 当前没有 per-CPU 变量支持。
- 所有 CPU 共享全局数据结构，增加缓存一致性开销。
- 无法实现高效的无锁算法。

**问题分析：**
- 缺少 per-CPU 运行队列，调度器性能受限。
- 缺少 per-CPU 内存分配器，分配性能差。
- 缺少 per-CPU 统计信息，难以进行性能分析。

**改进计划：**

1. **短期方案（2-3 周）：**
   - 引入 `ax-percpu` crate（已在 `kernel/Cargo.toml:92` 中依赖）。
   - 实现基础的 per-CPU 变量宏。

   ```rust
   // kernel/src/percpu.rs
   use ax_percpu::{PerCpu, percpu_init};

   // 定义 per-CPU 变量
   pub static CURRENT_TASK: PerCpu<Option<TaskRef>> = PerCpu::new(None);
   pub static CPU_STATS: PerCpu<CpuStats> = PerCpu::new(CpuStats::new());

   #[derive(Default)]
   pub struct CpuStats {
       pub syscall_count: u64,
       pub context_switches: u64,
       pub page_faults: u64,
   }

   // 初始化函数
   pub fn init_percpu() {
       percpu_init(ax_hal::cpu_num());
   }

   // 访问 per-CPU 变量
   pub fn current_task() -> Option<TaskRef> {
       CURRENT_TASK.get().clone()
   }

   pub fn inc_syscall_count() {
       CPU_STATS.get_mut().syscall_count += 1;
   }
   ```

2. **中期方案（1-2 月）：**
   - 实现 per-CPU 运行队列。
   - 实现 per-CPU 内存分配器缓存。
   - 实现 per-CPU 中断计数器。

   ```rust
   // kernel/src/task/scheduler.rs
   pub struct PerCpuScheduler {
       run_queue: VecDeque<TaskRef>,
       idle_task: TaskRef,
       current: Option<TaskRef>,
   }

   pub static SCHEDULERS: PerCpu<PerCpuScheduler> = PerCpu::new(PerCpuScheduler::new());

   impl PerCpuScheduler {
       pub fn pick_next_task(&mut self) -> TaskRef {
           self.run_queue.pop_front().unwrap_or_else(|| self.idle_task.clone())
       }

       pub fn enqueue_task(&mut self, task: TaskRef) {
           self.run_queue.push_back(task);
       }
   }
   ```

3. **长期方案（2-3 月）：**
   - 实现完整的 per-CPU 子系统。
   - 支持动态 CPU 热插拔。
   - 实现 per-CPU 性能监控框架。

---

## 问题 4：Futex 实现的可扩展性问题（严重性：中）

**涉及文件：**
- `kernel/src/task/futex.rs:32-100`

**具体问题：**
```rust
// kernel/src/task/futex.rs:34
pub struct WaitQueue {
    queue: SpinNoIrq<VecDeque<(Waker, u32)>>,  // 单一自旋锁
}

impl WaitQueue {
    pub fn wake(&self, count: usize, mask: u32) -> usize {
        let mut woke = 0;
        self.queue.lock().retain(|(waker, bitset)| {  // 持锁遍历
            if woke >= count || (bitset & mask) == 0 {
                true
            } else {
                waker.wake_by_ref();
                woke += 1;
                false
            }
        });
        woke
    }
}
```

**问题分析：**
- 使用单一自旋锁保护等待队列。
- `wake` 操作需要持锁遍历整个队列。
- 在高并发场景下，多个线程同时 `wait`/`wake` 会产生锁竞争。
- 唤醒操作的时间复杂度为 O(n)。

**改进计划：**

1. **短期方案（1-2 周）：**
   - 使用哈希表分桶，减少锁竞争。

   ```rust
   // kernel/src/task/futex.rs
   const FUTEX_BUCKETS: usize = 256;

   pub struct WaitQueue {
       buckets: [SpinNoIrq<VecDeque<(Waker, u32)>>; FUTEX_BUCKETS],
   }

   impl WaitQueue {
       fn bucket_index(&self, addr: usize) -> usize {
           (addr / 4) % FUTEX_BUCKETS
       }

       pub fn wait_if(&self, addr: usize, bitset: u32, ...) -> AxResult<bool> {
           let bucket = &self.buckets[self.bucket_index(addr)];
           // 只锁定对应的桶
           let mut queue = bucket.lock();
           // ...
       }
   }
   ```

2. **中期方案（2-3 周）：**
   - 实现优先级队列，优化唤醒顺序。
   - 使用位图快速查找可唤醒的线程。
   - 实现 `futex requeue` 优化。

3. **长期方案（1-2 月）：**
   - 参考 Linux 的 `futex2` 设计。
   - 实现无锁的 `futex` 快速路径。
   - 支持 NUMA 感知的 `futex` 分配。

---

## 问题 5：Epoll 实现的性能问题（严重性：中）

**涉及文件：**
- `kernel/src/file/epoll.rs` (455 行)

**具体问题：**
```rust
// kernel/src/file/epoll.rs:22
use ax_kspin::SpinNoPreempt;

// 使用自旋锁保护 epoll 数据结构
// 在高并发 I/O 场景下会产生锁竞争
```

**问题分析：**
- Epoll 是高性能 I/O 多路复用的关键组件。
- 当前实现使用自旋锁，在多核环境下性能受限。
- 缺少边缘触发（ET）模式的优化。
- 就绪列表的管理效率不高。

**改进计划：**

1. **短期方案（2-3 周）：**
   - 将 epoll 实例的锁拆分为多个细粒度锁。
   - 就绪列表和兴趣列表使用不同的锁。

   ```rust
   // kernel/src/file/epoll.rs
   pub struct EpollInstance {
       interests: RwLock<HashMap<EpollKey, Interest>>,  // 读写锁
       ready_list: Mutex<VecDeque<EpollKey>>,           // 互斥锁
       wait_queue: WaitQueue,
   }

   impl EpollInstance {
       // 添加兴趣（不常见操作）
       pub fn add_interest(&self, key: EpollKey, interest: Interest) {
           let mut interests = self.interests.write();
           interests.insert(key, interest);
       }

       // 检查就绪（频繁操作）
       pub fn poll(&self, events: &mut [EpollEvent], timeout: Duration) -> AxResult<usize> {
           let interests = self.interests.read();  // 只读锁，允许并发
           // 检查就绪事件
       }
   }
   ```

2. **中期方案（1-2 月）：**
   - 实现无锁的就绪列表（使用 MPSC 队列）。
   - 优化边缘触发模式的事件通知。
   - 实现批量事件处理。

3. **长期方案（2-3 月）：**
   - 实现 `io_uring` 风格的异步 I/O 接口。
   - 支持零拷贝的事件通知。
   - 实现 NUMA 感知的 epoll 实例分配。

---

## 问题 6：IPC 共享内存的同步问题（严重性：中）

**涉及文件：**
- `kernel/src/syscall/ipc/shm.rs` (568 行)

**具体问题：**
```rust
// kernel/src/syscall/ipc/shm.rs:82-96
pub struct ShmInner {
    pub shmid: i32,
    pub page_num: usize,
    va_range: BTreeMap<Pid, VirtAddrRange>,  // 进程映射表
    pub phys_pages: Option<Arc<SharedPages>>,
    pub rmid: bool,
    pub mapping_flags: MappingFlags,
    pub shmid_ds: ShmidDs,
}
```

**问题分析：**
- 共享内存段的元数据使用全局锁保护。
- 多个进程同时访问共享内存时会产生锁竞争。
- 缺少细粒度的同步机制。
- 没有考虑 NUMA 架构下的内存亲和性。

**改进计划：**

1. **短期方案（1-2 周）：**
   - 使用读写锁替代互斥锁。
   - 分离元数据锁和映射表锁。

   ```rust
   // kernel/src/syscall/ipc/shm.rs
   pub struct ShmInner {
       pub shmid: i32,
       pub page_num: usize,
       va_range: RwLock<BTreeMap<Pid, VirtAddrRange>>,  // 读写锁
       pub phys_pages: Option<Arc<SharedPages>>,
       pub rmid: AtomicBool,  // 原子变量
       pub mapping_flags: MappingFlags,
       pub shmid_ds: RwLock<ShmidDs>,  // 元数据读写锁
   }

   impl ShmInner {
       // 查询操作使用读锁
       pub fn get_mapping(&self, pid: Pid) -> Option<VirtAddrRange> {
           self.va_range.read().get(&pid).copied()
       }

       // 修改操作使用写锁
       pub fn add_mapping(&self, pid: Pid, range: VirtAddrRange) {
           self.va_range.write().insert(pid, range);
       }
   }
   ```

2. **中期方案（2-3 周）：**
   - 实现共享内存的 COW（Copy-On-Write）支持。
   - 优化共享内存的页表映射性能。
   - 实现共享内存的预分配机制。

3. **长期方案（1-2 月）：**
   - 支持 NUMA 感知的共享内存分配。
   - 实现大页（Huge Page）支持。
   - 实现共享内存的热迁移。

---

## 问题 7：系统调用处理的性能开销（严重性：中）

**涉及文件：**
- `kernel/src/syscall/mod.rs` (640 行)

**具体问题：**
```rust
// kernel/src/syscall/mod.rs:22-30
pub fn handle_syscall(uctx: &mut UserContext) {
    let Some(sysno) = Sysno::new(uctx.sysno()) else {
        warn!("Invalid syscall number: {}", uctx.sysno());
        uctx.set_retval(-LinuxError::ENOSYS.code() as _);
        return;
    };

    trace!("Syscall {sysno:?}");  // 每次系统调用都打印日志

    let result = match sysno {  // 巨大的 match 语句
        // 640 行的 match 分支
    };
}
```

**问题分析：**
- 系统调用分发使用巨大的 `match` 语句（640 行）。
- 每次系统调用都有日志开销（即使在 release 模式）。
- 缺少系统调用的性能统计。
- 没有系统调用的快速路径优化。

**改进计划：**

1. **短期方案（1-2 周）：**
   - 使用函数指针表替代 `match` 语句。
   - 移除热路径的日志调用。

   ```rust
   // kernel/src/syscall/mod.rs
   type SyscallHandler = fn(&mut UserContext) -> AxResult<isize>;

   static SYSCALL_TABLE: [Option<SyscallHandler>; 512] = {
       let mut table = [None; 512];
       table[Sysno::read as usize] = Some(handle_read);
       table[Sysno::write as usize] = Some(handle_write);
       // ...
       table
   };

   pub fn handle_syscall(uctx: &mut UserContext) {
       let sysno = uctx.sysno();

       if let Some(handler) = SYSCALL_TABLE.get(sysno as usize).and_then(|h| *h) {
           let result = handler(uctx);
           uctx.set_retval(result.unwrap_or_else(|e| -e.code() as isize));
       } else {
           uctx.set_retval(-LinuxError::ENOSYS.code() as _);
       }
   }

   fn handle_read(uctx: &mut UserContext) -> AxResult<isize> {
       sys_read(uctx.arg0() as _, uctx.arg1() as _, uctx.arg2() as _)
   }
   ```

2. **中期方案（2-3 周）：**
   - 实现系统调用的性能计数器。
   - 优化高频系统调用的路径。
   - 实现系统调用的批处理机制。

3. **长期方案（1-2 月）：**
   - 实现 `vDSO`（virtual Dynamic Shared Object）。
   - 将部分系统调用移到用户空间执行。
   - 实现系统调用的 JIT 优化。

---

## 问题 8：调度器缺少负载均衡（严重性：中）

**涉及文件：**
- `kernel/src/syscall/task/schedule.rs:91-128`
- `kernel/src/task/ops.rs`

**具体问题：**
```rust
// kernel/src/syscall/task/schedule.rs:96-99
pub fn sys_sched_getaffinity(pid: i32, cpusetsize: usize, user_mask: *mut u8) -> AxResult<isize> {
    // TODO: support other threads
    if pid != 0 {
        return Err(AxError::OperationNotPermitted);
    }
    // ...
}
```

**问题分析：**
- CPU 亲和性功能不完整，仅支持当前线程。
- 缺少跨 CPU 的负载均衡机制。
- 没有 consider CPU 拓扑结构（NUMA、SMT）。
- 缺少任务迁移的性能优化。

**改进计划：**

1. **短期方案（2-3 周）：**
   - 完善 CPU 亲和性支持，支持任意进程/线程。
   - 实现基础的负载均衡算法。

   ```rust
   // kernel/src/task/scheduler.rs
   pub struct LoadBalancer {
       cpu_loads: PerCpu<AtomicUsize>,
       balance_interval: Duration,
   }

   impl LoadBalancer {
       pub fn balance(&self) {
           let loads: Vec<usize> = (0..ax_hal::cpu_num())
               .map(|cpu| self.cpu_loads.get_on_cpu(cpu).load(Ordering::Relaxed))
               .collect();

           let avg_load = loads.iter().sum::<usize>() / loads.len();
     
           for (cpu, &load) in loads.iter().enumerate() {
               if load > avg_load * 120 / 100 {  // 超过平均负载 20%
                   self.migrate_tasks_from(cpu, load - avg_load);
               }
           }
       }
     
       fn migrate_tasks_from(&self, from_cpu: usize, count: usize) {
           // 将任务迁移到负载较低的 CPU
       }
   }
   ```

2. **中期方案（1-2 月）：**
   - 实现 NUMA 感知的调度策略。
   - 支持 CPU 拓扑信息（L1/L2/L3 缓存共享）。
   - 实现任务迁移的成本模型。

3. **长期方案（2-3 月）：**
   - 实现 `CFS`（Completely Fair Scheduler）风格的调度器。
   - 支持实时调度策略（`SCHED_FIFO`、`SCHED_RR`）。
   - 实现能耗感知的调度策略。

---

## 问题 9：文件描述符表的并发性能（严重性：低）

**涉及文件：**
- `kernel/src/file/mod.rs`
- 使用全局 `FD_TABLE`

**具体问题：**
- 文件描述符表使用全局锁保护。
- 高并发文件操作时会产生锁竞争。
- 缺少 per-process 的文件描述符表优化。

**问题分析：**
- 多线程同时打开/关闭文件时会竞争 FD 表锁。
- 文件描述符分配算法效率不高。
- 缺少文件描述符的缓存机制。

**改进计划：**

1. **短期方案（1-2 周）：**
   - 使用读写锁替代互斥锁。
   - 实现文件描述符的快速分配算法。

   ```rust
   // kernel/src/file/fd_table.rs
   pub struct FdTable {
       fds: RwLock<Vec<Option<Arc<dyn FileLike>>>>,
       next_fd: AtomicUsize,  // 原子变量，减少锁竞争
   }

   impl FdTable {
       pub fn get(&self, fd: usize) -> Option<Arc<dyn FileLike>> {
           self.fds.read().get(fd).and_then(|f| f.clone())
       }

       pub fn alloc_fd(&self, file: Arc<dyn FileLike>) -> AxResult<usize> {
           // 先尝试无锁分配
           let hint = self.next_fd.fetch_add(1, Ordering::Relaxed);
     
           let mut fds = self.fds.write();
           // 从 hint 开始查找空闲 fd
           for fd in hint..fds.len() {
               if fds[fd].is_none() {
                   fds[fd] = Some(file);
                   return Ok(fd);
               }
           }
           // 扩展表
           fds.push(Some(file));
           Ok(fds.len() - 1)
       }
   }
   ```

2. **中期方案（2-3 周）：**
   - 实现 per-thread 的文件描述符缓存。
   - 优化文件描述符的查找性能。
   - 实现文件描述符的延迟关闭。

3. **长期方案（1-2 月）：**
   - 实现无锁的文件描述符表。
   - 支持大规模文件描述符（百万级）。
   - 实现文件描述符的自动回收机制。

---

## 问题 10：缺少完善的测试框架（严重性：低）

**涉及文件：**
- 整个 `kernel/src` 目录
- 搜索结果：没有找到 `#[test]` 或 `#[cfg(test)]`

**具体问题：**
- 内核代码缺少单元测试。
- 缺少多核场景的集成测试。
- 缺少性能回归测试。
- 缺少并发正确性测试。

**问题分析：**
- 多核相关的 bug 往往具有非确定性，难以调试。
- 缺少自动化测试导致修改代码时容易引入回归。