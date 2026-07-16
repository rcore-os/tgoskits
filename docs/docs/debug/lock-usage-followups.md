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

- `spin::Mutex` 按非睡眠自旋锁处理，优先迁移到 `ax-kspin` 家族或语义明确的
  `ax_sync::SpinMutex`，不要用兼容名称 `ax_sync::Mutex` 隐藏其非睡眠语义。
- `SpinNoPreempt` 存在同 CPU IRQ 重入风险时，先改成 `SpinNoIrq`。
- 对早期启动、rootfs 挂载、pseudofs 初始化等当前还不完全 sleepable 的路径，先保留非阻塞锁，避免 `might_sleep()` 在启动阶段直接 panic。

这些策略能解除当时的启动和调试阻塞，但并不意味着所有被改成 `SpinNoIrq` 的锁都是合理的最终设计。特别是文件系统、VFS、块设备、用户内存访问和回调路径，只要持锁期间可能睡眠、重调度、触发 page fault、分配内存或执行 I/O，就不应该长期依赖自旋锁包住整段逻辑。

## 统一原则

后续调整锁时按以下原则复查。

1. `spin::Mutex` 的名字不能按 sleepable mutex 理解。它是忙等互斥锁，迁移时应先确认临界区是否真正允许睡眠。
2. `SpinNoPreempt` 只关闭抢占，不关闭本地 IRQ。若锁可能在 IRQ handler、IRQ waker 或 IRQ-enabled 任务上下文之间共享，应优先怀疑同 CPU IRQ 重入死锁。
3. `SpinNoIrq` 只解决 IRQ 重入风险。它仍然让代码处在原子上下文，不能包住会睡眠、重调度、fault user memory、执行 block I/O 或调用未知后端回调的逻辑。
4. `ax_sync::SpinMutex` 和兼容 `ax_sync::Mutex` 固定为非睡眠锁；需要 sleepable
   互斥时必须显式选择 feature-gated `ax_sync::PiMutex`，且仍不能在早期启动、
   IRQ-disabled、preempt-disabled 或其他原子上下文中使用。
5. 改锁类型不是最终目标。更好的修复通常是缩小临界区、移出后端回调、移出用户内存访问、拆分粗粒度文件系统锁，或把初始化工作移动到正常任务上下文。
6. lockdep subclass 只应用于同一抽象下的合法嵌套，不应掩盖真实 ABBA 顺序问题。
7. 近期不以新增或引入 RwLock 作为修复方向。已有 `spin::RwLock` 先作为 lockdep 盲区记录；如果读写分离不是必要语义，可以评估 mutex 化，否则先保持现状并禁止扩大使用面。

## 当前跟踪项

