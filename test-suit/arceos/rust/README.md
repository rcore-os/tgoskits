# ArceOS Rust 测试套件

`arceos-test-suit` 通过真正的 Rust `std` 运行普通功能测试，覆盖标准库到 libc、POSIX 适配层和 ArceOS 的调用链。`src/lib.rs` 的 `SELECTED_TESTS` 按 Cargo feature 登记用例，`src/main.rs` 负责逐项执行、恢复运行器的调度状态和汇总结果。

## 1. 接口边界

接口选择取决于测试要证明的行为。普通功能优先使用标准库；只有标准库无法表达的 Linux ABI 或内核能力才使用更低层入口。

### 1.1 标准库与 libc

`std` 必须指向工具链提供的标准库，不能通过 `extern crate ax_std as std` 替换。`ax_std` 作为运行时链接依赖提供 libc 符号；`axbuild` 的 `RustStd` 路径选择 musl 目标、链接 ArceOS，并经 `__axstd_std_check_entry` 进入 Rust 标准启动流程。

`task/mutex.rs` 使用 `std::sync::{Mutex, Condvar, Barrier, mpsc}` 检查互斥、通知和数据发布，`task/parallel.rs` 使用标准屏障同步计算线程。`task/tls.rs` 使用 `thread_local!` 检查线程隔离和退出析构，`mem/test.rs` 的对齐分配通过 `std::alloc`，文件、网络、时间及普通线程分别使用 `std::fs`、`std::net`、`std::time` 和 `std::thread`。

`io_mpx/syscalls.rs` 使用 `std::io::pipe` 创建管道，由 `std::fs::File` 管理描述符、读写及关闭。标准库没有 eventfd 和 epoll 接口，因此这些操作使用 `libc` 的函数、常量和 `epoll_event` 布局。`futex.rs` 通过 `libc::syscall` 验证错误优先级，不直接调用 `ax_std::os::libc_compat` 的 Rust 函数。

`std::io::pipe()` 所需的 `pipe2(O_CLOEXEC)` 由 `ax-posix-api::sys_pipe2` 实现，两端及其描述符标志在 fd 表锁内一起安装；容量不足时回收首个槽位，输出数组保持不变。`FileDescriptor` 保存每个 fd 独立的关闭标志，`F_GETFD`、`F_SETFD` 和复制操作读取或更新该状态。目前管道仅支持阻塞字节流和 `O_CLOEXEC`，非阻塞、packet、notification 模式不会被静默接受。

### 1.2 内核专用能力

调度策略、CPU 拓扑、亲和性、IPI、IRQ、WaitQueue、内核定时器、页表、显示硬件和异常恢复使用明确的 `ax_std::os::arceos` 入口。普通 std 线程仍由 ArceOS 的 pthread 实现创建任务，可以与这些 OS 能力共同用于测试；只有需要定制任务资源或生命周期的用例才直接创建内核任务。

`task/pi_mutex.rs` 和 `lockdep/baseline.rs` 保留 OS `Mutex`，因为 PI 捐赠与内核 lockdep 不是 `std::sync::Mutex` 的契约。`task/stack_guard_page.rs` 用 `spawn_raw` 明确创建被测内核栈。`task/scheduler_irq_window.rs` 用带亲和性的内核任务建立排队工作，并让 FIFO 控制器持有准备阶段，避免依赖每个 Fair 任务先取得一次时间片。该专项在四架构 CI 中独立运行，覆盖非 current 任务的亲和性更新和 IRQ 返回的分批调度窗口。`task/pi_mutex.rs` 在 owner 线程内部发布内核 `ThreadId`，不把标准库 `ThreadId` 的数值当作内核标识。

CPU 拓扑测试通过 `cpu_topology_len()` 获取运行时 CPU 数；`std::thread::available_parallelism()` 表达调用者可用并行度，不能代替内核拓扑。`RunnerTaskState` 在每项任务测试后恢复原亲和性和调度策略，防止用例互相影响。

## 2. 运行与判定

所有 ArceOS 构建和运行通过 `cargo xtask` 进行。feature、`test_runner!`、`SELECTED_TESTS` 和构建配置共同决定实际执行内容，不能用缺少运行时的空函数返回成功。

### 2.1 选择用例

默认 Rust 批次运行 `all`，并按发现器安排需要独立调度配置的用例。可以先通过 `--list` 核对可选 feature，再用 `--test-case` 定向运行。

```bash
cargo xtask arceos test qemu --test-group rust --arch riscv64 --list
cargo xtask arceos test qemu --test-group rust --arch riscv64
cargo xtask arceos test qemu --test-group rust --arch riscv64 --test-case task-mutex
```

`--test-case` 使用 feature 名称。测试通过不只要求程序退出正常，还要求运行器匹配对应成功标志。

### 2.2 回归证据

`main.rs` 为每项测试输出 `ARCEOS_TEST_BEGIN` 和 `ARCEOS_TEST_END`，全部成功后输出 `ArceOS test suite run OK!`。测试失败或 panic 必须传播为最外层 `xtask` 的非零退出码，预期异常和 lockdep 检测则使用发现器配置的专用判定规则。

`lockdep/baseline.rs` 的双线程自旋锁测试在释放 A、B 后才发布完成阶段，并在发布前检查两把锁已释放。工作线程持有 A、B 时提前通知，不能保证另一线程取得 B 后也能取得 A；恢复该错误顺序时，前置断言会确定性失败。

## 3. 其他测试入口

`test-suit/arceos` 还包含直接验证 C ABI 和硬件行为的测试。选择 std 是为了覆盖实际调用边界，不应删除这些入口的独立职责。

### 3.1 C 接口

`../c` 通过 musl 与 `ax-libc` 验证 malloc、pthread、时间、管道、epoll 和 socket 的 C/POSIX 语义。它与 Rust std suite 覆盖不同的消费者入口，继续保留 C harness 和 `ARCEOS_C_TEST_*` 判定。

### 3.2 平台与板卡

`../loongarch/unaligned-fixup` 使用标准 Rust 启动入口，异常表修复仍调用架构 helper。`../board-orangepi-5-plus/ipi` 复用 Rust suite 的 IPI/SMP feature。`../axtest/sg2002-usb-msc` 保留 no_std 硬件 harness，直接检查 USB、IRQ、DMA 和传输完成；其中的纯逻辑模块可以通过宿主单元测试验证，但不能替代实体板卡运行。
