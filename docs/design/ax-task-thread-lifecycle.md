# 线程装配与 Linux RT 生命周期

## 1. 语义基准

本次重构对照 `/home/zhourui/linux-src` 的 Linux v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，分析 `CONFIG_PREEMPT_RT=y` 对应路径。该目录现有 `.config` 未启用 RT，不能作为 RT 运行或性能证据。

### 1.1 所有权

`ax-task` 拥有调度状态和公共线程执行生命周期，`ax-runtime` 提供架构资源装配，Starry 的 `Thread` 持有 Linux 每线程状态并引用共享 `ProcessData`。OS 扩展直接安装到 `ThreadSpec`，不经过内核线程或运行时线程的外层扩展转发。公共创建使用 `ThreadBuilder`，运行队列和唤醒句柄保持非泛型。

### 1.2 顺序对照

Linux 顺序是迁移验收条件，不要求复制 C 对象布局。每条路径必须同时检查发布者、观察者和资源释放责任。

| Linux 基准 | 当前入口 | 目标保证 | 验证 |
| --- | --- | --- | --- |
| `dup_task_struct/copy_process/sched_fork` | `ThreadBuilder`、`create_thread` | 完整初始化后登记，保持 `New`，普通 wake 不激活 | 创建失败回滚、提前唤醒 |
| `wake_up_new_task` | `PreparedThread::stage/activate` | stage 不入队；身份发布后首次激活 | staged 状态及取消 |
| `ttwu_state_match` | RT 锁等待和唤醒状态机 | 普通唤醒更新 saved state，不冒充锁交接 | PI 锁与普通唤醒交错 |
| `context_switch/finish_task_switch` | `execute_switch_plan`、runtime switch tail | 先转移上下文，再 release 发布 prev off-CPU | SMP 切出与迁移 |
| `schedule_tail` | 新线程 trampoline | 首次执行前完成切换尾部和抢占控制权交接 | 首次入口 IRQ/抢占状态 |
| `put_task_stack/put_task_struct` | 任务上下文 reaper | 执行资源与对象引用分阶段回收 | 保留管理句柄的退出线程 |

`finish_task_switch` 先读取退出状态，再清除 `on_cpu`；任务引用和内核栈引用具有不同寿命。Linux RT 通过延迟释放避免在原子上下文取得睡眠锁，本项目复用任务上下文 reaper，并单独证明每个被借用对象的读者保护。

## 2. 发布与回收

创建令牌拥有取消责任；登记表拥有已转交资源。逻辑退出通知不代表线程已经物理切出，也不代表最终引用已经消失。

### 2.1 创建事务

`PreparedThread` 保存未运行线程，`StagedThread` 表示完成可失败准入的发布事务。首次激活前不得运行 trampoline 等待用户态发布，取消也不需要调度新线程。队列节点在构造阶段预留；CPU 下线、亲和性和远程投递必须与最终激活串行。

### 2.2 回收事务

切换尾部释放 CPU 所有权后，任务上下文回收执行资源。OS 扩展由强句柄和在途回调保护，最终析构不能提前。active-MM 令牌继续由独立的最后 CPU 回收协议处理；同 MM 的 user/kernel 转换不能跳过用户返回屏障。

## 3. 迁移验证

本节只记录实际取得的证据；未执行的矩阵不记为通过。重构保留分层资源语义，不用删断言或延长超时掩盖错误。

### 3.1 回归

在现有 ArceOS `task-wait-queue` 用例中增加 staged 线程必须保持 `New` 的断言，并通过公开 wake 验证不能提前激活。先在旧实现执行，再用同一断言验证修复。

### 3.2 完成条件

迁移工作区调用方并删除旧接口，完成项目 fmt、clippy、适用 std 测试及 ArceOS/Starry 四架构和适用 Axvisor QEMU。真实调度不使用宿主伪 runtime 替代。性能仅引用相同环境实际运行的基准；最终记录需补齐命令、结果和未验证限制。
