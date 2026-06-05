---
sidebar_position: 7
sidebar_label: "锁使用问题跟踪"
---

# 锁使用问题跟踪

本文档用于跟踪外部 `spin` 迁移后暴露出的锁使用问题，以及后续对锁范围、锁类型和上下文语义的调整进度。

它整理自历史记录：

- `reports/external-spin-audit.md`
- `reports/external-spin-migration-plan.md`
- `reports/spin-no-preempt-audit.md`

这些报告曾在 #1064 中删除。本文档保留其中仍然有效的判断，并按当前代码路径重新整理成后续维护用的跟踪清单。

## 背景

早期清理外部 `spin` crate 的直接目标是消除第三方 `spin::{Mutex,RwLock}` 在内核锁依赖检查中的可见性缺口。外部 `spin` 锁不会进入 `ax-kspin` / `ax-sync` 的 lockdep 路径，因此即使真实存在锁顺序反转，也可能无法被 lockdep 捕获。

当时采用了保守策略：

- `spin::Mutex` 按非睡眠自旋锁处理，优先迁移到 `ax-kspin` 家族，而不是机械迁移到 `ax_sync::Mutex`。
- `SpinNoPreempt` 存在同 CPU IRQ 重入风险时，先改成 `SpinNoIrq`。
- 对早期启动、rootfs 挂载、pseudofs 初始化等当前还不完全 sleepable 的路径，先保留非阻塞锁，避免 `might_sleep()` 在启动阶段直接 panic。

这些策略能解除当时的启动和调试阻塞，但并不意味着所有被改成 `SpinNoIrq` 的锁都是合理的最终设计。特别是文件系统、VFS、块设备、用户内存访问和回调路径，只要持锁期间可能睡眠、重调度、触发 page fault、分配内存或执行 I/O，就不应该长期依赖自旋锁包住整段逻辑。

## 统一原则

后续调整锁时按以下原则复查。

1. `spin::Mutex` 的名字不能按 sleepable mutex 理解。它是忙等互斥锁，迁移时应先确认临界区是否真正允许睡眠。
2. `SpinNoPreempt` 只关闭抢占，不关闭本地 IRQ。若锁可能在 IRQ handler、IRQ waker 或 IRQ-enabled 任务上下文之间共享，应优先怀疑同 CPU IRQ 重入死锁。
3. `SpinNoIrq` 只解决 IRQ 重入风险。它仍然让代码处在原子上下文，不能包住会睡眠、重调度、fault user memory、执行 block I/O 或调用未知后端回调的逻辑。
4. `ax_sync::Mutex` 适合需要 sleepable 互斥的路径，但不能在早期启动、IRQ-disabled、preempt-disabled 或其他原子上下文中随意使用。
5. 改锁类型不是最终目标。更好的修复通常是缩小临界区、移出后端回调、移出用户内存访问、拆分粗粒度文件系统锁，或把初始化工作移动到正常任务上下文。
6. lockdep subclass 只应用于同一抽象下的合法嵌套，不应掩盖真实 ABBA 顺序问题。
7. 近期不以新增或引入 RwLock 作为修复方向。已有 `spin::RwLock` 先作为 lockdep 盲区记录；如果读写分离不是必要语义，可以评估 mutex 化，否则先保持现状并禁止扩大使用面。

## 当前跟踪项

