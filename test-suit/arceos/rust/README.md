# ArceOS Rust 测试套件

`arceos-test-suit` 通过真正的 Rust `std` 运行普通功能测试，覆盖标准库到 libc、POSIX 适配层和 ArceOS 的调用链。`suite.toml` 统一登记测试显式启动顺序及按批次登记的测试身份、分类、入口与架构；`build.rs` 生成目标函数表，`src/runner.rs` 负责逐项执行、恢复运行器的调度状态和汇总结果，`src/main.rs` 只调用执行器。

## 1. 接口边界

接口选择取决于测试要证明的行为。普通功能优先使用标准库；只有标准库无法表达的 Linux ABI 或内核能力才使用更低层入口。

### 1.1 标准库与 libc

`std` 必须指向工具链提供的标准库，不能通过 `extern crate ax_std as std` 替换。`ax_std` 作为运行时链接依赖提供 libc 符号；`axbuild` 的 `RustStd` 路径选择 musl 目标、链接 ArceOS，并经 `__axstd_std_check_entry` 进入 Rust 标准启动流程。

`task/mutex.rs` 使用 `std::sync::{Mutex, Condvar, Barrier, mpsc}` 检查互斥、通知和数据发布，futex错误优先级并入同一组。`task/tls.rs` 使用 `thread_local!` 检查线程隔离和退出析构，`mem/test.rs` 的对齐分配通过 `std::alloc`，文件、网络、时间及普通线程分别使用 `std::fs`、`std::net`、`std::time` 和 `std::thread`。

`io_mpx/syscalls.rs` 使用 `std::io::pipe` 创建管道，由 `std::fs::File` 管理描述符、读写及关闭。标准库没有 eventfd 和 epoll 接口，因此这些操作使用 `libc` 的函数、常量和 `epoll_event` 布局。`task/mutex/futex.rs` 通过 `libc::syscall` 验证错误优先级，不直接调用 `ax_std::os::libc_compat` 的 Rust 函数。

`std::io::pipe()` 所需的 `pipe2(O_CLOEXEC)` 由 `ax-posix-api::sys_pipe2` 实现，两端及其描述符标志在 fd 表锁内一起安装；容量不足时回收首个槽位，输出数组保持不变。`FileDescriptor` 保存每个 fd 独立的关闭标志，`F_GETFD`、`F_SETFD` 和复制操作读取或更新该状态。目前管道仅支持阻塞字节流和 `O_CLOEXEC`，非阻塞、packet、notification 模式不会被静默接受。

### 1.2 内核专用能力

调度策略、CPU 拓扑、亲和性、IPI、IRQ、WaitQueue、内核定时器、页表、显示硬件和异常恢复使用明确的 `ax_std::os::arceos` 入口。普通 std 线程仍由 ArceOS 的 pthread 实现创建任务，可以与这些 OS 能力共同用于测试；只有需要定制任务资源或生命周期的用例才直接创建内核任务。

`task/pi_mutex.rs` 和 `lockdep/baseline.rs` 保留 OS `Mutex`，因为 PI 捐赠与内核 lockdep 不是 `std::sync::Mutex` 的契约。`task/stack_guard_page.rs` 用 `spawn_raw` 明确创建被测内核栈。`task/scheduler_irq_window.rs` 用带亲和性的内核任务建立排队工作，并让 FIFO 控制器持有准备阶段，避免依赖每个 Fair 任务先取得一次时间片。该功能随四架构 `general` 批次运行，覆盖非 current 任务的亲和性更新和 IRQ 返回的分批调度窗口。`task/pi_mutex.rs` 在 owner 线程内部发布内核 `ThreadId`，不把标准库 `ThreadId` 的数值当作内核标识。

CPU 拓扑测试通过 `cpu_topology_len()` 获取运行时 CPU 数；`std::thread::available_parallelism()` 表达调用者可用并行度，不能代替内核拓扑。`RunnerTaskState` 在每项任务测试后恢复原亲和性和调度策略，防止用例互相影响。

四架构的共享构建输入使用 320 KiB 任务栈和 5 毫秒 tick，覆盖公共配置到真实 runtime 的非默认参数链。`task-yield` 在创建和等待内核任务时检查 API 与 runtime 的栈参数一致，`cpu-capacity` 检查 HAL、运行时拓扑和编译容量的关系；既有调度时钟用例继续验证真实中断和任务计时。验证 fs 与无 fs 时使用相同线程用例，仅切换所需文件系统能力，避免另建参数实例测试。

## 2. 运行与判定

所有 ArceOS 构建和运行通过 `cargo xtask` 进行。Cargo feature、`suite.toml`、生成的 `SELECTED_TESTS` 和构建配置共同决定实际执行内容，不能用缺少运行时的空函数返回成功。