| 区域 | 当前状态 | 风险 | 后续方向 |
| --- | --- | --- | --- |
| `os/arceos/modules/axfs-ng/src/fs/fat/fs.rs` | FAT 主文件系统状态使用 `SpinNoIrq`。 | `read_at`、`write_at`、`append`、`set_len`、`sync` 和目录操作会在持锁期间进入 `fat`，再到块设备 `read_block` / `write_block` / `flush`。这是粗文件系统锁包住 I/O 的典型问题。 | 继续保持为已知技术债。后续应拆分 FAT 内部状态锁，避免持自旋锁进入块 I/O 和外部 sink callback；或者在 sleepable 任务上下文中使用 sleepable 锁。 |
| `os/arceos/modules/axfs-ng/src/fs/ext4/rsext4/fs.rs` | rsext4 主状态使用 `SpinNoIrq`。 | `sync_to_disk`、读写、truncate、create、unlink、rename 等路径可能持锁执行 cache、journal、allocation 和 block-device 操作。 | 不应再机械换成其他自旋锁。需要设计 ext4 粗锁拆分、I/O 外移或早期 rootfs 工作上下文调整。 |
| `os/arceos/modules/axfs-ng/src/fs/ext4/lwext4/fs.rs` | lwext4 文件系统对象使用 `SpinNoIrq`。 | 多个 VFS 操作持锁进入 `lwext4_rust`，`flush()` 也直接在锁内调用后端 flush。 | 与 rsext4 一起复查 ext4 系列锁策略，避免长期在原子上下文包住文件系统实现。 |
| `components/axfs-ng-vfs/src/node/dir.rs` | dentry cache 使用 `SpinNoIrq`，当前已缩小锁范围。 | 旧问题是 cache guard 下调用 filesystem `lookup`、`create`、`unlink`、`open_file` 等后端操作。当前已调整为 VFS cache 锁内只访问 cache map，后端操作在锁外执行。 | 保持当前边界。新增 dentry cache 路径时禁止在 cache guard 内调用后端 FS、socket、设备或用户态相关回调。 |
| `components/axfs-ng-vfs/src/mount.rs` | mountpoint location / children / propagation / peer 关系使用 `SpinNoIrq`，已缩小 bind mount 目标 dentry 锁范围。 | 旧问题是 `bind_mount()` 在目标 mountpoint guard 下构造 bind mount、复制递归子 mount 并更新传播组。当前目标 dentry guard 只检查和安装 mountpoint slot；递归复制、传播组维护在该 guard 外执行。 | 保持 mountpoint slot 锁只保护 slot 本身。后续新增 mount tree 逻辑时，不要在 dentry mountpoint guard 内做递归复制、传播组更新、后端 FS 调用或可能扩大的跨 mount 操作。 |
| `os/StarryOS/kernel/src/pseudofs/tmp.rs` | tmpfs root、目录 entries、metadata 使用 `SpinNoIrq`，length / symlink 使用兼容 `ax_sync::Mutex`（同样是 `SpinNoIrq`）；`read_dir()` 已把 sink callback 移到 entries 锁外。 | tmpfs 状态当前全部处于非睡眠锁保护下，以兼容 inode release 和早期启动约束；`read_dir()` 锁内只快照目录项，不再持 guard 调用外部 sink 或读取 inode metadata。 | 继续保持锁内不做回调、不访问用户内存、不进入 VFS 后端。后续若确认运行在可睡眠任务上下文，再逐点评估显式 `PiMutex` 或进一步缩短锁范围。 |
| `os/StarryOS/kernel/src/file/epoll.rs` | `mode`、`interests`、`ready_queue` 使用 `SpinNoIrq`，ready queue fast path 已调整。 | 旧问题是 `ready_queue` 可从 waker 路径入队，`VecDeque::push_back` 可能扩容。当前 `EPOLL_CTL_ADD` 预留 ready queue 容量，消费队列时保留全局 queue capacity，waker 入队只使用已有容量；容量意外不足时设置 overflow 标志，后续在 `epoll_wait` 任务上下文扫描恢复 ready 项。 | 保持 waker fast path 不做堆扩容。`PollSet::wake()` 本身若进入 IRQ 路径仍属于更大范围的 poll/waker bridge 设计问题，后续单独审计。 |
| `os/StarryOS/kernel/src/pseudofs/dev/loop_block.rs` | loop block cache blocks 使用 `SpinNoIrq`。 | 当前临界区主要是 bounded memory copy，风险可控；但它处在 ext4 block-device 回调路径上，不能在锁内做 VFS writeback 或分配。 | 继续保持锁内只做内存拷贝。若以后增加 writeback、动态扩容或 VFS 调用，必须重新设计。 |
| `os/StarryOS/kernel/src/pseudofs/dev/tty/terminal/mod.rs` | `window_size`、`termios` 使用 `SpinNoIrq`；job-control `session` / `foreground` 已合并为一个短状态锁。 | ioctl 路径先完成用户内存访问，再短暂持锁更新值；job-control 不再存在 `foreground -> session` 和 `session -> foreground` 的相反加锁顺序。 | 保持“copy user 在锁外，锁内只拷贝小对象”的规则。job-control 复合状态继续用单锁保护，`PollSet::wake()` 放在锁外执行。 |
| `os/StarryOS/kernel/src/pseudofs/dev/tty/pty.rs` | PTY producer 使用 `SpinNoIrq`。 | 当前只在锁内 `push_slice` 到 4 KiB ring buffer，wake 在锁外。风险较低。 | 保持短临界区。若未来 writer 可从 IRQ 直接进入或 buffer 需要扩容，应重新评估。 |
| `net/ax-net/src/unix/mod.rs` | 已调整为从 VFS `user_data` 中 clone `Arc<BindSlot>` 后释放 guard，再调用 transport。 | 旧问题是持 VFS `SpinNoIrq` guard 调用 socket transport，内部再拿 sleepable socket mutex。 | 保持现有边界。新增 path socket 操作时禁止把 transport callback 放在 VFS user_data guard 内执行。 |
| `os/StarryOS/kernel/src/file/netlink.rs` 和 `os/StarryOS/kernel/src/file/packet.rs` | 已调整为锁内取出消息或包，锁外 copy 到用户缓冲区。 | 旧问题是持 `SpinNoIrq` 时写用户内存，page fault 路径要求 IRQ enabled。 | 保持“队列状态锁内移动数据，用户内存访问锁外执行”的规则。 |
| `os/arceos/modules/axfs-ng/src/highlevel/file.rs` | 仍有 `spin::RwLock`，包括 `GLOBAL_CACHED_FILES` 和 `append_lock`。 | `spin::RwLock` 仍不属于 lockdep-aware `ax-kspin` / `ax-sync` 路径。`append_lock` 是历史记录中明确延期的 RwLock 设计问题。 | 近期不引入新 RwLock。先确认是否必须读写分离；不必须的点评估 mutex 化，必须的点保留并记录风险。 |
| `os/StarryOS/kernel/src/file/mod.rs`、`task/mod.rs`、`task/ops.rs` 等 | 仍有若干 `spin::RwLock`；`file/signalfd.rs` 的 signal mask 已改成 `SpinNoIrq<SignalSet>`。 | 剩余 RwLock 还不在 lockdep 统一可见范围内，且可能参与 FD table、task 状态等运行时路径。signalfd mask 只是单个可拷贝 bitset，当前不需要外部 `spin::RwLock`。 | 按运行时重要性分批审计：能 mutex 化的先 mutex 化；不能 mutex 化的先冻结新增使用，不把 RwLock 作为近期替换目标。后续若实现项目自有、lockdep-aware 的 RwLock，再评估 signalfd mask 是否值得恢复读写分离。 |
| drivers / portable crates 中的 `spin::Mutex` | 业务代码中的直接 `spin::Mutex` 已清理，vendored `spin` 也不再暴露 Mutex API。 | 后续若 portable crate 新增锁，仍不能绕回外部 `spin::Mutex`，否则内核运行路径会重新形成 lockdep 盲区。 | 继续依赖 `spin-lint` 和编译期 API 缺失防回退。新增 driver 锁时按 crate 边界选择项目内锁或明确的同步抽象。 |