| 区域 | 当前状态 | 风险 | 后续方向 |
| --- | --- | --- | --- |
| `os/arceos/modules/axfs-ng/src/fs/fat/fs.rs` | FAT 主文件系统状态使用 `SpinNoIrq`。 | `read_at`、`write_at`、`append`、`set_len`、`sync` 和目录操作会在持锁期间进入 `fatfs`，再到块设备 `read_block` / `write_block` / `flush`。这是粗文件系统锁包住 I/O 的典型问题。 | 继续保持为已知技术债。后续应拆分 FAT 内部状态锁，避免持自旋锁进入块 I/O 和外部 sink callback；或者在 sleepable 任务上下文中使用 sleepable 锁。 |
| `os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/fs.rs` | rsext4 主状态使用 `SpinNoIrq`。 | `sync_to_disk`、读写、truncate、create、unlink、rename 等路径可能持锁执行 cache、journal、allocation 和 block-device 操作。 | 不应再机械换成其他自旋锁。需要设计 ext4 粗锁拆分、I/O 外移或早期 rootfs 工作上下文调整。 |
| `os/arceos/modules/axfs-ng/src/fs/ext4/lwext4/fs.rs` | lwext4 文件系统对象使用 `SpinNoIrq`。 | 多个 VFS 操作持锁进入 `lwext4_rust`，`flush()` 也直接在锁内调用后端 flush。 | 与 rsext4 一起复查 ext4 系列锁策略，避免长期在原子上下文包住文件系统实现。 |
| `components/axfs-ng-vfs/src/node/dir.rs` | dentry cache 和 mountpoint 相关状态使用 `SpinNoIrq`。 | cache guard 下会调用 filesystem `lookup`、`create`、`unlink`、`open_file` 等后端操作。后端如果使用 sleepable mutex、分配、I/O 或用户态相关路径，会触发 `might_sleep()` 或形成锁顺序问题。 | 优先拆出“查 cache / 改 cache”和“调用 FS 后端”的边界。目标是持 VFS 自旋锁时只做内存元数据操作。 |
| `components/axfs-ng-vfs/src/mount.rs` | mountpoint location / children / propagation / peer 关系使用 `SpinNoIrq`。 | `Location::mount()` 曾在持 VFS mountpoint 锁时调用 `fs.root_dir()`，触发 tmpfs 早期路径问题。 | 复查 mount 流程是否仍有持锁调用后端的路径；必要时把 root dir 获取、backend 初始化移出 VFS 锁。 |
| `os/StarryOS/kernel/src/pseudofs/tmp.rs` | tmpfs root、目录 entries、metadata 使用 `SpinNoIrq`，length / symlink 使用 `ax_sync::Mutex`。 | tmpfs 目录 map 当前必须兼容 VFS cache guard 下的调用。它解决了早期 panic，但把目录元数据保留在自旋锁下。 | 等 VFS 不再持 cache guard 调 FS 后端后，复查 tmpfs directory map 是否能回到 sleepable 锁，或至少保证持锁期间不分配、不 fault、不回调。 |
| `os/StarryOS/kernel/src/file/epoll.rs` | `mode`、`interests`、`ready_queue` 使用 `SpinNoIrq`。 | `ready_queue` 可从 waker 路径入队，`VecDeque::push_back` 可能分配；IRQ wake 路径中分配内存不是稳定设计。 | 为 ready queue 设计预分配、限长队列或延迟执行机制，明确 waker fast path 不做堆扩容。 |
| `os/StarryOS/kernel/src/pseudofs/dev/loop_block.rs` | loop block cache blocks 使用 `SpinNoIrq`。 | 当前临界区主要是 bounded memory copy，风险可控；但它处在 ext4 block-device 回调路径上，不能在锁内做 VFS writeback 或分配。 | 继续保持锁内只做内存拷贝。若以后增加 writeback、动态扩容或 VFS 调用，必须重新设计。 |
| `os/StarryOS/kernel/src/pseudofs/dev/tty/terminal/mod.rs` | `window_size`、`termios` 使用 `SpinNoIrq`。 | 当前 ioctl 路径先完成用户内存访问，再短暂持锁更新值。风险较低，但未来容易因为 guard 生命周期扩展而回归。 | 保持“copy user 在锁外，锁内只拷贝小对象”的规则。可补 helper 收敛读写模式。 |
| `os/StarryOS/kernel/src/pseudofs/dev/tty/pty.rs` | PTY producer 使用 `SpinNoIrq`。 | 当前只在锁内 `push_slice` 到 4 KiB ring buffer，wake 在锁外。风险较低。 | 保持短临界区。若未来 writer 可从 IRQ 直接进入或 buffer 需要扩容，应重新评估。 |
| `os/arceos/modules/axnet-ng/src/unix/mod.rs` | 已调整为从 VFS `user_data` 中 clone `Arc<BindSlot>` 后释放 guard，再调用 transport。 | 旧问题是持 VFS `SpinNoIrq` guard 调用 socket transport，内部再拿 sleepable socket mutex。 | 保持现有边界。新增 path socket 操作时禁止把 transport callback 放在 VFS user_data guard 内执行。 |
| `os/StarryOS/kernel/src/file/netlink.rs` 和 `os/StarryOS/kernel/src/file/packet.rs` | 已调整为锁内取出消息或包，锁外 copy 到用户缓冲区。 | 旧问题是持 `SpinNoIrq` 时写用户内存，page fault 路径要求 IRQ enabled。 | 保持“队列状态锁内移动数据，用户内存访问锁外执行”的规则。 |
| `os/arceos/modules/axfs-ng/src/highlevel/file.rs` | 仍有 `spin::RwLock`，包括 `GLOBAL_CACHED_FILES` 和 `append_lock`。 | `spin::RwLock` 仍不属于 lockdep-aware `ax-kspin` / `ax-sync` 路径。`append_lock` 是历史记录中明确延期的 RwLock 设计问题。 | 近期不引入新 RwLock。先确认是否必须读写分离；不必须的点评估 mutex 化，必须的点保留并记录风险。 |
| `os/StarryOS/kernel/src/file/mod.rs`、`task/mod.rs`、`task/ops.rs`、`file/signalfd.rs` 等 | 仍有若干 `spin::RwLock`。 | 这些锁还不在 lockdep 统一可见范围内，且可能参与 FD table、task 状态、signal mask 等运行时路径。 | 按运行时重要性分批审计：能 mutex 化的先 mutex 化；不能 mutex 化的先冻结新增使用，不把 RwLock 作为近期替换目标。 |
| drivers / portable crates 中的 `spin::Mutex` | 当前代码仍可搜到若干直接使用，例如 `drivers/usb/usb-host`、`drivers/ax-driver`、`drivers/rdrive`、`drivers/firmware/arm-scmi-rs`、`drivers/interface/rdif-serial`、`memory/buddy-slab-allocator`。 | portable crate 不能简单依赖 ArceOS 专用锁；同时这些锁若进入内核运行路径，仍可能是 lockdep 盲区。 | 需要按 crate 边界设计同步抽象，区分纯 portable、test-only、host-only 和内核运行路径。 |