### 2.1 选择用例

`suite.toml` 顶部的 `run_order` 决定默认启动顺序，下面用 `[batches.general]`、`[[batches.general.tests]]` 等具名表多行登记38项功能。组定义的摆放位置不影响启动顺序，组内测试声明顺序决定执行顺序；批次的架构限制由成员继承，成员可进一步收窄。Cargo只维护单项feature及依赖，不再登记聚合成员。 `run_order` 必须包含每个已登记批次且仅出现一次，未知、重复或遗漏批次会报错。默认运行 `general`、隔离设备/全局状态和三项终止式诊断。NVMe与iTCO通过 `x86-devices` 共享Q35/UEFI启动，panic、lockdep、栈保护各自启动。

`--list` 按 `run_order` 展开批次和成员，标注每批一次启动及测试数量；`--test-case` 可以选择单项或复合批次，默认顺序由 `run_order` 决定。每个功能仍输出独立结果。原Rust批次选择 `--test-case all` 改为 `--test-case general`，没有兼容别名；C套件的all保持原义。单项优先使用自身专用QEMU配置，否则继承批次runtime并回退至通用配置。

```bash
cargo xtask arceos test qemu --test-group rust --arch riscv64 --list
cargo xtask arceos test qemu --test-group rust --arch riscv64 --plan
cargo xtask arceos test qemu --test-group rust --arch riscv64
cargo xtask arceos test qemu --test-group rust --arch riscv64 --test-case general
cargo xtask arceos test qemu --test-group rust --arch riscv64 --test-case task-mutex
```

`--plan` 只读取声明与配置，输出逻辑项、执行次数、实际feature组合、构建身份、启动次数、运行检查和宿主辅助设施，不构建镜像或启动目标。默认四架构启动数为RISC-V 6、AArch64 9、x86_64 7、LoongArch64 6；CI另保留RISC-V宿主CPU0绑定的IPI专项。JSON可重定向至 `target/` 等已忽略目录，输出不是源码。测试通过仍要求对应成功标志和外层退出码。

`task-priority` 在同一CPU竞争并核对Fair权重的完成顺序；`task-rt-policy` 包含两个已就绪RR线程不主动yield的轮转检查；`task-yield` 强制两线程的真实FP状态重叠后比较硬件寄存器。`mem-stage1-transition` 同时检查远端写保护、TLB失效、重映射和未映射页故障恢复。网络组 `net-http` 验证真实HTTP往返，不把数字IP解析或仅bind称为DNS/收包测试。

### 2.2 回归证据

`runner.rs` 为每项测试输出 `ARCEOS_TEST_BEGIN` 和 `ARCEOS_TEST_END`，全部成功后输出 `ArceOS test suite run OK!`。测试失败或 panic 必须传播为最外层 `xtask` 的非零退出码，预期异常和 lockdep 检测则使用发现器配置的专用判定规则。panic仅接受本用例预期正文、panic回溯及符号/行号；终止式诊断可能在普通BEGIN送出前结束，按专用诊断和外层PASS验收。

`lockdep/baseline.rs` 的双线程自旋锁测试在释放 A、B 后才发布完成阶段，并在发布前检查两把锁已释放。工作线程持有 A、B 时提前通知，不能保证另一线程取得 B 后也能取得 A；恢复该错误顺序时，前置断言会确定性失败。

`net-unix-path` 在真实任务运行时中验证 Unix socket 的路径绑定和连接：命名空间夹具
明确检查可阻塞上下文并执行睡眠，再验证连接、地址、接收来源及关闭后抽象名称重绑定。
地址状态若由禁止抢占的自旋锁保护，命名空间入口会确定性失败；测试不能用宿主空运行时替代。
该用例独占一次性命名空间注册，因此作为独立内核运行，不与 `general` 中的真实文件系统提供者混用。
它保护网络层的阻塞契约，实际文件系统与系统调用兼容性仍需 Starry 回归或板端测试。

```bash
cargo xtask arceos test qemu --test-group rust --arch aarch64 --test-case net-unix-path
```

## 3. 其他测试入口

`test-suit/arceos` 还包含直接验证 C ABI 和硬件行为的测试。选择 std 是为了覆盖实际调用边界，不应删除这些入口的独立职责。

### 3.1 C 接口

`../c` 通过 `ax-libc` 静态库验证 malloc、pthread、时间、管道、epoll 和 socket 的 C/POSIX 语义。它与 Rust std suite 覆盖不同的消费者入口，继续保留 C harness 和 `ARCEOS_C_TEST_*` 判定。

### 3.2 平台与板卡