## 调整计划

近期目标不是把所有锁替换成某一种统一原语，而是让锁的使用与上下文匹配。优先级按风险和依赖关系排列。

| 优先级 | 工作项 | 范围 | 完成标准 |
| --- | --- | --- | --- |
| P0 | 建立锁使用分类清单 | `SpinNoIrq`、`SpinNoPreempt`、剩余 `spin::RwLock`、关键 atomic | 每个候选点标出保护对象、是否 IRQ/waker 路径、是否可能 sleep / fault / 分配 / I/O / 回调。 |
| P1 | 缩小 VFS dentry / mount 锁范围 | `components/axfs-ng-vfs/src/node/dir.rs`、`components/axfs-ng-vfs/src/mount.rs` | VFS 自旋锁内只做 cache / mount 元数据操作；FS 后端调用在锁外执行。 |
| P1 | 清理自旋锁内的高风险操作 | 用户内存访问、后端 callback、block I/O、可能分配的 waker 入队 | `might_sleep()` 覆盖路径下不再出现持 spin guard 的 user copy / callback / I/O。 |
| P1 | 修正 epoll ready queue fast path | `os/StarryOS/kernel/src/file/epoll.rs` | waker 路径不做堆扩容；通过预分配、限长队列或延迟到任务上下文解决。 |
| P2 | 重新设计 FAT/ext4/lwext4 粗文件系统锁 | `os/arceos/modules/axfs-ng/src/fs/` | 文件系统锁不再长期包住 block I/O / flush / journal / 外部 sink callback，或明确只在 sleepable 上下文使用 sleepable mutex。 |
| P2 | 明确早期启动 sleepability 边界 | rootfs mount、pseudofs init、tmpfs root_dir | 区分早期启动误伤和运行期 atomic sleep bug；减少因为启动阶段限制而长期保留自旋锁的场景。 |
| P2 | 复查 tmpfs 保守自旋锁 | `os/StarryOS/kernel/src/pseudofs/tmp.rs` | VFS 后端调用移出 spin guard 后，评估 tmpfs entries / metadata 是否能改成 mutex 或进一步缩短自旋锁范围。 |
| P3 | 处理已有 `spin::RwLock` 盲区 | `axfs-ng` highlevel file、Starry FD/task/signal 等 | 不新增 RwLock 方案；逐点判断能否 mutex 化，不能的记录为 deferred 并冻结新增使用。 |
| P3 | portable drivers 同步抽象 | `drivers/`、`memory/` 中后续新增或调整的锁 | 区分 portable core 和 OS glue；内核运行路径不重新直接依赖外部 `spin::Mutex` 作为默认锁。 |
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
- `might_sleep()` 已纳入显式 IRQ context，并能在 `lockdep` 构建下输出 held-lock stack；held non-sleep lock 作为直接触发条件仍待后续阶段。锁策略调整仍应先消除 spin guard 内的 fault、alloc、I/O、callback，而不是依赖诊断机制长期兜底；详细计划见 [`might_sleep` 后续增强计划](./might-sleep-followups.md)。

## 复查命令

外部 `spin::Mutex` / `spin::RwLock` 复查。`spin::Mutex` 结果应只剩历史文档
引用；`spin::RwLock` 仍是后续阶段：

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

rg -n "ax_kspin::PreemptGuard|PreemptGuard::new\(|NoPreemptGuard::new\(" \
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