## 调整计划

近期目标不是把所有锁替换成某一种统一原语，而是让锁的使用与上下文匹配。优先级按风险和依赖关系排列。

| 优先级 | 工作项 | 范围 | 完成标准 |
| --- | --- | --- | --- |
| P0 | 建立锁使用分类清单 | `SpinNoIrq`、`SpinNoPreempt`、`spin::Mutex`、`spin::RwLock`、关键 atomic | 每个候选点标出保护对象、是否 IRQ/waker 路径、是否可能 sleep / fault / 分配 / I/O / 回调。 |
| P1 | 缩小 VFS dentry / mount 锁范围 | `components/axfs-ng-vfs/src/node/dir.rs`、`components/axfs-ng-vfs/src/mount.rs` | VFS 自旋锁内只做 cache / mount 元数据操作；FS 后端调用在锁外执行。 |
| P1 | 清理自旋锁内的高风险操作 | 用户内存访问、后端 callback、block I/O、可能分配的 waker 入队 | `might_sleep()` 覆盖路径下不再出现持 spin guard 的 user copy / callback / I/O。 |
| P1 | 修正 epoll ready queue fast path | `os/StarryOS/kernel/src/file/epoll.rs` | waker 路径不做堆扩容；通过预分配、限长队列或延迟到任务上下文解决。 |
| P2 | 重新设计 FAT/ext4/lwext4 粗文件系统锁 | `os/arceos/modules/axfs-ng/src/fs/` | 文件系统锁不再长期包住 block I/O / flush / journal / 外部 sink callback，或明确只在 sleepable 上下文使用 sleepable mutex。 |
| P2 | 明确早期启动 sleepability 边界 | rootfs mount、pseudofs init、tmpfs root_dir | 区分早期启动误伤和运行期 atomic sleep bug；减少因为启动阶段限制而长期保留自旋锁的场景。 |
| P2 | 复查 tmpfs 保守自旋锁 | `os/StarryOS/kernel/src/pseudofs/tmp.rs` | VFS 后端调用移出 spin guard 后，评估 tmpfs entries / metadata 是否能改成 mutex 或进一步缩短自旋锁范围。 |
| P3 | 处理已有 `spin::RwLock` 盲区 | `axfs-ng` highlevel file、Starry FD/task/signal 等 | 不新增 RwLock 方案；逐点判断能否 mutex 化，不能的记录为 deferred 并冻结新增使用。 |
| P3 | portable drivers 同步抽象 | `drivers/`、`memory/` 中的 `spin::Mutex` | 区分 portable core 和 OS glue；内核运行路径不继续直接依赖外部 `spin::Mutex` 作为默认锁。 |
| P3 | atomic 与锁的合理性审计 | mount flags、epoll membership、cached file reclaim、file flags 等 | 独立标志可保留 atomic；复合不变量、flag+data 发布协议、队列/map 生命周期应回到锁或明确内存序协议。 |