非对齐异常表恢复由[CPU套件](../cpu/README.md)的unaligned-fixup成员执行，旧loongarch组仅保留手动兼容转发。`../board-orangepi-5-plus/ipi` 复用 Rust suite 的 `task-ipi` feature及逐远端CPU应答。`../axtest/sg2002-usb-msc` 保留 no_std 硬件 harness，直接检查 USB、IRQ、DMA 和传输完成；其中的纯逻辑模块可以通过宿主单元测试验证，但不能替代实体板卡运行。


## 4. 后续维护规范

### 4.1 文件职责与执行顺序

| 文件 | 维护内容 |
| --- | --- |
| suite.toml | 唯一启动顺序run_order；各批次按tests登记ID、名称、入口、分类及架构 |
| Cargo.toml | 单项feature与依赖；不复制general等批次的成员集合 |
| build.rs、src/suite.rs | 从同一清单生成目标函数绑定；不手写第二份测试表 |
| src/<功能模块> | 真正测试实现与断言 |
| src/runner.rs | 顺序执行、逐项结果、任务身份/亲和性/调度策略恢复、整批完成 |
| qemu-*.toml、cases/<配置>/ | 默认及专属运行环境、输出匹配、宿主夹具 |
| scripts/axbuild/src/test/normal/arceos/rust_suite/ | 宿主登记读取、选择、list/plan及专用判定 |

run_order中每个批次必须恰好出现一次，组定义在文件中的位置不改变启动顺序；tests的声明顺序就是批内执行顺序。一个普通批次启动一次；单项继承对应运行配置，存在自身fixture时优先使用它。不能把逻辑测试项数当作启动次数。

### 4.2 添加与修改

1. 先明确本项目要保护的行为、错误实现及已有覆盖，优先扩展既有完整功能测试。
2. 在src中实现普通函数，返回TestResult；成功Ok，失败Err或assert/panic。使用真实Rust std覆盖std/POSIX调用链，不用mini-std或宿主fake runtime替代目标行为。
3. 增加单项Cargo feature、lib模块声明及suite.toml登记；entry指向真实实现。ID全清单唯一，测试arches不能超出其批次arches。不在SELECTED_TESTS或Cargo中再维护聚合成员。
4. 优先加入运行条件兼容的现有批次；新增批次须补run_order，声明必要架构、features或runtime配置。预期panic、lockdep、栈保护保持独立启动和专用判定。
5. 使用--list/--plan确认选择、架构、features和启动数，运行单项及完整所属批次；需要跨架构能力时覆盖相应架构。不能通过不登记、空函数或成功正则放宽制造通过。

普通测试由runner逐项调用；Err或panic中止正常流程，全部返回成功后才输出ArceOS test suite run OK!。宿主普通配置等待整批标记，并匹配panic/ARCEOS_TEST_FAIL；目前不逐条解析END核对ID与数量。每项自身仍必须有有效断言，runner不能替代功能判定。成员内禁止process::exit或捕获失败后返回Ok。

改变线程亲和性或调度策略时保留RunnerTaskState恢复。后台线程、IRQ、设备、命名空间及其他全局资源由用例自己完成清理，不能假设runner会恢复所有状态。需要独占一次性注册的功能保持自己的runtime。

### 4.3 删除、合并与运行配置

删除前证明已有测试承接独特失败信号；同时检查清单、feature、lib声明、函数、专属依赖/夹具、别名和文档。空批次及run_order项一起移除。板端IPI等消费者仍使用的功能不能直接删除。

合并批次必须确认构建feature、硬件、运行时和退出契约兼容，并实跑合批；函数删除只记负载减少，只有整个独立目标取消才记启动减少。记录前后选择、顺序、构建身份、启动数及通过情况。

普通成功标记只在全体通过后输出；预期诊断只接受该功能的指定证据。host_pcm_waveform、host_http_server、serial-rx等宿主夹具是实际判定的一部分，不因客体输出成功而跳过。--plan默认只写stdout，需要JSON文件时保存到已忽略的target目录。

### 4.4 验收

```bash
cargo xtask arceos test qemu --arch riscv64 --test-group rust --list
cargo xtask arceos test qemu --arch riscv64 --test-group rust --plan
cargo xtask arceos test qemu --arch riscv64 --test-group rust --test-case task-mutex
cargo xtask arceos test qemu --arch riscv64 --test-group rust --test-case general
```

根据实际改动运行cargo fmt、受影响cargo xtask clippy、相关架构QEMU及共享板端消费者。错误修复须保留错误实现失败、修复后通过的命令和日志；宿主执行机制用合成配置验证发现/选择/失败传播，不镜像固定生产清单。新增功能是否接入CI与是否保留实现分别说明，不默认偷偷新增启动。