### Atomic 使用准则

适合使用 atomic：

- 独立 bool、bit flag、计数器或 ID 分配器。
- 不需要和其他字段保持一致的轻量状态。
- fast path 去重位，但真实队列或 map 仍由锁保护。

不适合只用 atomic：

- 多字段复合不变量。
- 队列、map、slab、生命周期状态。
- `flag + data` 发布协议但没有清晰 Acquire / Release 关系。
- 需要和锁内数据保持一致的状态，例如队列成员关系、mount tree 关系、reclaim 全局状态。

## 已形成的经验

以下问题已经有明确处理模式，后续改动应保持这些边界。

- 用户内存访问不能放在 `SpinNoIrq` / `SpinNoPreempt` guard 内。先从队列或状态中取出数据，释放 guard 后再 `vm_read` / `vm_write` / `IoDst::write`。
- waker fast path 不应做可能分配的结构扩容。若必须从 IRQ wake 路径进入，应使用预分配、固定容量或延迟到任务上下文。
- VFS cache / user_data guard 内不应执行文件系统、socket、设备等后端回调。应先复制出必要的 `Arc` 或小状态，再释放 guard。
- 文件系统粗锁包住 block I/O 是当前最大的未完成项。把 `SpinNoPreempt` 改成 `SpinNoIrq` 只是关闭 IRQ 重入风险，不代表 I/O under spin lock 合理。
- 早期启动阶段的 `might_sleep()` 误伤和真实运行期 atomic sleep bug 需要区分。长期方向是让启动阶段进入更清晰的 sleepability 状态，或把 rootfs / pseudofs 初始化移动到正常任务上下文。

## 复查命令

外部 `spin::Mutex` / `spin::RwLock` 复查：

```bash
rg -n "^use spin::Mutex|^use spin::\{[^}]*Mutex|spin::Mutex|spin::MutexGuard|spin::mutex::" \
  --glob '*.rs' --glob '!target/**'

rg -n "spin::RwLock|use spin::RwLock" \
  --glob '*.rs' --glob '!target/**'
```

`SpinNoPreempt` / `NoPreempt` 复查：

```bash
rg -n "SpinNoPreempt|SpinNoPreemptGuard|BaseSpinLock<NoPreempt|NoPreempt" \
  --glob '*.rs' --glob '!target/**'

rg -n "ax_kernel_guard::NoPreempt|NoPreempt::new\(|NoPreemptGuard::new\(" \
  --glob '*.rs' --glob '!target/**'
```

可能在自旋锁内执行用户内存访问或回调的路径，不能只靠 `rg` 判定。建议从以下入口做人工复查：

- `components/axfs-ng-vfs/src/node/dir.rs`
- `components/axfs-ng-vfs/src/mount.rs`
- `os/arceos/modules/axfs-ng/src/fs/`
- `os/arceos/modules/axfs-ng/src/highlevel/file.rs`
- `os/StarryOS/kernel/src/file/epoll.rs`
- `os/StarryOS/kernel/src/file/netlink.rs`
- `os/StarryOS/kernel/src/file/packet.rs`
- `os/StarryOS/kernel/src/pseudofs/tmp.rs`
- `os/StarryOS/kernel/src/pseudofs/dev/loop_block.rs`

## 验证要求

调整锁相关逻辑时，至少做以下验证：

1. 运行 `cargo fmt`。
2. 对每个修改过的 crate 运行针对性 clippy，优先使用：

```bash
cargo xtask clippy --package <crate>
```

3. 若改动 `ax-kspin`、`ax-sync`、lockdep 或 `might_sleep()` 覆盖路径，补跑对应 lockdep / multitask 回归。
4. 若改动 VFS、FAT、ext4、tmpfs、loop block 相关锁，补跑能覆盖 mount、read/write、sync、rename、unlink、page cache 或 loop rootfs 的 Starry / ArceOS 用例。
5. 若改动用户内存访问和 socket / netlink / packet 队列路径，确保 faultable user copy 不在自旋锁 guard 内。

## 记录更新规则

每次对上表中的锁策略做实质调整，应同步更新本文档：

- 写明旧锁类型和新锁类型。
- 写明锁内还剩哪些操作。
- 写明是否仍可能 sleep、fault、分配、I/O 或回调。
- 写明本次验证命令和结果。
- 如果只是保守止血，不要把状态标成完成，应明确留下后续设计方向。
