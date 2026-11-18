# 文件系统系统调用兼容性记录

## 1. 基准与证据

本记录随所有权迁移逐入口补齐。当前范围是接续记录第 9.31 至 9.41 节的 scoped 查找、proc 自指链接、拥有型结果及文件系统实例，其他元数据、命名、挂载、凭据、映射与套接字入口继续按主计划核对。每行结论仅覆盖注明的参数与对象范围。

### 1.1 固定源码

Linux 基准为 v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。编号分别核对该版本的 x86 `syscall_64.tbl`、通用 `scripts/syscall.tbl` 和架构选择；Starry 核对 `syscall/mod.rs` 的真实分派。AArch64、RISC-V 与 LoongArch64 的通用 64 位入口采用 56/openat、61/getdents64、78/readlinkat、79/newfstatat、80/fstat、291/statx、437/openat2；Starry 对 79 的 Rust 枚举命名因目标而异。

### 1.2 运行条件

拥有型迁移源码 `802004f30c` 与夹具提交 `272141b52a` 的 18 包标准库及 AX/runtime/Kernel 的 106 项 Clippy 通过；x86_64 的 ownership、路径和 proc 程序分别为 107/107、45/45 和 1/1。同一阶段完整 system 为 534/541，通过范围与七个失败分别保留。

类型化父解析 `cf78f01c61` 的原权限程序在四架构均为 1/1，AX/Kernel 的 78 项 Clippy 通过；x86 ownership、path 分别保持 107/107、45/45。`5111b45358` 随后修正交换操作的尾斜杠前置判定，原有完整改名旅程的同一增强断言先失败后通过，最终 18 包标准库及 AX 六项 Clippy 通过。这一后续修正没有增加 MemoryFs 的交换能力，也没有把前一版本的四架构 ELF 改写为它的运行结果。

前一 proc 阶段 `ac8e8bdd61` 的同一完整程序在 AArch64 为 1/1，在 Linux 6.8 的私有用户、PID、挂载命名空间通过。后者提供可观察对照，固定 v7.1 源码决定语义。这些历史结果不改写为本次所有权迁移在该架构的运行证明；RISC-V、LoongArch64 及未直接运行的边界仍保留证据缺口。

## 2. 查找与打开

`sys_openat2` 先解释输入结构和 flag，再选定普通、BENEATH 或 IN_ROOT 起点，并把策略交给 `FsView`。普通和受限请求共用 `LookupSession/Continuation`，创建的原观察交到实际槽位命令。

### 2.1 受限策略

下列各行分别覆盖互斥根策略、正交限制和缓存能力。组件的受控 rename 与槽位失败证据证明内部协议；完整系统调用的并发、参数及其余目标运行继续补齐。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat2/BENEATH；四架构 437 | v7.1 [handle_dots](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2223) | 从实际起点限制遍历；绝对输入、绝对链接及确定向上越界为 EXDEV，不能排除竞争时 EAGAIN | sys_openat2 → with_fs → FsView policy → LookupSession → PathObservation；创建进入 NamespaceMutation 实际槽位 | 部分正确 | x86 普通相对、绝对拒绝及 parent escape 通过；MemoryFs 受控移动与槽位红绿通过；其余架构运行及直接 syscall 并发旅程待补 |
| openat2/IN_ROOT；四架构 437 | v7.1 [path_init](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2714) | dirfd 成为本次根，绝对路径与链接使用该根，根处 parent traversal 钳制；与 BENEATH 互斥为 EINVAL | sys_openat2 → FsView policy → LookupSession.root/ascend/jump_root；不修改线程共享 root/cwd | 部分正确 | x86 绝对路径、绝对链接和 parent 钳制通过；复杂挂载、并发与其他目标运行待补 |
| openat2/NO_XDEV；四架构 437 | v7.1 [nd_jump_root](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L1127) | 按实际 mount 身份拒绝向下、向上及对象跳转，EXDEV；初始绝对输入本身允许选择根 | sys_openat2 → LookupRestrictions → LookupSession.bind/ascend/jump_root/follow | 部分正确 | x86 same mount、向下 tmpfs 与向上 mount crossing 通过；复杂覆盖层及其他目标运行待补 |
| openat2/NO_SYMLINKS；四架构 437 | v7.1 [pick_link](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 跟随任意 symlink 为 ELOOP；末链接不跟随且 O_PATH 时保留链接 | sys_openat2 → LookupSession.follow；Continuation.finish_open 保留不跟随的末项 | 部分正确 | x86 普通路径、跟随拒绝和末链接 O_PATH nofollow 通过；其他目标运行待补 |
| openat2/NO_MAGICLINKS；四架构 437 | v7.1 [nd_jump_link](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L1163) | 普通文本链接允许；对象回调保留自身错误，成功对象跳转为 ELOOP；nofollow 末链接可持有 | sys_openat2 → LookupSession.follow → LinkOps.target(saved caller) → Text/Object 分派 | 部分正确 | x86 普通链接、proc fd 对象拒绝、末项 nofollow、self/thread-self 文本跟随通过；其他目标及完整错误顺序待补 |
| openat2/CACHED O_PATH；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1293)、[缓存遍历](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2678) | 只使用有效缓存；需慢路径或观察失效时 EAGAIN，不能由缓存中没有名字推断 ENOENT | sys_openat2 → try_cached_fs_binding → CachedLookup/PathObservation → OpenOptions.open_cached_path → path_only 或 Directory → captured CachedFileTable.reserve/FileDescriptorReservation.try_install | 部分正确 | 同一 C 的 Linux 通过、6a4a9f23f1 三项 EOPNOTSUPP 失败、46138dd272 x86 QEMU 1/1 通过；缓存正项、tmpfs 未命中、实际 inode、CLOEXEC 与挂载引用见 15.1；02baa6365f 的旧 C 在四架构 CI 均通过；受控任务发布竞争及全部组合仍待补 |
| openat2/CACHED 普通只读文件；四架构 437 | v7.1 [complete_walk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L1045)、[may_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4238) | 缓存解析完成后按本次身份检查最终对象，每次打开取得新的打开描述和零位置 | sys_openat2 → captured binding/CachedLookup → OpenOptions.open_cached_path → cached_metadata/DAC → CachedFile.try_existing_memory → File.from_cached_readonly → original fd table | 部分正确 | 4c2ab40a5e 相同 C 在 Linux 通过，旧 Starry 缺少文件与目录只读打开并有两个 nofollow 失败；536b9c90af x86 QEMU 1/1，原数据、独立游标与实际描述见 15.4；原生后端、额外策略与其他架构本轮运行仍待补 |
| openat2/CACHED 只读目录；四架构 437 | v7.1 [do_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4655) | 检查目录读权限，描述保留原目录并拥有独立枚举位置 | sys_openat2 → CachedLookup → OpenOptions.open_cached_path cached DAC → OpenResult.Dir(original PathHandle) → Directory.new | 部分正确 | 相同 C 的 Linux 与 x86 QEMU 均打开目录并原始 getdents64 读到条目；未把其他类型、后端及目录枚举边界当作已证明 |
| openat2/CACHED 写入与额外准入；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1293) | create/truncate/tmpfile 组合先为 EAGAIN；已有对象的写打开及 O_NOATIME 等策略另按各自条件准入 | sys_openat2 保留变更类参数顺序；open_cached_path 仍仅接受 O_PATH 和受限普通只读，其他准入返回 WouldBlock | 不正确 | 写打开、端点及额外策略仍未实现，不能从只读数据旅程推断完整 CACHED 支持 |
| openat2/CACHED 文本链接；四架构 437 | v7.1 [pick_link](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2003) | 可用的缓存链接参与同一次预算和 root/mount 策略，不能调用需要慢路径的链接 producer | sys_openat2 → CachedLookup.walk/path frames → NodeOps.cached_link_target → MemoryFs 实际 Arc 文本；普通与缓存共用 SYMLINKS_MAX | 部分正确 | 同一 C 在 Linux 通过、5983630d73 失败 10 项、c854aa5f7f x86 QEMU 1/1；覆盖绝对/相对、IN_ROOT/BENEATH、NO_MAGICLINKS/NO_SYMLINKS/nofollow、尾斜杠及预热后 40/41 预算；02baa6365f 的旧 C 已在四架构 CI 通过；atime、挂载时间策略、动态 producer 和新增 nofollow 组合仍由后续阶段核对 |
| openat2/CACHED BENEATH dotdot；四架构 437 | v7.1 [follow_dotdot_rcu](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2151)、[try_to_unlazy](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L930) | 根处向上遍历在 RCU 返回 ECHILD，CACHED 不能进入 ref-walk，最终 EAGAIN；绝对输入和绝对链接仍为 EXDEV | sys_openat2 → CachedLookup.ascend root check → WouldBlock；绝对链接经 jump_root → CrossesDevices | 部分正确 | 相同 C 的 Linux EAGAIN，5983630d73 错为 EXDEV，c854aa5f7f x86 客机 EAGAIN；绝对链接另经 EXDEV 断言，其他目标和受控并发仍待补 |
| openat2/CACHED NO_XDEV 向上；四架构 437 | v7.1 [follow_dotdot_rcu](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2151) | 向上跨挂载需要离开 RCU 时缓存模式为 EAGAIN；不能套用普通模式的 EXDEV | sys_openat2 → CachedLookup.ascend → cached_covered_location → NO_XDEV/WouldBlock，保持原路径引用 | 部分正确 | Linux 对照 EAGAIN，5983630d73 的 EXDEV 稳定失败，c854aa5f7f x86 QEMU 通过；向下及全部组合另需相应证明 |
| openat2/CACHED 原生后端元数据；四架构 437 | v7.1 [inode_permission](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c) | 每个搜索目录检查实际有效属性；需要 I/O 或重验证则为 EAGAIN | CachedLookup.check_search → NodeOps.cached_metadata；目前仅 MemoryNode 实现，其余默认 WouldBlock | 不正确 | 实际 tmpfs 属性已接入；原生 Ext4、FAT 和动态 producer 的缓存属性能力尚未实现，既有负项版本证明不能替代此门禁 |

### 2.2 普通打开与链接句柄

普通打开沿用保存的 caller 与 FsView，proc fd 路径经实际对象链接转交至 add_to_fd。自指链接作为普通文本 symlink 参与原会话的总预算；PTY 实例与挂载表的 per-open producer 仍需迁移。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| open；x86_64 2 | v7.1 [do_sys_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 普通路径参与同一总链接预算，跟随和创建由 flags 决定 | syscall/mod.rs → sys_open → sys_openat → one with_fs → OpenOptions/LookupSession → add_to_fd；无 fd 前缀分流 | 部分正确 | x86 完整路径旅程含 legacy open 的 40/41 总预算和悬空创建；其余消费者继续迁移 |
| openat；x86_64 257；其余三架构 56 | v7.1 [do_sys_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 绝对输入忽略 dirfd；O_PATH + O_NOFOLLOW 获取 self/thread-self 链接自身 | sys_openat → flags_to_options → one FsView/LookupSession → add_to_fd borrowing same view；保存 caller 和实际 PathHandle | 部分正确 | d788254e65 的 x86 完整 system 541/541；同一 procfd 程序在旧实现和不完整通用分派上失败，最终通过；PTY、per-open 与非阻塞交接仍待补 |
| openat2/resolve=0；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1190) | 严格校验 how 后按普通打开行为解析 | sys_openat2 → 参数检查 → sys_openat；同一线程入口 caller，普通查找会话 | 部分正确 | x86 完整路径旅程的 resolve=0、预算、悬空创建和 proc fd 对象通过；结构边界、其余消费者与其他目标运行待补 |
| creat；x86_64 85 | v7.1 [creat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 按 WRONLY + CREAT + TRUNC 解释路径 | sys_creat → sys_openat → LookupSession；仍受普通创建与截断阶段限制 | 无法确认 | 本阶段未直接调用 legacy creat；不能由 open/openat 的通过直接提升结论 |

## 3. 链接显示与目录观察

`SelfLink` 长期只保存 proc 实例的固定 PID view 与种类，目标从本次 `ProcLinkCaller` 选择。显示不解析显示文字来取得对象；空路径 readlink 保留已打开描述，普通 pathname 则由 FsView 保留选定路径使用权。

### 3.1 链接读取

现有完整 proc 旅程从持有链接 fd 的线程切换到兄弟线程读取，并继续执行跨进程授权变化。边界参数与未实际运行目标在各行保留缺口。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| readlinkat/held self link；x86_64 267；其余三架构 78 | v7.1 [do_readlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L524)、[self](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/proc/self.c#L10)、[thread-self](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/proc/thread_self.c#L10) | 空 pathname 读取原链接，self/thread-self 目标随本次读者变化，并投影到该 proc PID view | sys_readlinkat → get_file_like → retained File location → SelfLink.read_link → saved ProcLinkCaller 的 process/thread identity；不重新取 current | 部分正确 | x86 同一完整 C 程序先因 self 类型失败，再因 ENOTDIR 失败，最终 1/1；AArch64 同程序 1/1；RISC-V、LoongArch64 与不可见 PID 状态待补 |
| readlinkat/named target；x86_64 267；其余三架构 78 | v7.1 [do_readlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L524) | 绝对输入忽略 dirfd；末项不跟随；proc 对象读取重新授权，路径使用权保留到回调结束 | sys_readlinkat → with_fs → FsView.read_link → original LookupSession → owned PathHandle → LinkOps.read_link | 部分正确 | x86、AArch64 的 proc fd/cwd/root/exe/ns/mnt 允许、拒绝和恢复通过；其余目标、边界 fd 与完整属性语义待补 |
| readlinkat/size and non-link errors；x86_64 267；其余三架构 78 | v7.1 [do_readlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L524) | int 长度非正为 EINVAL，先于 pathname；空路径有效非链接为 ENOENT，具名非链接为 EINVAL，无效 fd 为 EBADF | sys_readlinkat 的 int 转换及空路径分支；FsView.read_link 的最终类型检查 | 无法确认 | 已核对实现顺序；本阶段完整 C 程序没有独立覆盖这些边界输入，仍需并入原功能旅程 |
| readlink；x86_64 89 | v7.1 [readlink](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L565) | 以 AT_FDCWD 解释具名路径和 int 长度 | sys_readlink → sys_readlinkat → FsView.read_link | 无法确认 | 本阶段没有 legacy readlink 的直接调用；共享实现的通过不等于该入口已实测 |

### 3.2 类型与目录枚举

self 的节点类型修正同时改变打开描述与 nofollow 元数据观察。下列入口分开记录，避免把 fstat 的运行结果当作所有 stat API 或目录枚举都已通过。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fstat/self link；x86_64 5；其余三架构 80 | v7.1 [vfs_fstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L257) | O_PATH nofollow 的 self/thread-self fd 为 symlink；显示操作不改变文件类型 | sys_fstat → resolve_fd → ResolveAtResult.stat → SimpleFile/SimpleFsNode metadata | 部分正确 | x86、AArch64 两类持有链接的 S_ISLNK 通过；RISC-V、LoongArch64 及完整大小、时间与实体寿命待补 |
| newfstatat/fstatat/self aliases；x86_64 262；其余三架构 79 | v7.1 [newfstatat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L498) | flags 决定跟随；nofollow self/thread-self 为链接，跟随为调用者对应目录 | sys_fstatat → resolve_at(saved caller) → FsView/LookupSession → ResolveAtResult.stat | 部分正确 | x86 既有完整 proc 程序的对象比较继续通过；self aliases 的全部 nofollow 与状态组合待补 |
| stat；x86_64 4 | v7.1 [newstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L472) | 跟随自指文本链接取得相应目录属性 | sys_stat → sys_fstatat → resolve_at → LookupSession | 无法确认 | 本阶段未直接运行 legacy stat 入口 |
| lstat；x86_64 6 | v7.1 [newlstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L485) | 不跟随末项，观察自指链接本身 | sys_lstat → sys_fstatat(AT_SYMLINK_NOFOLLOW) → resolve_at | 无法确认 | 本阶段未直接运行 legacy lstat 入口 |
| statx/self aliases；x86_64 332；其余三架构 291 | v7.1 [statx](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L757) | 跟随、nofollow、empty-path、mask 与同步 flags 共同决定观察 | sys_statx → resolve_at(saved caller) → LookupSession → metadata_to_kstat → statx layout | 无法确认 | 该入口的既有实现保留，本阶段没有 self/thread-self 的直接 statx 判定 |
| getdents64/proc root；x86_64 217；其余三架构 61 | v7.1 [iterate_dir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/readdir.c) | 根目录暴露 self/thread-self，实体类型和稳定遍历由实际目录接口提供 | sys_getdents64 → Directory read state → SimpleDir → ProcFsHandler.child_names | 无法确认 | 已接入名字清单；本阶段没有直接枚举及 DT_LNK 判定，不能从 open 或 fstat 外推 |

## 4. 拥有型路径消费者

`802004f30c` 将查找结果、父目录和搜索记录改为 `PathHandle`，下列入口保留实际使用权直到交给状态所有者。该迁移消除解析返回时的使用权空窗；它不自动证明旧有权限、参数、缓存和命名空间语义已经与 Linux 一致。

### 4.1 根目录与挂载

普通卸载把本次目标消费为 `PathUnmount`，只排除该操作实际持有的使用权。执行缓存与旧 MM 退休修正已由四架构定向旅程证明；第 8 节记录后续完整批次和共享实例的证据。活动消费者仍按原使用权决定 busy。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| umount2；x86_64 166；其余三架构 39 | v7.1 [path_umount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2035) | 普通卸载保留目标到拆树提交，其他实际使用者导致 EBUSY；expire、lazy detach 另有顺序和寿命 | sys_umount2 → owned target → PathHandle.prepare_unmount → cache flush/busy → PathUnmount.commit；挂载树为 namespace 状态，执行映像与映射保留实际路径 | 部分正确 | 271ba070e1 原 executable 程序四架构证明存活 busy、退出后可卸载；2b31db7844 移除头部 LRU 后完整 x86 system 541/541 仍通过；expire 与 F1-F8 授权未闭合 |
| mount；x86_64 165；其余三架构 40 | v7.1 [mount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 解析并保持来源、目标到相应 mount/bind/move 提交，按实际 namespace 解释授权 | sys_mount → FsView → owned target/source；loop Direct 与 Overlay lower/upper/work 保存原 PathHandle；MountNamespace 持拓扑 | 部分正确 | x86 私有 tmpfs 安装和跨 mount 限制通过；cgroup/devpts 与 readonly 重配置的独立语义见第 8 节；F1-F8 及完整多操作数视图继续处理 |
| pivot_root；x86_64 155；其余三架构 41 | v7.1 [pivot_root](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4759) | new_root 与 put_old 按规定顺序解析并保持到拓扑事务及 root/cwd 修订 | sys_pivot_root → one saved FsBindingSnapshot → owned directories → NamespaceChange.pivot_root → FsContextSnapshot.pivot_root | 无法确认 | 当前迁移通过类型检查；既有组件 pivot 证据不能替代本阶段直接 syscall 的并发及错误顺序证明 |
| chdir；x86_64 80；其余三架构 49 | v7.1 [chdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L550) | 跟随路径、检查目录及 search 后更新共享 fs 的 cwd，保留已取得目录 | sys_chdir → saved binding/FsView.resolve_checked → NamespaceChange.prepare_cwd(PathHandle) → commit；FsContextState 保存 cwd | 部分正确 | x86 path-lookup 的 cwd 切换、挂载穿越及恢复通过；共享 CLONE_FS、并发根切换与其余目标仍需直接证明 |
| fchdir；x86_64 81；其余三架构 50 | v7.1 [fchdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L571) | 目录 fd 的实际对象经 search 检查后成为 cwd，目录改名不改其身份 | sys_fchdir → retained Directory.path_handle.clone → NamespaceChange.prepare_cwd → commit | 无法确认 | 本阶段未新增或选择直接 fchdir 的完整旅程；拥有型迁移不能单独作为系统兼容证明 |
| chroot；x86_64 161；其余三架构 51 | v7.1 [chroot](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L588) | 路径及 search 检查后核对 CAP_SYS_CHROOT，再更新共享 fs root，cwd 独立 | sys_chroot → saved binding/FsView → owned directory → NamespaceChange.prepare_root → commit | 无法确认 | 当前所有权转交已迁移；本阶段没有直接 chroot 的完整参数、权限及共享状态红绿证明 |

### 4.2 执行映像

装载入口消费拥有型路径，保持主映像、脚本解释器及 PT_INTERP 的实际对象。每次装载的 ElfImage 读取当前数据页；执行访问租约、页缓存和 MM 映射有各自寿命，完整退出与卸载仍由真实 executable 旅程判定。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve；x86_64 59；其余三架构 221 | v7.1 [do_open_execat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L764) | 主映像与解释器保留实际对象，proc fd 路径必须经过真实挂载与链接授权，失败释放执行访问 | sys_execve → one FsView.resolve → load_user_app(PathHandle, FsView) → shebang/PT_INTERP resolve in supplied view → ExecutableFile/ElfImage/CachedFile → AddrSpace | 部分正确 | 09ccf6cb2c 的 exec family 与视图证明保留；2b31db7844 的同一写后重执行 C 与完整 x86 system 541/541 通过，写后头部失效见 14.1；CLONE_FS 受控并发和其余 ELF 边界仍有缺口；解释器格式与短读见 14.3 |
| execveat；x86_64 322；其余三架构 281 | v7.1 [do_open_execat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L764) | dirfd、AT_EMPTY_PATH、nofollow 和 AT_EXECVE_CHECK 决定对象与行为，解释器按调用者文件系统视图解析 | sys_execveat → one FsView → resolve_at_from_view or retained memfd path_handle → load_user_app borrowing original view → loader/MM → publication 外退休旧 MM | 部分正确 | 09ccf6cb2c 的原 exec family 程序直接覆盖 dirfd、空路径、nofollow、无效 fd 和参数预算，x86 完整批次通过；AT_EXECVE_CHECK 未实现，全部 fd 类型与错误优先级尚未证明 |


## 5. 类型化末组件

`190c9a2d5f` 的父解析输出为 `ParentLookup`；`cf78f01c61` 包含导入及重新导出的完整迁移。原查找的点和点点在父出口保持未消费，普通 lookup/open 在原 Continuation 内继续遍历。命名操作据此决定错误，既有后端仍在实际槽位重新授权。

### 5.1 命名入口

每个入口单独保留调用链和运行范围。原完整权限程序已从错误版本失败变为四架构通过，18 包标准库、迁移版四目标 Clippy 及后续 AX 定向 Clippy 均通过。共享 FS 并发观察不能由函数签名或单线程通过外推；MemoryFs 的 EXCHANGE/WHITEOUT 能力仍未提供。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| mkdir；x86_64 83 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 路径父遍历成功后，根、点和点点均为 EEXIST；正常目录允许尾斜杠 | sys_mkdir → sys_mkdirat → FsView.create_dir → ParentLookup → native create | 部分正确 | 原目录权限程序包含创建存在性、dirfd 与 DAC 判定 |
| mkdirat；x86_64 258；其余三架构 34 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 绝对路径忽略 dirfd；特殊末项不转为其物理目录名称，正常创建在真实父槽位授权 | sys_mkdirat → with_fs → FsView.create_dir → typed component → Location.create | 部分正确 | 原完整目录权限程序直接覆盖 mkdirat；跨目标结果在本阶段终态记录 |
| rmdir；x86_64 84 | v7.1 [filename_rmdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L5392) | 父遍历后，点为 EINVAL、点点为 ENOTEMPTY、根为 EBUSY；最终链接不作为目录目标跟随 | sys_rmdir → sys_unlinkat(AT_REMOVEDIR) → FsView.remove_dir → ParentLookup | 部分正确 | 原完整权限程序覆盖点、点点和目录删除；本轮 LoongArch 增强断言与 LTP rmdir02 通过，父 EACCES 先于 ENOTDIR，挂载点仍 EBUSY |
| unlink；x86_64 87 | v7.1 [filename_unlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L5534) | 特殊末项为 EISDIR；带尾斜杠的非目录为 ENOTDIR；最终链接自身参与删除 | sys_unlink → sys_unlinkat → FsView.remove_file → original parent slot | 部分正确 | 增强权限程序先在错误 EISDIR 失败，77d42086c9 证明父 EACCES 优先，同时保留尾斜杠 EISDIR/ENOTDIR；完整并发范围待补 |
| unlinkat；x86_64 263；其余三架构 35 | v7.1 [filename_unlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L5534) | flags 决定 unlink 或 rmdir；父目录和末项保持原意图，实际提交仍重验当前名称对象 | sys_unlinkat → with_fs → remove_file/remove_dir → ParentLookup → namespace command | 部分正确 | 3dd3c3e138 同一增强程序失败；77d42086c9 的权限及 cgroup 完整程序通过，实际命名槽在父 DAC 后检查类型和当前 namespace 挂载点；完整并发范围待补 |
| rename；x86_64 82 | v7.1 [do_renameat2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L6110) | 按普通替换解释两端；特殊末项保留 EBUSY，mount 身份检查先于末项拒绝 | sys_rename → sys_renameat → sys_renameat2 → one FsView → rename_resolved | 部分正确 | 原完整 ownership 程序包含替换、同对象和跨 mount 行为 |
| renameat；x86_64 264；AArch64/LoongArch64 38；RISC-V 无此入口 | v7.1 [do_renameat2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L6110) | 两端 dirfd 使用同一原始 root/cwd；绝对路径各自忽略 dirfd | sys_renameat → sys_renameat2 → view_at for each operand → ParentLookup | 部分正确 | 原权限程序及路径程序包含 renameat/renameat2；RISC-V 由 renameat2 承担 |
| renameat2；x86_64 316；其余三架构 276 | v7.1 [do_renameat2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L6110) | 未知或冲突 flags 为 EINVAL；不同 mount 为 EXDEV；源特殊项 EBUSY，NOREPLACE 的特殊目标 EEXIST | sys_renameat2 → one saved FsView → two ParentLookup → rename_resolved → native attempt | 部分正确 | 原权限、ownership 和路径程序覆盖 flags、存在性、改名后身份；并发共享 FS 的 syscall 证明仍待补 |
| link；x86_64 86 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 按不跟随源末链接的 linkat 语义取得源；目标特殊末项为 EEXIST | sys_link → sys_linkat → resolve_at_with_boundary_checked → link_resolved | 部分正确 | 原权限程序覆盖普通 link；不由共享 helper 外推所有 legacy 输入 |
| linkat；x86_64 265；其余三架构 37 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 源 empty-path/跟随 flags 保留；目标点直接 EEXIST，不重走打开目录的物理父亲 | sys_linkat → one FsView → owned source + typed destination → link_resolved | 部分正确 | 旧 272141b52a 的完整权限程序因点目标错误 EACCES 失败；cf78f01c61 同一程序在四架构通过；共享 FS 的直接并发交错仍待补 |
| symlink；x86_64 88 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 目标文字不参与当前查找；新链接名称的特殊末项 EEXIST，尾斜杠不创建非目录 | sys_symlink → sys_symlinkat → FsView.symlink → ParentLookup → create_symlink | 部分正确 | 原权限程序包含 symlink 与只读目录拒绝；尾斜杠的直接系统输入仍需补齐 |
| symlinkat；x86_64 266；其余三架构 36 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 按新名称的 dirfd 解析父目录，保存文字并授权实际创建槽位 | sys_symlinkat → with_fs → FsView.symlink → typed creation_name → MemoryFs FileContent 的实际不可变文本 | 部分正确 | 原完整权限程序覆盖 symlinkat；c854aa5f7f 的同一 Linux/x86 程序通过原始 SYS_symlinkat 创建目标后分别普通和缓存跟随，内容身份与 40/41 预算通过；完整错误优先级仍未穷尽 |
| mknod；x86_64 133 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 类型和设备能力检查保留，创建名称中的特殊末项不能替换已有目录 | sys_mknod → sys_mknodat → FsView.create_node → ParentLookup → create_initialized | 无法确认 | AX 既有设备创建功能旅程通过；不能替代该 legacy syscall 的直接运行 |
| mknodat；x86_64 259；其余三架构 33 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 按 dirfd 保留父目录和末项，真实 owner 决定能力、权限与创建结果 | sys_mknodat → with_fs → FsView.create_node → typed creation_name → actual slot | 部分正确 | 原权限程序覆盖设备能力及存在性；完整类型、尾斜杠和其他边界继续核对 |

### 5.2 视图边界

`view_at` 只从入口已保存的 FsView 选择各个 dirfd，link/rename 解析两端及最后提交不再次调用 current_fs_context 或 snapshot_fs_binding。空路径源继续保留实际打开描述；fd 表中对象按真实观察点取得。当前记录只确认调用链已经共用视图，跨线程修改 CLONE_FS 的受控直接系统证明仍需补齐。


## 6. 临时文件系统装配

启动期的临时目录权限决定普通用户是否能在真实父目录创建对象。`mount_temporary`
通过初始化接口设置两个独立 tmpfs 根为 01777，保留运行期 DAC、umask 和创建者身份。

### 6.1 路径套接字

原完整程序在旧实现的 /tmp 权限处失败，修后实际核对 socket inode 及创建者。
当前证据仅覆盖 x86_64 的此条创建路径，其他 flags、错误顺序和目标继续核对。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| bind(AF_UNIX pathname)；x86_64 49；其余三架构 200 | v7.1 [unix_bind_bsd](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/unix/af_unix.c#L1350) | 创建要求父目录 write/search；socket 由当前 fsuid/fsgid 和 umask 初始化；Linux 的 /tmp 模式由系统装配决定 | mount_all → mount_temporary → explicit initialization；sys_bind → saved FS/caller/umask → UnixSocket.bind_path_with → create_node initialized in actual parent | 部分正确 | 同一 unix-bind-chown C 程序在 5111b45358 失败，在 04d7860504 x86_64 为 1/1、外层 0；Linux 6.8 私有 tmpfs 对照 7/0；其余目标与完整 AF_UNIX flags/errors 未验证 |

### 6.2 验证边界

/dev/shm 与 /tmp 共用同一初始化规则，但本轮未单独证明共享内存的完整 syscall
旅程。04d7860504 的两包标准库和 axbuild/Kernel 73 项 Clippy 均通过，完整
x86_64 system 为 536/541、仍有 5 个失败；静态与部分运行结果不代表整体兼容。


## 7. 进程映射的退出发布

`complete_mm_exit` 将无用户、无内核 pin 的 MM 内容退休放在 zombie 发布之前，
活动进程或外部内核访问继续持有其原资源。同步点在 process publication 锁外，
复用缓存失效已有的 reclaim_done 与 Freed/NeedsRepair 状态。

### 7.1 退出与等待

原 executable 功能旅程用管道控制子进程存活，检查挂载占用、退出、等待和卸载。
这证明本次映射寿命的可观察边界，不替代各入口其余 flags、rusage 和共享语义。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| exit；x86_64 60；其余三架构 93 | v7.1 [do_exit/exit_mm](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 当前线程退出；最后 MM 用户结束映射寿命，其他共享用户继续运行 | sys_exit → do_exit(false) → last thread → complete_mm_exit → release user/ref → synchronize_retirement → publish_zombie | 无法确认 | 原旅程经 libc 结束子进程，不能单独证明 raw exit 的线程及共享 MM 路径；真实内核批次通过 |
| exit_group；x86_64 231；其余三架构 94 | v7.1 [do_group_exit](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) 与 [__mmput](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c) | 线程组退出完成 MM 使用者清理后发布可等待状态；实际活动用户仍保留引用 | sys_exit_group → do_exit(true) → last-thread MM release → no-pin completion → zombie publication | 部分正确 | 5082dc693f 原完整程序失败；271ba070e1 同一文件四架构通过，含存活 busy 和 waitpid 后可卸载；完整 CLONE_VM 与外部 pin 系统交错仍待证明 |
| wait4；x86_64 61；其余三架构 260 | v7.1 [kernel_wait4](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 观察已发布子进程状态；正常退出的 MM 映射不应仅因后台延迟继续占用挂载 | Sysno::wait4 → sys_waitpid → zombie candidate/reap；生产者 do_exit 在 publish_zombie 前完成无 pin MM 内容 | 部分正确 | 原 executable 程序四架构的 waitpid、状态及随后卸载均通过；本阶段未证明全部 options、rusage 和并发父子语义 |
| waitid；x86_64 247；其余三架构 95 | v7.1 [kernel_waitid](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/exit.c) | 按 idtype/options 观察或保留退出事件；MM 清理通过同一 zombie 发布边界可见 | sys_waitid → wait candidate/reap；退出生产者 complete_mm_exit → publish_zombie | 无法确认 | 本轮定向 executable 旅程不直接调用 waitid；不能从 wait4 结果推断其全部选择与输出语义 |

### 7.2 回收边界

仍有内核 pin 的 MM 不进入同步等待，完成后由原 worker 回收；NeedsRepair 保留
实际资源并报告问题。内核测试、四架构定向程序与静态检查的精确源码均为
271ba070e1；77d42086c9 的 LoongArch 完整 system 见第 8 节。ELF 写后缓存一致性、性能和独立审查尚未闭合。

## 8. 文件系统实例

`77d42086c9` 将同一 cgroup 层级与独立 devpts 挂载分别连接到实际 Filesystem
所有者；`st_dev` 由该实例的共享挂载状态提供。下表单独记录实例、属性和伪终端
路径，不以原先 self-link 或普通挂载的结果替代这些语义。

### 8.1 入口与对象

LoongArch64 的完整 system 为 538/538，包含 cgroup-basic、cgroup-ns、devpts、
目录权限、readonly remount 与 LTP rmdir02。Linux 6.8 提供同条件可观察对照，
固定 v7.1 源码决定实例边界和错误顺序。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| mount/cgroup2；x86_64 165；其余三架构 40 | v7.1 [cgroup_do_get_tree](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/cgroup/cgroup.c#L2219) | 同一层级共享 superblock，cgroup namespace 选择该层级中的根目录 | sys_mount → cgroup namespace root/pin → CGROUP2_FILESYSTEM → select_mount_root → mount_root_with_source；Fs state 全局属层级，root 属挂载 | 部分正确 | 2d98a3119d 跨 namespace device 断言先失败；77d42086c9 两个完整 cgroup 程序通过；Linux 不同 namespace 共享 device；完整授权及并发发布仍待补 |
| mount/devpts；x86_64 165；其余三架构 40 | v7.1 [devpts_get_tree](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/devpts/inode.c#L399) | 每次挂载创建私有实例，历史 newinstance flag 不改变该选择 | sys_mount → parse_devpts_options → new_devptsfs(DevPtsOptions) → fresh SimpleFs/PtsInstance；启动 devfs 的实例只在自身共享 | 部分正确 | 628a87d496 的默认实例隔离断言失败；77d42086c9 完整 devpts 程序通过；Linux 默认挂载均从 0 分配且 device 独立；创建者 fsuid 和完整 remount 未闭合 |
| mount/MS_REMOUNT readonly；x86_64 165；其余三架构 40 | v7.1 [mount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 真实活动写入者可使只读转换 EBUSY；单个挂载与共享文件系统保护分开 | sys_mount → Mountpoint.set_filesystem_readonly 或 set_readonly → FilesystemMountState/本挂载 writer 状态 | 部分正确 | 原捕获运行器在 Linux 和 LoongArch 均因自己的 /tmp 输出写入者失败；匿名捕获下同一 remount 程序通过；不据此推断所有重配置 flag 与并发行为 |
| fsopen；四架构 430 | v7.1 [fsopen](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fsopen.c#L120) | 创建调用者授权的 fs_context，能力与 flags 按入口顺序检查 | sys_fsopen → MountContextKind → MountContext/FD table | 部分正确 | 本轮直接 mount 旅程不证明完整 fsopen；当前 flags 检查早于 capability，固定 Linux 顺序相反，授权研究范围保留 |
| fsconfig/devpts create；四架构 431 | v7.1 [fsconfig](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fsopen.c#L350) | 参数属于指定 context，创建或重配置消费该 context 的阶段状态 | sys_fsconfig → MountContextState.devpts_options → new_devptsfs → state.filesystem | 无法确认 | 已同步私有实例构造调用；缺少同 context 多次创建、错误阶段及重配置的完整定向判据 |
| fsmount；四架构 432 | v7.1 [fsmount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4431) | 从已创建 context 构造独立挂载，同一实例保留相同 superblock 身份 | sys_fsmount → context.filesystem.clone → Mountpoint.new_root_with_source → detached Directory fd；共享 Fs state、独立 mount_id | 部分正确 | 构造使用同一默认 device；多 fsmount 的直接系统身份判据待补；当前不接受 Linux 的 FSMOUNT_NAMESPACE 等完整 flag 范围 |
| open/devpts；x86_64 2 | v7.1 [ptmx_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/tty/pty.c#L767) | 打开目标实例的 ptmx/slave；独立实例中的相同编号不指向同一终端 | sys_open → sys_openat → FsView/OpenOptions → Ptmx/PtsDir → PtsInstance | 无法确认 | libc 的 open 结果不能单独证明 x86 legacy open 入口；本轮主要运行于 LoongArch64 |
| openat/devpts；x86_64 257；其余三架构 56 | v7.1 [ptmx_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/tty/pty.c#L767) | 已解析的实际 devpts 实例决定分配表、slave 可见性和打开目标 | sys_openat → owned path → Device/Ptmx → PtyLink/PtsInstance；每个新挂载独立 | 部分正确 | 完整 devpts 程序验证重复默认挂载、相同编号隔离、slave 打开及控制终端；完整权限、锁状态与 flags 仍待核对 |
| ioctl/PTY；x86_64 16；其余三架构 29 | v7.1 [pty_unix98_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/tty/pty.c#L625) | PTY 编号与锁作用于实际终端对象；控制终端和前台组使用会话状态 | sys_ioctl → retained FileLike → Device/Tty/Ptmx → PtsInstance 与会话 terminal 状态 | 部分正确 | devpts 程序实际执行 TIOCGPTN、TIOCSPTLCK、TIOCSCTTY、TIOCGPGRP；未将成功接受解锁命令解释为完整锁状态实现 |
| read/PTY；x86_64 0；其余三架构 63 | v7.1 [PTY driver](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/tty/pty.c) | master 从其实际 slave 端接收数据，打开描述保持终端寿命 | sys_read → retained FileLike.read → Tty/PtyReader → 该 PtyLink 的共享通道 | 部分正确 | 控制终端写入后，父进程 poll/read master 实际得到 T；跨实例双向并发与完整短读语义未由此证明 |
| write/PTY；x86_64 1；其余三架构 64 | v7.1 [PTY driver](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/tty/pty.c) | 对所选终端写入的数据到达配对端，返回实际完成量 | sys_write → write access/length checks → retained FileLike.write → Tty/PtyWriter | 部分正确 | 同一完整 devpts 旅程验证 /dev/tty 写入与 master 收到的字节；完整错误、阻塞和跨实例竞争仍待补 |
| getdents64/devpts；x86_64 217；其余三架构 61 | v7.1 [devpts instance](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/devpts/inode.c) | 枚举该挂载实例的 slave 名称，不能混入另一实例的分配表 | sys_getdents64 → Directory open state → PtsDir/PtsInstance | 无法确认 | 新旅程通过 stat/open 核对可见性，尚无直接 getdents64 的跨实例枚举判据 |
| stat/实例身份；x86_64 4 | v7.1 [newstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L472) | st_dev 属于实际文件系统实例，命名空间根选择不拆分 cgroup 层级实例 | sys_stat → sys_fstatat → location.metadata → mountpoint.device → shared FilesystemMountState | 无法确认 | 本轮 LoongArch 程序没有 x86 legacy stat 入口的直接证据 |
| newfstatat/fstatat/实例身份；x86_64 262；其余三架构 79 | v7.1 [newfstatat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L498) | cgroup 别名、跨 namespace 根共享 st_dev；不同 devpts 实例具有独立 st_dev | sys_fstatat → resolve_at → PathHandle.location.metadata → mount device | 部分正确 | cgroup-basic、cgroup-ns 和 devpts 经 libc 核对 device/inode 及父目录关系；各原始 stat 入口与 flags 的完整覆盖仍待确认 |
| fstat/实例身份；x86_64 5；其余三架构 80 | v7.1 [vfs_fstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L257) | 已打开描述观察原实例的属性，路径别名不改变底层文件系统身份 | sys_fstat → resolve_fd → retained file/directory path → metadata_to_kstat | 部分正确 | cgroup-basic 比较目录、控制文件 fd 与重复挂载路径的实际 device/inode；其他 fd 类和完整属性仍待补 |
| statx/实例身份；x86_64 332；其余三架构 291 | v7.1 [statx](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L757) | dev major/minor 来自同一实际实例，mount 身份与 device 身份独立 | sys_statx → resolve_at → metadata_to_kstat → statx layout | 无法确认 | 本阶段没有对新实例 device 字段和 mount_id 的直接 statx 判据 |

### 8.2 证据边界

该版 18 包标准库与 VFS/AX/Kernel 81 项 Clippy 全部通过。LoongArch64 使用
匿名捕获完成 538 个程序；原生成功断言输出按既有策略抑制，程序与最外层汇总
均为零失败。后续发布 b6755f03ec 的四架构 CI 均已通过：x86 为 541/541，
RISC-V 与 LoongArch 为 538/538，AArch64 分组为 513、23、1、1 项，均零失败。
该提交尚不包含 09ccf6cb2c 的执行链视图改动，不能当作后者的运行结果。
Linux devpts 对照因宿主单 GID 映射使用 gid=0，Starry 仍验证
gid=5；Linux 的四个 unlink 输入以移除 capability 形成拒绝写入条件，不能替代
完整 root 到 uid/gid 1000 的凭据转换证明。


## 9. 描述路径打开

原 procfd 旅程使用同一 C 核对绝对路径、相对目录、普通别名、原描述与新描述，覆盖重命名、删除和 fd 槽复用。18ef129e76 的旧捷径与 332b0512d2 的不完整通用分派均失败，d788254e65 的 x86 完整 541/541、18 包标准库和 78 项 Clippy 通过。

### 9.1 对象与打开标志

打开由保存的视图完成，再将实际对象交给描述构造；原有 opened path 不因另一个 mount 遮住 procfs 而失效，绝对路径也不能绕过该遮盖。文件系统查询的剩余消费者单独保留结论。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat/procfd 与管道；x86_64 257；其余三架构 56 | v7.1 [fifo_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L1121) | 绝对描述路径遵守实际挂载，保留目录 fd 可继续访问旧 proc 对象；重开允许新访问模式并遵守 nofollow、exclusive 与 directory | `sys_openat` → one `FsView`/`OpenOptions` → proc Object link → `add_to_fd` → `Pipe::open_fifo` → `ObjectNode<Arc<Shared>>` | 部分正确 | d788254e65 的原 procfd 完整程序通过；18ef129e76 的同一 C 错误绕过挂载与 flags，跨模式 EACCES；其他目标等待本版 CI |
| openat2/procfd；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1190) | how 先严格校验，再对同一对象执行普通或受限解析，不能由描述字符串前缀改变行为 | `sys_openat2` → flag/resolve 校验 → `sys_openat` 或 `FsView` policy → `add_to_fd` with saved view → `Pipe::open_fifo` | 部分正确 | 同一完整程序对 openat/openat2 分别验证；x86 541/541，CACHED、完整 how 边界及其他目标仍有缺口 |
| fstat/重开管道；x86_64 5；其余三架构 80 | v7.1 [vfs_fstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L257) | 重开改变描述状态，不改变同一管道 inode、device 和类型 | sys_fstat → resolve_fd → `backing_file`/`PathHandle` 或 Pipe.stat → shared ObjectInode | 部分正确 | 原 procfd 程序比较两端、不同访问模式和别名重开的 device/inode/FIFO 类型；完整属性及其他目标仍待证明 |
| fstatfs/原生伪对象；x86_64 138；其余三架构 44 | v7.1 [fd_statfs](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/statfs.c#L116) | 按保留描述的实际 file path 返回 pipefs、sockfs、nsfs 信息，关闭原 fd 不使副本失去该来源 | sys_fstatfs → get_file_like → descriptor_path → owned PathHandle → actual SimpleFs.stat → user output | 部分正确 | b5b8baf659 的同一程序中原始 pipe/socket/namespace fd EINVAL；a960991962 在 x86 67/0，Linux 同一 C 67/0；3dfc6f4a42 的相同实现和 C 已在四架构完整 CI 实际执行，其他匿名描述的路径能力仍待补 |

### 9.2 描述与端点

`PipeAccess`、状态标志和读写端点计数属于独立打开描述与原 Shared 通道。命名 FIFO 的伙伴等待和首次 HUP 抑制保留；匿名管道按实际 ObjectNode payload 选择打开行为。以下入口按自身的调用链和已有直接证据分别记录。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| read/重开管道；x86_64 0；其余三架构 63 | v7.1 [anon_pipe_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L269) | 新读描述访问原缓冲，关闭原两端不使仍存活的重开描述失效 | sys_read → get_file_like → Pipe.read → Shared.state 中的 `PipeState`.buffer/`PipeAccess` | 部分正确 | d788254e65 的 x86 原程序先读保留的 B，再通过新写描述读 P；其他目标等待本版运行 |
| readv/重开管道；x86_64 19；其余三架构 65 | v7.1 [anon_pipe_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L269) | 向量读取使用同一打开描述与管道缓冲，访问模式按本次打开决定 | sys_readv → IoVectorBuf → Pipe.read → Shared | 无法确认 | 未直接对本版重开描述运行 readv 的完整向量与错误旅程 |
| write/重开管道；x86_64 1；其余三架构 64 | v7.1 [anon_pipe_write](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L431) | 新写描述写入原管道缓冲，端点与访问模式独立于原 fd 的模式 | sys_write → validate_write_access/IoSrc → Pipe.write → Shared.state/`PipeState`.buffer | 部分正确 | x86 同一程序从原读端重新打开 writer，并与从原写端重开的 reader 交换数据；其他目标待证明 |
| writev/重开管道；x86_64 20；其余三架构 66 | v7.1 [anon_pipe_write](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L431) | 向量写使用同一新描述与原缓冲，按管道写入规则处理部分完成 | sys_writev → IoVectorBuf → Pipe.write → Shared | 无法确认 | 本阶段未对本版重开描述直接验证 writev 的向量、部分完成与错误顺序 |
| close/重开管道；x86_64 3；其余三架构 57 | v7.1 [pipe_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L725) | 最后相应端点关闭后发布对端 HUP 或 ERR，其他独立描述继续保持原通道 | sys_close → close_file_like → FileDescriptor drop → Pipe.drop → shared endpoint counts/notifications | 部分正确 | x86 原程序关闭原两端后继续读写，并在最后 writer/reader 关闭后检查 readiness；其他目标待证明 |
| fcntl/重开状态；x86_64 72；其余三架构 25 | v7.1 [fifo_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L1121) | 独立重开有自己的访问与状态标志，F_SETFL 不改变原打开描述 | sys_fcntl → FileLike.open_flags/set_nonblocking → `PipeAccess`/AtomicBool；dup 仍共享同一描述 | 部分正确 | x86 原程序检查每种访问模式、CLOEXEC 及移除新描述 NONBLOCK 后原描述仍 NONBLOCK；锁与其他命令仍待补 |
| poll/重开管道；x86_64 7 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 匿名读描述在无 writer 时立即 HUP，匿名写描述无 reader 时可重开并观察 ERR | sys_poll → do_poll → Pipe.poll → Shared.readiness；匿名打开不设置 named FIFO HUP suppression | 正确 | 332b0512d2 的同一 C 两个 readiness 判据失败；d788254e65 的 x86 完整批次通过，Linux 相同完整程序通过 |
| ppoll/重开管道；x86_64 271；其余三架构 73 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 管道 readiness 相同，timespec、sigmask 及返回时状态按 ppoll 契约处理 | sys_ppoll → timespec/sigmask 校验 → do_poll → Pipe.poll | 无法确认 | 本版 x86 C 的 poll 判据不能替代直接 ppoll 参数、信号和其他架构 ABI 证明 |
| select/重开管道；x86_64 23 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 无 writer 的空管道可读为 EOF，无 reader 的写端就绪反映错误状态 | sys_select → do_select → FileLike/Pipe.poll → Shared.readiness → fd sets | 无法确认 | 本阶段未对新重开描述执行直接 select 的完整 fd set 判据 |
| pselect6/重开管道；x86_64 270；其余三架构 72 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 在 pselect6 的时间与信号约束下投影管道 readiness | sys_pselect6 → do_select → sigmask/timeout → Pipe.poll → fd sets | 无法确认 | 未对新描述直接验证 pselect6 的信号、ABI 和 readiness 组合 |
| epoll_wait/重开管道；x86_64 232 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 已登记的管道从真实端点状态报告 HUP 与 ERR | sys_epoll_wait → do_epoll_wait → Epoll → Pipe.poll/register_shared | 无法确认 | 本阶段没有新描述的直接 epoll_wait 登记、端点关闭与回放旅程 |
| epoll_pwait/重开管道；x86_64 281；其余三架构 22 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 在临时信号掩码下等待同一管道端点事件 | sys_epoll_pwait → do_epoll_wait → Epoll → Pipe.poll/register_shared | 无法确认 | 未直接对本版重开描述验证 epoll_pwait 的信号及事件边界 |
| epoll_pwait2/重开管道；四架构 441 | v7.1 [pipe_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c#L660) | 按 timespec 超时与临时信号掩码等待管道状态 | sys_epoll_pwait2 → timespec → do_epoll_wait → Epoll → Pipe.poll/register_shared | 无法确认 | 未直接对新描述执行 epoll_pwait2 的精度、信号及事件旅程 |


## 10. 文件系统统计

原 statfs 旅程使用真实原生描述、proc 对象链接和 Overlay 层，区分文件系统统计的来源与文件本身的字节长度。固定 Linux 源码决定统计字段与层选择，Linux 6.8 同一 C 提供运行对照。

### 10.1 原生路径

`fstatfs` 使用已经选定描述的 `PathHandle`，`statfs` 经实际挂载与链接取得同一来源。SimpleFs 的零值由 Linux 伪文件系统契约规定，磁盘后端的容量继续来自其实际实现。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| statfs/伪对象几何信息；x86_64 137；其余三架构 43 | v7.1 [simple_statfs](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/libfs.c#L43) | 原生伪文件系统使用页大小和名称上限，磁盘容量与文件计数为零；proc 对象链接指向实际原生 FS | sys_statfs → FsView.resolve/proc Object link → selected mount filesystem → SimpleFs.stat | 部分正确 | 旧版页几何与零容量判据失败；a960991962 的原 pipe/socket/namespace 程序及 Linux 同一 C 通过；3dfc6f4a42 的四架构 CI 已执行同一 C，其他伪文件系统属性继续核对 |
| fstatfs/memfd；x86_64 138；其余三架构 44 | v7.1 [fd_statfs](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/statfs.c#L116) | memfd 查询其 tmpfs 文件系统，proc 路径与副本观察同一 fsid 和几何信息 | sys_fstatfs → descriptor_path → Memfd.inner PathHandle → MemoryFs.stat | 部分正确 | 原完整 C 验证 type、fd/path fsid 和关闭原 fd 后副本，x86 通过；3dfc6f4a42 的四架构 CI 已执行；完整容量记账尚未证明 |

### 10.2 分层来源

`OverlayFs` 保留装配时的层路径，每次重新查询所选 backing 的当前统计，构造前保存层的最大名称上限。容量证明使用不同几何信息，原有 fsid 表达和其他未验证字段继续保留结论边界。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| statfs/Overlay；x86_64 137；其余三架构 43 | v7.1 [ovl_statfs](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/overlayfs/super.c#L276) | 有 upper 时使用实际 upper 容量，否则使用首个 lower；保留 Overlay 类型和层名称上限 | sys_statfs → selected logical mount → OverlayFs.stat → retained upper/first lower filesystem.stat；logical mount flags | 部分正确 | b5b8baf659 的两个来源判据均失败；a960991962 同一 C 在 x86 与 Linux 通过，upper/lower 几何信息可区分；3dfc6f4a42 的四架构 CI 已执行；fsid/UUID 与全部统计字段仍未闭合 |
| fstatfs/Overlay；x86_64 138；其余三架构 44 | v7.1 [ovl_statfs](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/overlayfs/super.c#L276) | 打开描述按其实际 Overlay filesystem 查询，名称变化不改该统计来源 | sys_fstatfs → descriptor_path → retained logical path → OverlayFs.stat | 无法确认 | 本阶段直接 Overlay 判据使用 statfs；未单独覆盖 Overlay fd 的改名、卸载和统计寿命 |
| mount/Overlay 名称能力；x86_64 165；其余三架构 40 | v7.1 [ovl_check_namelen](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/overlayfs/super.c#L397) | 构造时核对层统计，名称上限取层最大值，统计失败阻止发布 | sys_mount → resolved layer PathHandles → new_overlayfs layer stat/maximum → Filesystem construction → mount publication | 部分正确 | 同一 C 的 upper-backed 与双 lower-only 装配和结果通过；本轮没有底层 stat EIO 的受控失败及不同名称上限的独立运行证明 |

## 11. 显示序号观察

本轮持续集成跟进修正 DRM 系统测试对既有序号行为的判定，并保留共享核心的
真实 off/on/get 调用。主机驱动时间戳夹具、实际 C 判据和 Starry 运行分别证明
计数规则、错误判定与 ioctl 旅程，不能将其中任一层扩写为全部硬件时序支持。

### 11.1 模式切换

`DrmVblankCrtc` 在重新启用时建立新时间锚，显式增加一次序号，关闭期间不由
新纪元补算。系统测试使用实际关闭事件保留的序号作为起点。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl/MODE_SETCRTC 重新启用；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_vblank_on_config](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1482) | 模式切换重设时间锚并增加一个序号；关闭到启用之间的时间不纳入新纪元计数 | sys_ioctl → retained Card0File.ioctl → drm_ioctl → drm_mode_setcrtc → atomic simple CRTC enable → drm_crtc_vblank_on_config → DrmVblankCrtc count/time | 部分正确 | 9b8e29fd84 的真实核心夹具关闭 100 个周期后验证 reset 增量 1；a8cc3944a1 的 LoongArch 原完整 DRM 程序 1/1；其余目标和真实 Linux DRM 运行对照未在本阶段闭合 |

### 11.2 首次查询

`CRTC_GET_SEQUENCE` 可以为没有引用的活动 CRTC 启用计时器，返回一致的序号与
边沿时间。无硬件计数器时，补算活动帧数采用最近整数取整。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl/CRTC_GET_SEQUENCE 首次活动查询；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_get_sequence_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L2006) 与 [drm_update_vblank_count](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L339) | 按实际 CRTC 取得引用并读取一致的 count/time；时间戳推导帧数四舍五入，不能用向下取整的耗时上界拒绝合法结果 | sys_ioctl → Card0File.ioctl → drm_ioctl → drm_crtc_get_sequence_ioctl → vblank_get/enable → drm_update_vblank_count → DrmVblankCrtc seqlock | 部分正确 | 原引用旅程精确断言 3/4 周期的活动时间得到总增量 2 及对应时间戳；实际 C 判据同一回放在 8d412dbfe7 失败、a8cc3944a1 通过，修后仍拒绝增量 102 和序号回退；LoongArch ioctl 程序 1/1，其他目标和真实 Linux DRM 运行对照仍有缺口 |

## 12. 伪文件系统实体

`SimpleInode` 保留普通实体属性，活跃包装绑定原 `SimpleFsOwner`，
Cgroup 保留组件自己的属性 owner。Native 生命周期证明与 syscall 兼容证明
分别记录；包装退休不自动证明每个 ABI 的参数、权限和字段策略。

### 12.1 共享属性

mqueue 的路径配方、mqd stat 和后续权限检查共享队列实体。两个既有 C 旅程
及内核生命周期用例已取得对应红绿；未直接执行的入口保留证据缺口。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| chmod/静态伪实体；x86_64 90 | v7.1 [chmod_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L618) | 修改实际 inode 的权限 | sys_chmod → sys_fchmodat → saved FsView → SimpleFsNode/SimpleInode | 部分正确 | devpts 同一 C 旅程在旧实现丢失 ptmx 权限、修后与 Linux 通过；C 库 chmod 的实际原始入口未单独跟踪 |
| fchmod/mqd；x86_64 91；其余三架构 52 | v7.1 [chmod_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L618) | 描述与名字共享 inode 属性 | sys_fchmod → descriptor_path(MqDescriptor) → mqueue_location → SimpleFsNode.change_metadata → queue SimpleInode | 部分正确 | mqueue 程序直接 SYS_fchmod；旧实现失败，修后 x86 与 Linux 通过；其余目标及完整权限错误仍待最终 CI/专门证据 |
| fchmodat/mqueue 路径；x86_64 268；其余三架构 53 | v7.1 [chmod_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L618) | 路径修改对已有描述可见 | sys_fchmodat → change_at → retained path → SimpleFsNode.change_metadata → queue SimpleInode | 部分正确 | 同一 C 直接 SYS_fchmodat 后 fstat(mqd) 核对权限；旧实现失败、修后 x86 与 Linux 通过 |
| fchmodat2/伪实体；四架构 452 | v7.1 [chmod_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L618) | flags 参与目标选择与授权 | syscall/mod.rs → sys_fchmodat(flags) → owned target → MetadataRequest → SimpleInode | 部分正确 | 共享实体已迁移；未在本单元直接覆盖 AT_EMPTY_PATH/AT_SYMLINK_NOFOLLOW 全部组合 |
| chown/伪实体；x86_64 92 | v7.1 [chown_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 属主更新作用于原实体，权限由实际凭据决定 | sys_chown → sys_fchownat → change_at → MetadataRequest → actual receiver → SimpleInode 属性锁 | 无法确认 | Native 旅程证明属主在包装重建后保留；本单元未直接证明这些 syscall 的参数、userns、错误顺序与 charge 退还旅程 |
| lchown/伪实体；x86_64 94 | v7.1 [chown_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 属主更新作用于原实体，权限由实际凭据决定 | sys_lchown → sys_fchownat(AT_SYMLINK_NOFOLLOW) → change_at → MetadataRequest → actual receiver → SimpleInode 属性锁 | 无法确认 | Native 旅程证明属主在包装重建后保留；本单元未直接证明这些 syscall 的参数、userns、错误顺序与 charge 退还旅程 |
| fchown/伪实体；x86_64 93；其余三架构 55 | v7.1 [chown_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 属主更新作用于原实体，权限由实际凭据决定 | sys_fchown → change_fd → descriptor_path → MetadataRequest → actual receiver → SimpleInode 属性锁 | 无法确认 | Native 旅程证明属主在包装重建后保留；本单元未直接证明这些 syscall 的参数、userns、错误顺序与 charge 退还旅程 |
| fchownat/伪实体；x86_64 260；其余三架构 54 | v7.1 [chown_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 属主更新作用于原实体，权限由实际凭据决定 | sys_fchownat → change_at → MetadataRequest → actual receiver → SimpleInode 属性锁 | 无法确认 | Native 旅程证明属主在包装重建后保留；本单元未直接证明这些 syscall 的参数、userns、错误顺序与 charge 退还旅程 |
| utime/伪实体；x86_64 132 | v7.1 [do_utimes](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/utimes.c) | 所选 inode 保留提交的时间字段 | sys_utime → sys_utimensat → MetadataRequest → shared SimpleInode | 无法确认 | 属性 owner 已共享，时间范围、精度、权限及 proc 刷新策略未由本单元证明 |
| utimes/伪实体；x86_64 235 | v7.1 [do_utimes](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/utimes.c) | 所选 inode 保留提交的时间字段 | sys_utimes → sys_utimensat → MetadataRequest → shared SimpleInode | 无法确认 | 属性 owner 已共享，时间范围、精度、权限及 proc 刷新策略未由本单元证明 |
| utimensat/伪实体；x86_64 280；其余三架构 88 | v7.1 [do_utimes](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/utimes.c) | 所选 inode 保留提交的时间字段 | sys_utimensat → retained fd/path → MetadataRequest → shared SimpleInode | 无法确认 | 属性 owner 已共享，时间范围、精度、权限及 proc 刷新策略未由本单元证明 |
| fstat/mqd；x86_64 5；其余三架构 80 | v7.1 [vfs_fstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L257) | mqd 与名字报告同一 inode 及属性 | sys_fstat → resolve_fd → MqDescriptor.stat → queue SimpleInode → metadata_to_kstat | 部分正确 | 同一 mqueue C 核对 dev/ino、双向 chmod 和固定 size=80；x86 与 Linux 通过，其他目标及完整字段待最终 CI/专门证据 |
| newfstatat/fstatat/mqueue；x86_64 262；其余三架构 79 | v7.1 [newfstatat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L498) | 名字查找读到队列的实际属性 | sys_fstatat → resolve_at → SimpleDir → queue-backed FileMaker → SimpleInode | 部分正确 | 同一 C 的 stat 观察队列属性共享；C 库选择的原始 stat 入口未单独跟踪，不外推全部 flags |
| statx/静态伪实体；x86_64 332；其余三架构 291 | v7.1 [do_statx](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c) | 掩码与 flags 决定原实体的可见字段 | sys_statx → retained path → SimpleFsNode/SimpleInode → statx conversion | 无法确认 | 真实 Native 旅程证明普通根/文件实体寿命；缺少该入口的直接 rematerialization 与字段掩码证明 |

### 12.2 队列与对象

当前队列名字域仍是全局 `MQ_REGISTRY`，共享 FS 与该域对应，不新增业务 ID
或属性登记表。匿名对象保留既有物理 inode 分配域；类型化失败传播不代表
已经把全局分配器的终止式 OOM 策略改成可恢复 Linux ENOMEM。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| mq_open；x86_64 240；其余三架构 180 | v7.1 [do_mq_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L911) | 已有名字的权限来自队列 inode | sys_mq_open → MQ_REGISTRY → MessageQueue.check_open_access → shared SimpleInode | 部分正确 | 完整 mqueue 状态读取旅程通过；chmod/chown 后跨凭据 reopen 与完整限额错误顺序未在本单元证明 |
| mq_unlink；x86_64 241；其余三架构 181 | v7.1 [mq_unlink](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L940) | sticky 检查使用当前 inode 属主 | sys_mq_unlink → queue.uid → SimpleInode；queue Drop refunds creator_uid | 部分正确 | 共享属主与固定计费创建者已分开；跨属主 unlink/最后描述释放后的 charge 退还尚缺直接旅程 |
| mq_notify；x86_64 244；其余三架构 184 | v7.1 [do_mq_notify](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L1266) | 注册状态变更维护同一 inode 时间 | sys_mq_notify → MessageQueue.register_notify → message mutex → inode time lock | 部分正确 | 原通知功能由完整 system 继续执行；与时间修改的受控交错、字段观察未在本单元证明 |
| mq_getsetattr；x86_64 245；其余三架构 185 | v7.1 [mq_getsetattr](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L1427) | 描述 flags 与共享 inode 时间分别维护 | sys_mq_getsetattr → MqDescriptor flags/MessageQueue.touch_attr → SimpleInode | 部分正确 | 每次打开的 flags/偏移保持独立；新时间 owner 的完整 syscall 字段/竞争证明待补 |
| read/mqd；x86_64 0；其余三架构 63 | v7.1 [mqueue_read_file](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c) | 状态偏移属于打开描述，成功复制后维护时间 | sys_read → MqDescriptor.read → status snapshot/user copy → queue SimpleInode times | 部分正确 | 既有程序验证分段读、EOF、重新打开的独立偏移、lseek 和只写拒绝；Linux 与修后 x86 通过，完整 EFAULT/时间语义未由此证明 |
| read/mqueue 普通路径 fd；x86_64 0；其余三架构 63 | v7.1 [mqueue_read_file](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c) | 普通路径打开同样使用队列文件操作 | sys_read → actual QueueStatus → retained MqDescriptor.read → cursor mutex → status snapshot/user copy → shared SimpleInode times | 部分正确 | f7ff4bd7a1 的普通分派在同一 C 失败，2bcfc68ab9 通过；10ed803b4a 增强定位/故障旅程通过；原 File 保留只读挂载，名字/IPC 域和单段部分复制仍有缺口 |
| pipe；x86_64 22 | v7.1 [do_pipe2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c) | 分配共享管道 inode，失败不发布描述 | syscall/mod.rs legacy pipe → sys_pipe2(flags=0) → Pipe::new → Shared::new → ObjectInode::new | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |
| pipe2；x86_64 293；其余三架构 59 | v7.1 [do_pipe2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c) | 两端共享实际管道实体及其寿命 | sys_pipe2 → Pipe::new → Shared::new → ObjectInode::new | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |
| socket；x86_64 41；其余三架构 198 | v7.1 [sock_alloc](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c) | socket 拥有实际 inode，失败清理构造资源 | sys_socket → Socket/PacketSocket/NetlinkSocket::new → ObjectInode::new | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |
| socketpair；x86_64 53；其余三架构 199 | v7.1 [socketpair](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c) | 两个端点分别拥有 inode，部分失败不发布半对 fd | sys_socketpair → Socket::new for both endpoints → ObjectInode::new → fd installation | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |
| accept；x86_64 43；其余三架构 202 | v7.1 [accept](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c) | 新连接取得独立 socket inode | sys_accept → sys_accept4 → listener.accept_user → Socket::new → ObjectInode::new | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |
| accept4；x86_64 288；其余三架构 242 | v7.1 [accept4](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/net/socket.c) | 新描述 flags 与连接资源按操作提交 | sys_accept4 → listener.accept_user → Socket::new → ObjectInode::new → fd installation | 部分正确 | 构造方已全部迁移为返回可报告分配错误；265 项 Native 与完整 x86 system 验证正常对象寿命，低内存及全部 flag/错误优先级未在本单元证明 |

## 13. 队列描述与定位读取

普通路径打开使用实际节点保留的 QueueStatus 生产者选择 MqDescriptor，原 File 保留挂载和元数据路径。以下范围以 Linux v7.1 固定源码及同一 C 的 Linux 6.8 对照为依据；最终实现 10ed803b4a 的同一 C 在 x86 通过，72 项四目标 Clippy 与 Kernel 标准库检查通过；88cee91658 的完整 x86 system 541/541 单独记录。全局队列名字域、mq_open 的名字显示和单段内部部分复制等缺口继续保留。

### 13.1 数据接口

定位与顺序读取共用状态快照和复制后的时间提交规则，游标分别由局部值和描述锁保护。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| readv；x86_64 19；其余三架构 65 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L992) | 传统 read 文件操作逐段读取，后续失败保留已完成段，顺序读取共用游标 | sys_readv → MqDescriptor.read_vectored → cursor mutex → IoVectorBuf.read_segments → read_status | 无法确认 | 本单元直接故障旅程使用 preadv2；顺序 readv 的受控并发及同一段内部部分复制仍待证明 |
| pread64；x86_64 17；其余三架构 67 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L753) | 定位状态读不改变共享游标，失败不更新时间 | sys_pread64 → retained MqDescriptor.read_at → local offset → read_status | 部分正确 | 同一 C 在 Linux 通过、568402cfbc 失败；10ed803b4a 的同一 C 在 x86 通过；单段内部分复制与其余目标不据此宣称完成 |
| preadv；x86_64 295；其余三架构 69 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L1120) | 定位向量读取共用本次局部偏移；只写描述先于 iovec 导入拒绝 | sys_preadv → sys_preadv2 → MqDescriptor.validate_read_access/read_vectored → read_segments | 部分正确 | 同一 C 覆盖完整内容、原游标不变及坏 iovec 的 EBADF 优先；10ed803b4a 的同一 C 在 x86 通过 |
| preadv2；x86_64 327；其余三架构 286 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L1184) | flags=0 时后续向量段失败返回已完成的前缀；offset=-1 使用共享游标 | sys_preadv2 → MqDescriptor.read_vectored → read_segments/read_status | 部分正确 | 同一 C 直接验证有效 8 字节后缺页返回 8；offset=-1、RWF_* 与单段内部分复制不由该判据覆盖 |
| pwrite64；x86_64 18；其余三架构 68 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L784) | 队列无原始写能力，先按访问模式返回 EBADF 或 EINVAL | sys_pwrite64 → retained MqDescriptor.status_write_error | 部分正确 | 同一 C 以坏用户指针核对可写描述 EINVAL；完整模式、偏移和错误优先级仍有未验证输入 |
| pwritev；x86_64 296；其余三架构 70 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L1196) | 队列无原始写能力，先于向量导入拒绝 | sys_pwritev → sys_pwritev2 → positioned_write_file → MqDescriptor.status_write_error | 无法确认 | 本单元直接测试 pwritev2；未单独运行此入口的完整输入旅程 |
| pwritev2；x86_64 328；其余三架构 287 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L1204) | flags=0 的队列定位写在用户向量导入前拒绝 | sys_pwritev2 → positioned_write_file/validate_write_access → MqDescriptor.status_write_error | 部分正确 | 同一 C 用坏 iovec 核对 EINVAL；offset=-1、其他模式和 flags 组合未穷尽 |
| write；x86_64 1；其余三架构 64 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L668) | 可写队列描述缺少原始 write 操作，返回 EINVAL | sys_write → MqDescriptor.validate_write_access → status_write_error | 部分正确 | 普通路径写在 f7ff4bd7a1 返回 EBADF；2bcfc68ab9 与 Linux 同一 C 返回 EINVAL；只读和坏地址组合另有边界 |
| writev；x86_64 20；其余三架构 66 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L1031) | 对象写能力检查先于向量导入 | sys_writev → MqDescriptor.validate_write_access → status_write_error | 无法确认 | 共享对象门禁已接通，缺少普通路径 fd 的直接 writev 判据 |
| lseek；x86_64 8；其余三架构 62 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L53) | SEEK_END 以 inode 固定大小 80 为基准，溢出拒绝返回 EINVAL 并保留游标 | sys_lseek → FileLike.seek → MqDescriptor.seek_status → read_offset mutex | 部分正确 | 同一 C 覆盖固定大小、LLONG_MAX 后加 1 拒绝及原游标保持；10ed803b4a 的同一 C 在 x86 通过 |
| fcntl/状态标志；x86_64 72；其余三架构 25 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L1387) | mq_setattr 只修改 O_NONBLOCK，保留 O_APPEND 和独立打开的描述状态 | sys_fcntl → MqDescriptor flags atomic；sys_mq_getsetattr → set_nonblocking_flag | 部分正确 | 同一 C 覆盖 dup 共享、另一打开独立、APPEND/NONBLOCK 保留与不同 CLOEXEC 规则；其他 fcntl 命令另行核对 |

### 13.2 描述与对象

打开、复制和关闭决定实际队列、打开状态以及原挂载的寿命。普通路径 fd 保留原权限检查结果，mq_open 继续使用 IPC 对象挂载。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| open；x86_64 2 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 普通路径按 inode 文件操作选择队列描述 | legacy sys_open → sys_openat → add_to_fd → mqueue_at_location → MqDescriptor.from_file | 无法确认 | 本单元直接调用 openat，未单独运行 legacy open |
| openat；x86_64 257；其余三架构 56 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 普通路径与别名挂载保留打开时的真实队列和挂载 | sys_openat → FsView/OpenResult → add_to_fd → retained QueueStatus → MqDescriptor.from_file | 部分正确 | 原普通路径 mq_* 失败，2bcfc68ab9 的同一完整 C 与 Linux 通过；创建、截断、unlink 后重建及 IPC 域仍有缺口 |
| openat2；四架构 437 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 受解析约束选择的真实对象使用同一文件操作分派 | sys_openat2 → captured FsView → add_to_fd → MqDescriptor.from_file | 无法确认 | 类型分派共用，未直接覆盖队列 openat2 的解析 flags 旅程 |
| mq_timedsend/普通路径 fd；x86_64 242；其余三架构 182 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L1057) | 普通打开的队列 fd 可发送到实际 inode 队列 | sys_mq_timedsend → descriptor_from_fd → MqDescriptor.queue → MessageQueue.send | 部分正确 | 同一 C 经普通路径发送优先级 5 的 path 消息；另一 mq_open 描述接收该消息，限额/超时与其他错误不由此证明 |
| mq_timedreceive/与路径描述共享队列；x86_64 243；其余三架构 183 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L1157) | 不同打开共享消息与优先级状态 | sys_mq_timedreceive → descriptor_from_fd → MessageQueue.recv | 部分正确 | 同一 C 核对 4 字节、优先级 5 和内容；完整超时、取消与消息复制错误仍需原功能边界 |
| dup；x86_64 32；其余三架构 23 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/file.c) | 副本共享描述游标和状态，CLOEXEC 属于 fd 表项 | sys_dup → shared Arc<FileLike> → MqDescriptor read_offset/flags | 部分正确 | 同一 C 分段读与 mq_setattr 经副本观察共享结果；进程复制和 exec 边界不由此证明 |
| dup2；x86_64 33 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/file.c) | 替换旧表项的关闭行为与新描述共享分别提交 | sys_dup2 → replacement close hook → shared MqDescriptor | 无法确认 | 未新增普通路径队列替换和通知清理的直接旅程 |
| dup3；x86_64 292；其余三架构 24 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/file.c) | 副本共享描述并按参数设置 CLOEXEC | sys_dup3 → descriptor table → MqDescriptor | 无法确认 | 此入口未直接覆盖队列路径描述 |
| close；x86_64 3；其余三架构 57 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L658) | 每次 fd 关闭均清理当前进程拥有的通知 | sys_close → release_locks_on_close → MqDescriptor.on_close → queue.clear_notify_owner | 部分正确 | 同一 C 关闭路径 fd 的副本后通过原 mqd 重新注册成功；完整跨进程/退出分支仍待相应证明 |
| close_range；四架构 436 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/ipc/mqueue.c#L658) | 每个关闭的队列表项执行 flush | sys_close_range → release_locks_on_close → MqDescriptor.on_close | 无法确认 | 共用 flush 路径；本单元未直接验证此入口的队列路径描述 |
| fchmod/只读队列别名；x86_64 91；其余三架构 52 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L618) | fd 元数据写使用原挂载，只读 bind mount 返回 EROFS | sys_fchmod → descriptor_path → MqDescriptor.backing_file → original PathHandle → MetadataRequest | 部分正确 | 同一 C 通过只读 bind alias 打开，fchmod 必须 EROFS；Linux 与 2bcfc68ab9 已通过，完整凭据/userns 权限仍待证明 |
| utimensat/队列 fd；x86_64 280；其余三架构 88 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/utimes.c) | NULL 路径在保留描述的实际 inode 上更新时间 | sys_utimensat → descriptor_path → original path or IPC object mount → shared SimpleInode | 部分正确 | 同一 C 设置固定过去时间后核对 EOF 和 EFAULT 不推进时间；范围、权限和 userns 未由此覆盖 |
| fchown/队列路径 fd；x86_64 93；其余三架构 55 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 元数据授权按原描述挂载与实际 inode 解释 | sys_fchown → descriptor_path → MqDescriptor.backing_file → MetadataRequest | 无法确认 | 路径投影已保留原挂载；本单元只直接验证 fchmod 拒绝 |
| statx/队列路径 fd；x86_64 332；其余三架构 291 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c) | 描述选择保留实际挂载与 inode 属性 | sys_statx → descriptor_path → original PathHandle → shared metadata | 无法确认 | 未直接覆盖 AT_EMPTY_PATH、挂载 ID 与属性掩码 |
| fstatfs/队列路径 fd；x86_64 138；其余三架构 44 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/statfs.c#L116) | 通过保留挂载报告文件系统与挂载标志 | sys_fstatfs → descriptor_path → original mount → SimpleFs.stat | 无法确认 | 只读元数据门禁已直接验证，尚未直接检查该 fd 的 statfs 标志 |
| ioctl/队列路径 fd；x86_64 16；其余三架构 29 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/ioctl.c) | 按队列文件类型处理命令与错误 | sys_ioctl → FileLike defaults or MqDescriptor flags | 无法确认 | 本单元未直接执行队列 ioctl 命令 |
| mmap/队列路径 fd；x86_64 9；其余三架构 222 | v7.1 [决定语义的实现](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/mmap.c) | 队列不提供普通文件数据映射 | sys_mmap → MqDescriptor.file_mmap default → ENODEV | 无法确认 | 本单元没有队列 mmap 的直接运行证据 |

### 13.3 就绪观察

普通路径描述的 Pollable 现在委托实际消息队列，读就绪由消息数量决定，写就绪由容量决定。本单元没有直接验证这些入口的队列路径描述，保留无法确认。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| poll；x86_64 7 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 按所选队列的消息与剩余容量观察就绪 | legacy poll → sys_ppoll → MqDescriptor.poll → MessageQueue.poll | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| ppoll；x86_64 271；其余三架构 73 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 按所选队列的消息与剩余容量观察就绪 | sys_ppoll → queue Pollable/registrations → MessageQueue | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| select；x86_64 23 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 按所选队列的消息与剩余容量观察就绪 | legacy select → sys_pselect6 → queue Pollable | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| pselect6；x86_64 270；其余三架构 72 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 按所选队列的消息与剩余容量观察就绪 | sys_pselect6 → queue Pollable/registrations | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| epoll_ctl；x86_64 233；其余三架构 21 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 按所选队列的消息与剩余容量观察就绪 | sys_epoll_ctl → retained interest → MqDescriptor registrations → queue poll sets | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| epoll_wait；x86_64 232 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 按所选队列的消息与剩余容量观察就绪 | sys_epoll_wait → eventpoll ready queue → MqDescriptor.poll | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| epoll_pwait；x86_64 281；其余三架构 22 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 按所选队列的消息与剩余容量观察就绪 | sys_epoll_pwait → eventpoll wait → queue readiness | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |
| epoll_pwait2；四架构 441 | v7.1 [就绪入口](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/eventpoll.c) | 按所选队列的消息与剩余容量观察就绪 | sys_epoll_pwait2 → eventpoll wait → queue readiness | 无法确认 | 类型分派与原队列实现共用，缺少此入口的路径描述直接旅程 |

## 14. 可执行头的当前内容

`ElfImage` 只保留本次加载的解析结果，主映像和 PT_INTERP 都从已有数据页缓存重新读取。
本轮直接回归增强原执行/写互斥程序，没有增加默认用例或启动。完整 ELF 错误兼容性
仍按入口分别判断，不能从主映像的写后重执行外推解释器错误或全部 execveat 标志。

### 14.1 写后重执行

已退出的映像允许取得 writer。修改并关闭该 writer 后，下一次执行必须使用当前
头部；错误执行失败后也要释放执行租约，恢复原始字节后能够再次执行相同 inode。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve/主映像写后重执行；x86_64 59；其余三架构 221 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L855) | 每次装载检查当前 ELF magic，坏主映像返回 ENOEXEC；失败不保留执行访问 | sys_execve → load_user_app → load_elf → ElfImage.load → CachedFile.read_at → 本次 ELFHeaders；ExecutableFile/MM 保留实际路径 | 部分正确 | 相同 C 在 Linux 通过、088637d6cd 稳定失败、2b31db7844 完整 x86 system 541/541 通过；恢复原头后同一 inode 再执行成功。06400fc668 的四架构均实际通过该程序；直接、映射和后端改写一致性仍需各自证明，解释器错误见 14.3 |
| execveat/写后重执行；x86_64 322；其余三架构 281 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L855) | 选定文件后同样读取当前头部；dirfd、空路径与 flags 保持各自检查顺序 | sys_execveat → one FsView/retained path → load_user_app → ElfImage.load → CachedFile → loader/MM | 无法确认 | 共用重新读取边界，原 exec family 程序在本轮完整批次通过；未直接运行 execveat 写后损坏/恢复旅程，不把 execve 的判据替代此入口 |

### 14.2 寿命与证据范围

`sys_umount2` 不再需要清理解析结果 LRU，正常 busy 判断继续保留真正的执行映像
和映射路径。`memtrack` 也移除已经没有对象可清理的头部缓存调用；四目标 72 项
Clippy 含其功能组合。最终源码 2b31db7844 另通过 Native 265/265 和 Kernel
标准库检查，原卸载旅程包含在完整 x86 system 中。

2b31db7844 阶段记录的解释器格式错误与短读差异由第 14.3 节继续处理。直接、
映射或后端写入的数据页一致性仍需要各自证明；移除旧解析头命中，不能代替全部
raw 消费者契约。该阶段原始日志、源码摘要及同条件微基准输入和样本保存在
生产证据键 `elf_header_freshness`。

### 14.3 解释器拒绝与恢复

`ElfImage::load` 在解析前检查本次已读字节数。主映像允许短脚本，PT_INTERP
要求完整原生头；`UnexpectedEof` 映射 EIO，`InvalidElfInterpreter` 映射
ELIBBAD，路径、权限和实际 I/O 错误保留原传播。以下范围有两个原始执行入口
的 x86 直接证据，不能据此宣称全部 ELF 结构或其他体系结构的兼容性。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve/PT_INTERP 格式与短读；x86_64 59；其余三架构 221 | v7.1 [elf_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L480)、[解释器检查](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L957) | 完整头的坏格式返回 ELIBBAD；不足原生头的短读先返回 EIO；失败保留调用者并释放主映像/解释器执行访问 | sys_execve → load_user_app/UserImageBuilder → load_elf → view.resolve/check_executable_access → ExecutableFile → ElfImage.load → StarryError.errno/abort | 部分正确 | 第一轮同一 C 在 Linux 104/0、cb2e029332 为 102/2、3e6f7499f9 为 104/0；第二轮相同增强 C 在 Linux 112/0、83960a291e 为 110/2、c463de79d5 为 112/0。完整读取后格式与短读的 x86 旅程通过；路径内嵌 NUL 后填充、全部格式/体系结构及其他优先级仍有缺口 |
| execveat/PT_INTERP 格式与短读；x86_64 322；其余三架构 281 | v7.1 [elf_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L480)、[解释器检查](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c#L957) | 选定文件后按同一解释器读取规则拒绝；dirfd 与 flags 仍先按自身规则检查 | sys_execveat → one FsView/selected path → load_user_app → load_elf → native-header minimum → InvalidElfInterpreter or UnexpectedEof → ABI | 部分正确 | 同一两轮 C 直接调用 SYS_execveat，分别稳定观察 EINVAL/ELIBBAD 的旧错误及修后 ELIBBAD/EIO；恢复同一解释器 inode 后实际执行 shell 并取得 42。AT_EXECVE_CHECK、全部 dirfd/flags 和上述格式缺口不由此证明 |

本轮没有增加默认用例或启动。最终 Kernel 标准库和四目标 Clippy 72/72 通过。测试程序自己的旧头文件 x86_64 后备编号另修正为 322：强制启用
后备分支时，原值在 Linux 为 98/14，修后 112/0；常规头文件下两版预处理逐字
相同。该项属于测试用例的 ABI 修正，没有作为 Starry 内核的产品失败证据。

6a651d64f5 的最终持续集成另已验证四架构真实 test-execve 程序和 Native suite，
44 项成功、5 个重复 PR 包装跳过。完整任务日志为 `ci_6a651d64f5_terminal`；
LoongArch 按配置仅重放失败输出，没有跳过该程序。这个终态早于缓存路径改动。

## 15. 缓存路径的打开描述

缓存遍历完成后输出真实 PathHandle，内核从原调用者快照选择的 fd 表安装描述。
本轮没有复制一份供后续 stat 使用的持久属性，也没有在最终安装阶段调用普通
设备打开工厂。生产证据键为 `cached_path_only`，设计第 7.3 节说明边界。

### 15.1 正项与负项观察

原 test-path-lookup-ownership 增强其私有 tmpfs 挂载旅程。相同 C 在 Linux
通过，6a4a9f23f1 的旧实现失败三项，均为 EOPNOTSUPP；46138dd272 的真实
x86 QEMU 为 1/1，成功后还验证原 inode、CLOEXEC、unlink 后属性和挂载 use。
测试源码逐字相同，默认用例和启动数不增加。普通打开、链接跟随、其他元数据
后端及受控任务发布竞争仍由前表各行明确列出，不能从一个正项外推完整支持。

Linux v7.1 的 [shmem_dir_inode_operations](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/shmem.c#L5248)
使用 [simple_lookup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/libfs.c#L69)，
丢弃缺失名字的负目录项；因此普通查询已经返回 ENOENT 后，缓存查询仍可为
EAGAIN。MemoryFs 保留没有负项版本能力的语义。有效负项的组件证明使用实际
DirNode 缓存，并通过另一个目录别名真实创建使其失效，不伪造 tmpfs 负缓存。

### 15.2 描述的使用与退休

缓存 fd 的后续操作仍访问实际描述。以下判据来自同一 x86 客机旅程，其他
体系结构尚无本轮缓存 fd 的运行证据；没有把先前提交的完整 CI 当作本轮结果。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fstat/CACHED fd；x86_64 5；其余三架构 80 | v7.1 [vfs_fstat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c) | 描述保留被打开的 inode，unlink 后仍可读取更新的 nlink | sys_fstat → get_file_like → File.stat → original PathHandle/Location.metadata → MemoryNode.inode | 部分正确 | 相同 C 的 fstat 消费者比较实际 dev/ino，并在 unlink 后读到 nlink=0；Linux 和 x86 QEMU 通过，其他目标及完整 stat 参数边界未由此证明 |
| fcntl/F_GETFD 缓存 fd；x86_64 72；其余三架构 25 | v7.1 [do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | O_CLOEXEC 安装到返回 fd 的表项，F_GETFD 返回 FD_CLOEXEC | sys_fcntl → original FileTable descriptor.cloexec；FileDescriptorReservation.try_install 在原表短写锁内安装 | 部分正确 | 原 O_PATH 旅程及 536b9c90af 的普通只读 fd 均要求 F_GETFD 包含 FD_CLOEXEC；Linux 对照通过，其他命令及新增只读组合的其他目标不由此证明 |
| close/缓存 fd；x86_64 3；其余三架构 57 | v7.1 [close_fd](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 关闭最后的路径描述后释放它持有的挂载 use，仍存活的其他描述继续保活 | sys_close → close_file_like → table.remove → release_locks_on_close/File drop → PathHandle/MountUseGuard | 部分正确 | 同一 Linux/x86 旅程先关闭原 File 和 dirfd，缓存 fd 仍使卸载 busy，最后关闭缓存 fd 后卸载成功；其他目标、dup 和 close_range 的缓存 fd 旅程待补 |
| umount2/唯一缓存 fd；x86_64 166；其余三架构 39 | v7.1 [path_umount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2035) | 打开的路径引用使普通卸载为 EBUSY，最后引用释放后允许卸载 | sys_umount2 → PathUnmount → actual MountUseState；缓存 File 保留 PathHandle，原 File 与 dirfd 已关闭 | 部分正确 | Linux 与 x86 客机在只剩缓存 fd 时实际得到 EBUSY，关闭后卸载成功；没有混用其他打开引用，其他目标和 detach/expire 不由此证明 |

组件测试还证明缓存查询不调用普通 lookup 或 metadata、锁竞争直接拒绝，以及
清除缓存后不能继续将未验证项作为命中。全局发布观察在隔离进程中真实进入
目标阶段，最外层拒绝零测试。18 包标准库及三包 81/81 项 Clippy 均通过。

### 15.3 链接文本与共享预算

c854aa5f7f 的缓存路径使用 MemoryFs 的实际不可变文本和拥有型路径帧，不调用
普通 LinkOps，也不把对象链接显示文字当作目标。相同增强 C 在 Linux 通过，
5983630d73 稳定失败 10 项，修后真实 x86 QEMU 1/1；普通预热后严格核对
40 次成功、41 次 ELOOP，组件集成测试另核对原末项后缀与共享预算。

缓存 BENEATH 根处和 NO_XDEV 向上的 EAGAIN 与普通模式 EXDEV 分开验证。
绝对 BENEATH 文本链接仍为 EXDEV，IN_ROOT 的绝对目标沿原起点根解析；
NO_SYMLINKS、NO_MAGICLINKS、最终 nofollow 和尾斜杠也在同一旅程中执行。
18 包标准库、两个修改软件包的 9/9 项 Clippy 和格式检查通过，Kernel Rust
未修改，默认用例及启动数不增加。完整记录为 `cached_link_text`；链接 atime、
挂载时间策略、动态 producer、普通缓存打开及原生缓存属性继续保留限制。

普通读取仍将同一实际文本投影到原接口类型。内部解析通过不等于用户态读取
入口已经取得直接证据，该边界单独列出。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| readlinkat/MemoryFs 文本；x86_64 267；其余三架构 78 | v7.1 [do_readlinkat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L524) | 读取实际链接文字并按缓冲区长度复制，不把目标作为新路径跟随 | sys_readlinkat → FsView.read_link → DirEntry.read_link → MemoryNode.read_at；原生查询将 FileContent 的 Arc 文本转换为 NamespaceRead.LinkText String | 无法确认 | 同一 C 的普通与缓存解析实际消费同一目标，标准库通过；本轮没有原始 SYS_readlinkat 的 MemoryFs 文本读取旅程，不能代替此入口的直接证据 |

### 15.4 普通只读描述

536b9c90af 复用实际 inode 的已有无 backing 页缓存，并在最终 cached 属性上检查
本次凭据。相同最终 C 在 Linux 通过，4c2ab40a5e 的旧 Starry 失败四项，修后
真实 x86 QEMU 为 1/1。原载荷、共享和独立位置、状态及目录条目分别检查，
所有新增描述均在既有唯一 O_PATH 卸载判据前关闭。组件测试使用真实 MemoryFs，
证明冷缓存拒绝、预热后准入，以及 chmod 后新打开拒绝而旧描述仍可读。

最终链接是否跟随与最终对象是否必须为目录分开处理；各消费者仍由原打开
描述持有状态。下列结果来自同一实际系统旅程，其他架构的本轮普通只读运行、
完整参数与错误顺序没有由这次验证证明。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat2/CACHED 普通 nofollow；四架构 437 | v7.1 [may_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4238) | 未跟随的最终符号链接按普通内容打开时返回 ELOOP | sys_openat2 → CachedLookup 保留最终链接 → OpenOptions.open_cached_path 的 Symlink 检查 | 部分正确 | 相同最终 C 在旧实现为 EAGAIN，Linux 与 536b9c90af x86 客机为 ELOOP；其他目标与其余错误优先级待补 |
| openat2/CACHED 目录标志与 nofollow；四架构 437 | v7.1 [do_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4655) | O_PATH+O_NOFOLLOW+O_DIRECTORY 不跟随最终链接，链接自身不是目录则 ENOTDIR | sys_openat2 → CachedLookup.walk 中 require_directory 只检查最终类型，不参与 follow 决策 | 部分正确 | 指向以斜杠结尾的目录文本链接：旧实现错误打开目标，Linux 与修后 x86 均 ENOTDIR；其他目标待补 |
| openat2/CACHED 输入尾斜杠；四架构 437 | v7.1 [lookup_fast_for_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4532) | 输入尾斜杠要求跟随最终链接并取得目录，即使提供 O_NOFOLLOW | sys_openat2 → CachedLookup.walk trailing_directory → cached_link_target → 原帧及最终目录检查 | 部分正确 | 同一 Linux/x86 用例实际打开带尾斜杠的目录链接；链接目标尾斜杠仅在已经跟随目标后生效，其他目标待补 |
| read/CACHED 普通文件；x86_64 0；其余三架构 63 | v7.1 [ksys_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c) | 从实际文件内容按共享打开位置读取并推进位置 | sys_read → get_file_like → kernel File.read → ax File.read/position → CachedFileShared 原数据页 | 部分正确 | 原始 read 读到原 writer 的载荷，dup 后从后继位置继续，独立打开从零开始；Linux/x86 通过，完整部分复制与其他目标不由此证明 |
| pread64/CACHED 普通文件；x86_64 17；其余三架构 67 | v7.1 [ksys_pread64](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c) | 定位读取实际内容，不修改打开描述的共享位置 | sys_pread64 → file_or_espipe → ax File.read_at → CachedFileShared；顺序位置仍由原 File 持有 | 部分正确 | 原始 pread64 在偏移 10 读取三个正确字节，后续 read 从偏移 7 继续；Linux/x86 通过，其余参数、fd 竞争和其他目标待补 |
| dup/CACHED 普通文件；x86_64 32；其余三架构 23 | v7.1 [dup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/file.c) | 新 fd 共享原打开描述，独立 open 不共享游标 | sys_dup → dup_fd → 原 FileDescriptor.inner Arc → 同一 ax File.position/CachedFileShared | 部分正确 | 原始 dup 之后连续 read 消费载荷的下一段，第二次 open 从头读；Linux/x86 通过，最后 dup 的独占卸载引用与其他目标待补 |
| write/CACHED 只读文件；x86_64 1；其余三架构 64 | v7.1 [vfs_write](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c) | 没有写访问权的描述拒绝写入，返回 EBADF | sys_write → File.validate_write_access → 原 ax FileFlags.READ，无 writer lease | 部分正确 | 原始 write 在有效缓冲区得到 EBADF，Linux/x86 通过；写族其余入口、错误顺序与其他目标待补 |
| fcntl/F_GETFL 缓存只读文件；x86_64 72；其余三架构 25 | v7.1 [do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | 返回只读访问模式和该描述的 O_NONBLOCK 状态 | sys_openat2.force_largefile → File.open_flags；sys_fcntl.F_GETFL 对原参数作状态投影，再查询原描述的 nonblocking | 部分正确 | f4a9c2cb24 的严格 C 在旧 Starry 返回 526592，4732aeade7 与 Linux 均为 34816，已排除 O_CLOEXEC/O_NOCTTY 并保留 O_LARGEFILE/O_NONBLOCK；F_SETFL 切换、其他目标与其余命令不由此证明 |
| getdents64/CACHED 只读目录；x86_64 217；其余三架构 61 | v7.1 [iterate_dir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/readdir.c) | 目录打开描述在原目录上维护枚举状态 | sys_getdents64 → Directory.position/read_state → original Location.open_directory_read_state/read_dir_with_state → MemoryNode | 部分正确 | 私有 tmpfs 的缓存只读目录由原始 getdents64 读到实际条目；Linux/x86 通过，条目布局、续读与其他目标不由此证明 |

实际数据与路径由既有所有者保留，最终属性没有复制为持久 stat 副本。18 包
标准库、三包 Clippy 81/81 和格式检查通过，记录为 cached_readonly_descriptions。写准入、端点、
额外 flag、原生缓存元数据及受控任务和 fd 发布仍在原计划内。

## 16. 显示时钟与事件序号

本轮在真实计时器 producer 保留已发布边沿，避免尚未前推的截止时间产生负帧差。
同时纠正系统程序对绝对零和相对零的混用。以下调用链已从当前 card0 的实际
打开描述追踪；`Card0File` 与匿名 DRM-KAPI 文件的 `KapiFile` 是不同入口。
完整时间锚和测试边界见 [DRM 等待契约](drm-vblank-wait.md)。

### 16.1 控制与计数

固定 Linux 的计数算法和用户请求语义分别核实。本地任务定时器保留旧边沿的
适配另有真实核心组件回归，不能把受控 C 请求回放当作 Linux DRM 运行对照。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl/WAIT_VBLANK 当前查询；x86_64 16；其余三架构 29 | v7.1 [drm_wait_vblank_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1734) | RELATIVE 和零增量查询当前计数；32 位绝对零不具有通用查询语义 | sys_ioctl → retained Card0File.ioctl → drm_ioctl → drm_wait_vblank_ioctl → per-CRTC DrmVblankCrtc count/time | 部分正确 | 实际 C 请求的受控回放在旧实现的高计数和回绕起点失败，修后通过；真实 Linux DRM 对照及四架构当前提交终态仍需分别核实 |
| ioctl/WAIT_VBLANK 相对等待与禁用；x86_64 16；其余三架构 29 | v7.1 [drm_wait_vblank_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1734) | 活动计数到达目标或 CRTC 禁用结束等待；禁用回复为当前冻结边沿，不是目标序号 | sys_ioctl → Card0File.ioctl → drm_wait_vblank_ioctl → vblank queue 与 DrmVblankCrtc.enabled/count/time；SETCRTC 的 off 路径发布冻结状态并唤醒 | 部分正确 | 子进程通过已有管道回传等待回复，父进程与实际禁用事件核对冻结序号和微秒截断；C 回放还拒绝伪造目标回复，保持原三秒 EBUSY 截止时间和信号测试 |
| ioctl/CRTC_QUEUE_SEQUENCE 相对零与未来事件；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_queue_sequence_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L2063) | RELATIVE 零增量立即送达；未来目标保留在队列，并共同消耗所属打开描述的 event_space | sys_ioctl → Card0File.ioctl → drm_crtc_queue_sequence_ioctl → per-open DrmFile event_space/event_list 与 per-device vblank_event_list | 部分正确 | 原程序保留另一 open 的事件隔离、128 个未来事件及容量耗尽判据；受控回放证明高计数时旧绝对零请求不能表示立即事件 |
| ioctl/CRTC_GET_SEQUENCE 定时器时间锚；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_get_sequence_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L2006) | 返回同一 CRTC 的一致计数和边沿时间；计时器尚无更晚边沿时不能因负差产生巨量帧数 | sys_ioctl → Card0File.ioctl → drm_crtc_get_sequence_ioctl → vblank_get/enable → timer timestamp producer → DrmVblankCrtc seqlock | 部分正确 | 真实核心受控旧实现从 0 跳到 3186109819，修后保留旧边沿并让后续真实边沿只计入一次；119 项核心标准库通过，仍不等同原生 Linux 高精度时钟 |
| ioctl/MODE_SETCRTC 时钟禁用与恢复；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_vblank_off](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1339) | 禁用冻结当前边沿并送出未完成事件；恢复建立新时间锚，关闭期间不计入活动帧数 | sys_ioctl → Card0File.ioctl → drm_mode_setcrtc → atomic simple CRTC disable/enable → drm_crtc_vblank_off/on → per-CRTC state | 部分正确 | 原程序的禁用事件、另一打开描述的冻结边沿和恢复计数判据继续保留；本地任务定时器适配的并发边界见 DRM 等待契约 |
| ioctl/MODE_PAGE_FLIP 事件边沿；x86_64 16；其余三架构 29 | v7.1 [drm_mode_page_flip_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_plane.c) | 已提交翻转的事件返回实际完成序号和边沿，不能把尚未发生的目标当作完成 | sys_ioctl → Card0File.ioctl → drm_mode_page_flip_ioctl → atomic simple CRTC flush → arm/send_vblank_event → per-open DrmFile | 部分正确 | 原完整程序核对新 framebuffer 绑定、FLIP_COMPLETE 类型、user_data、CRTC 和时间戳；四目标终态与真实 Linux DRM 对照仍分别记录 |

### 16.2 事件消费者

时钟产生的事件最终由原打开描述消费，读故障不能丢失尚未复制的事件。
普通读取、向量读取和就绪查询分别经过其实际系统调用入口，没有把同一设备的
共享核心测试当作每个用户入口都已完成的证明。
musl 的 [poll 实现](https://git.musl-libc.org/cgit/musl/tree/src/select/poll.c?h=v1.2.5)
在体系结构没有 `SYS_poll` 时调用 `SYS_ppoll`；不能把该包装调用直接记为
x86 独有 `poll` 入口的验证，也没有据此断言运行镜像的 musl 版本。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| read/DRM 事件；x86_64 0；其余三架构 63 | v7.1 [drm_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_file.c) | 从当前打开描述取完整事件；非阻塞空队列返回 EAGAIN | sys_read → FileLike.read → Card0File.read/read_events → drm_read_kernel_to → per-open DrmFile.event_list | 部分正确 | 原程序读翻转、当前及禁用事件，并验证独立 open 隔离；修正请求构造保持这些消费者断言有效，真实 Linux DRM 对照未执行 |
| readv/DRM 事件部分故障；x86_64 19；其余三架构 65 | v7.1 [drm_read](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_file.c) | 第二事件复制故障时返回第一事件已复制字节，并保留故障事件供下次读取 | sys_readv → IoDst/UserIoVecCursor → Card0File.read_events → drm_read_kernel_to/copy_event_to_io_dst → DrmFile.event_list | 部分正确 | 同一程序通过原始 SYS_readv 向第二个 PROT_NONE 向量复制，随后 SYS_read 必须取回第二事件；没有放宽原判据 |
| poll/DRM 就绪；x86_64 7；其余三架构无此入口 | v7.1 [do_sys_poll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 只在所属打开描述有已送达事件时报告可读，未来事件不提前可读 | sys_poll → do_poll → Card0File.poll/poll_events → drm_poll → per-open DrmFile.event_list/event_wait | 无法确认 | 程序保留未来事件零超时不可读和真实事件唤醒判据；本轮定向目标为 LoongArch，其 libc poll 使用 ppoll，不能代替 x86 poll 入口证据 |
| ppoll/DRM 就绪；x86_64 271；其余三架构 73 | v7.1 [do_ppoll](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/select.c) | 同一事件状态控制可读与等待唤醒；超时或信号不伪造完成事件 | sys_ppoll → do_poll → Card0File.poll/poll_events → drm_poll → DrmFile.event_wait 与 Card0File.poll | 部分正确 | LoongArch 程序的 poll 经 libc 到 ppoll，覆盖未来事件和真实事件；本轮未增加原始 ppoll 的信号掩码或超时边界证明 |
| close/DRM 打开描述；x86_64 3；其余三架构 57 | v7.1 [drm_release](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_file.c) | 关闭所属描述取消未完成事件并释放 event_space，不影响其他打开描述 | sys_close → table.remove → Card0File drop → drm_release → DrmFile event cancellation | 部分正确 | 原程序关闭持有 128 个未来事件的另一 open，要求原 open 不出现该描述的事件；更多并发关闭和 fd 共享状态不由此单独证明 |

## 17. 打开描述的状态标志

4732aeade7 将原始打开参数、描述符 CLOEXEC 和 F_GETFL 的状态投影区分。
设备关闭继续读取原参数；投影只改变返回给用户的位，不修改 fd 表或页数据。
相同最终 C 在 Linux 通过，f4a9c2cb24 的旧实现稳定失败四项，修后 x86 QEMU
1/1。两个 O_PATH 判据分别来自普通 openat 与 CACHED openat2。

### 17.1 原生大文件策略

64 位原生路径打开设置原始 UAPI 的 O_LARGEFILE，O_PATH 描述不取得该位。
AArch64 的位为 0400000，其余三个目标沿 asm-generic 为 00100000；直接 C
判据使用这些内核位，避免宿主 libc 把 O_LARGEFILE 定义为零掩盖差异。
legacy 入口与直接受测入口分开保留证据范围。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat/64 位状态；x86_64 257；其余三架构 56 | v7.1 [openat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1381)、[大文件位](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/uapi/asm-generic/fcntl.h#L50) | 普通原生 64 位打开在最终 f_flags 中包含 O_LARGEFILE；创建与描述符标志不作为状态返回 | sys_openat → force_largefile → flags_to_options → add_to_fd → File original open_flags；状态查询由 sys_fcntl 投影 | 部分正确 | 私有 tmpfs 文件以创建、独占、截断、NOCTTY、CLOEXEC 及读写方式打开，原始 F_GETFL 在 Linux/x86 均为 32770；其他目标的本轮状态读取待补 |
| openat2/64 位状态；四架构 437 | v7.1 [openat2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1389)、[AArch64 位](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/include/uapi/asm/fcntl.h#L26) | 严格解释原始 how，非 O_PATH 的原生 64 位打开补入 O_LARGEFILE；不把自动补位当作用户提供的 O_PATH 组合 | sys_openat2 原始 how/flags 校验 → force_largefile → 原 resolve 分支；CACHED 捕获仍只一次，readonly_flags 已允许 LargeFile | 部分正确 | CACHED 普通只读的原始 F_GETFL 在 Linux/x86 均为 34816，CACHED O_PATH 为 2097152；resolve=0 与其他策略的状态读取、其余目标待补 |
| open/64 位状态；x86_64 2 | v7.1 [open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1374) | 原生 legacy open 同样设置大文件状态，O_PATH 保留自己的状态子集 | sys_open → sys_openat.force_largefile → File/Directory → F_GETFL 投影 | 无法确认 | 共享调用链已核对，原链接预算旅程仍通过；本阶段没有对 legacy open 返回 fd 直接读取 F_GETFL，不能借用 openat 的状态证明 |
| creat/64 位状态；x86_64 85 | v7.1 [creat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1442) | 按 WRONLY+CREAT+TRUNC 执行，并在原生 64 位描述设置 O_LARGEFILE，创建位不暴露为状态 | sys_creat → sys_openat.force_largefile → 原创建/截断 → F_GETFL 投影 | 无法确认 | 本阶段没有原始 SYS_creat 的完整状态旅程，静态继承不作为运行通过 |

### 17.2 状态与描述符分离

F_GETFL 对 O_PATH 只保留 O_PATH、O_DIRECTORY 和 O_NOFOLLOW；普通描述去掉
O_CLOEXEC、O_CREAT、O_EXCL、O_NOCTTY、O_TRUNC。原调用参数保留给实际
业务回调，FD_CLOEXEC 则继续存于 fd 表。此处只证明初始状态分类，没有把
F_SETFL 或所有端点的状态切换记为完成。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fcntl/F_GETFL 普通文件初始状态；x86_64 72；其余三架构 25 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1182)、[do_dentry_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L963)、[do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | 返回访问及状态，创建、独占、截断、NOCTTY 和 CLOEXEC 位不应泄露 | sys_fcntl.F_GETFL → original File.open_flags → shared status mask；原 File 和 fd 表字段不变 | 部分正确 | 同一最终 C 的旧 Starry 为 525250，Linux/4732aeade7 x86 为 32770；同一 fd 的 F_GETFD 仍为 FD_CLOEXEC，其他目标与状态切换待补 |
| fcntl/F_GETFL 普通 O_PATH；x86_64 72；其余三架构 25 | v7.1 [O_PATH_FLAGS](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1161)、[do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | 普通 openat 忽略的访问、创建、截断和非阻塞位不能重新出现在 O_PATH 的 F_GETFL 中 | sys_openat 的既有 O_PATH 解释 → kernel File original flags → sys_fcntl 的 path mask | 部分正确 | 携带 RDWR、NONBLOCK、CLOEXEC、CREAT、EXCL、TRUNC、NOCTTY 的 O_PATH 原始状态从旧 2624450 修为 Linux 的 2097152；后续实际载荷未被截断，其他目标待补 |
| fcntl/F_GETFL 缓存 O_PATH；x86_64 72；其余三架构 25 | v7.1 [O_PATH_FLAGS](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1161) | 严格 CACHED O_PATH 的 CLOEXEC 属于 fd 表，F_GETFL 只显示实际路径状态 | captured CachedFileTable.reserve → original-table FileDescriptorReservation.try_install → original File.open_flags → sys_fcntl 的相同 path mask | 部分正确 | 相同 C 旧返回 2621440，Linux/4732aeade7 x86 均为 2097152，原 F_GETFD 仍保持 CLOEXEC；其他目标及其余组合待补 |
| fcntl/F_GETFD 普通文件及 O_PATH；x86_64 72；其余三架构 25 | v7.1 [do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | 对 F_GETFL 的投影不能清除表项的 FD_CLOEXEC | sys_fcntl.F_GETFD → current_fd_table descriptor.cloexec；普通打开由 add_to_fd/add_file_like 发布 | 部分正确 | 普通读写文件和携带忽略位的 O_PATH 均实际返回 FD_CLOEXEC；缓存 fd 的安装链另见 15.2，其他目标与执行关闭行为不由此证明 |

本阶段没有增加系统程序、默认启动或 Rust 单元测试。行为由原完整系统旅程
的确定性红绿证明；AX 的另一个提交只澄清缓存准入的类型错误注释。记录为
open_description_status。Kernel 一包标准库、四目标 Clippy 72/72、AX 注释的六项
Clippy 及格式检查通过；原只读阶段的 18 包结果保留原源码，不改写为新版本。

## 18. 缓存打开的描述符额度

14ad08a41a 在同次 binding 捕获结束后从原表预留最低 fd，再检查 pathname
相关的 BENEATH、dirfd 和查找。相同最终 C 在 Linux 通过，6799b62256 的
旧 Starry 失败三项，修后真实 x86 QEMU 为 1/1；输入检查、变更类 CACHED
拒绝和额度恢复后的槽号回滚保留原判据。

### 18.1 输入与额度顺序

私有进程只把自身 soft RLIMIT_NOFILE 暂时降为零，hard 值不变。原始请求
区分输入错误、变更类 flags 和已经进入查找阶段的错误，不把它们合成一种
EAGAIN。binding 捕获本身的竞争及完整参数边界仍须由受控并发阶段证明。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat2/CACHED pathname 错误与额度；四架构 437 | v7.1 [do_sys_openat2](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1355) | getname 的 EFAULT 和空路径 ENOENT 早于 fd 预留；额度为零不能覆盖这些错误 | sys_openat2 how/flags 校验 → vm_load_path_string/空串检查 → captured binding → reserve | 部分正确 | Linux/x86 在额度为零时分别得到 EFAULT 与 ENOENT；真实请求没有在调用前解引用 NULL，其他目标及边界长度待补 |
| openat2/CACHED 变更 flags 与额度；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1293) | CACHED 与创建或截断等组合在路径复制和 fd 预留前返回 EAGAIN | sys_openat2 的早期变更类检查 → WouldBlock，不进入 capture/reserve | 部分正确 | 额度为零且 pathname 为 NULL 的 CACHED+O_CREAT 请求在 Linux/x86 为 EAGAIN；其余变更组合与目标待补 |
| openat2/CACHED 冷名字与额度；四架构 437 | v7.1 [FD_PREPARE](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/linux/file.h#L195) | 可复制的非空名字先取得 fd，额度耗尽为 EMFILE，不先进入冷查找得到 EAGAIN | captured binding 退出保护 → CachedFileTable.reserve → FileDescriptorReservation.try_in_table/reserve_below；缓存查找尚未开始 | 部分正确 | 同一 C 的旧 EAGAIN 稳定失败，Linux/14ad08a41a x86 为 EMFILE；正项的 EMFILE 也通过，捕获竞争及其他目标待补 |
| openat2/CACHED 无效 dirfd 与额度；四架构 437 | v7.1 [FD_ADD](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/linux/file.h#L236)、[path_init](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2673) | fd 额度判断早于从 dirfd 选起点；耗尽时为 EMFILE，恢复额度后无效 dirfd 为 EBADF | original-table reservation → CachedFileTable.directory → same CachedLookup context | 部分正确 | 旧 EBADF 在满额度组合下失败；Linux/x86 满额度 EMFILE、恢复后 EBADF，其他目标和受控表竞争待补 |
| openat2/CACHED 绝对 BENEATH 与额度；四架构 437 | v7.1 [nd_jump_root](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L1127)、[path_init](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L2716) | 额度先为 EMFILE，成功预留后的绝对 BENEATH 才为 EXDEV，且该拒绝早于 dirfd 获取 | sys_openat2 仅延后 CACHED 的绝对拒绝 → reserve → explicit BENEATH absolute check → 原缓存遍历；普通分支保持原顺序 | 部分正确 | 旧满额度 EXDEV 稳定失败，Linux/修后 x86 为 EMFILE；既有非满额度绝对链接 EXDEV 仍通过，其他目标待补 |

### 18.2 预留的回滚

FileDescriptorReservation 同时持有原表 Arc 和未公开的 Reserved 槽。查找失败、
不支持的准入及最终 try-write 失败沿原 RAII 清理，成功时在原表写锁内消费
预留并安装，失败描述在锁外退休。FIFO 与缓存入口共用数字 fd 上限检查，
PreparedFileDescriptor 原 entry-count 上限没有改写。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat2/CACHED 失败后的槽号；四架构 437 | v7.1 [fd_prepare cleanup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/linux/file.h) | 查找失败归还未公开的 fd，后续打开仍选择原最低空闲号码 | original FileDescriptorReservation Drop → same table.release_reserved；成功由 try_install 消费，保持原 File/PathHandle | 部分正确 | dup 探测并关闭原最低号，实际冷查找和坏 dirfd 失败后，新缓存打开仍取得该号；Linux/x86 通过，完整受控发布竞争及其他目标待补 |

Linux 夹具把私有 tmpfs 明确设为 MS_NOATIME，并普通预热链接名称，消除
新链接粗粒度 atime/ctime 的边界依赖。首次未预热夹具的六项 Linux EAGAIN
独立保留，不作为产品红绿；该条件不证明 Starry 的 atime/挂载时间策略。
Kernel 一包标准库、四目标 Clippy 72/72 和三处 include-backed 格式检查通过，
记录为 cached_fd_admission。没有新增 Rust 单元、系统程序或默认启动。

## 19. 打开描述的可变状态

5451b16b07 从实际状态所有者投影 O_NONBLOCK，避免原始打开参数使清除操作
失效。Directory 在同一打开描述的 AtomicU32 中保存 O_APPEND/O_NONBLOCK；
dup 保留原 Arc，独立打开分别构造状态。相同 C 在 Linux 通过，e6a73d6358
旧 Starry 稳定失败 11 项，修后实际 x86 QEMU 为 1/1。

### 19.1 文件与目录的状态

原路径旅程遍历两位的全部状态，经 dup 修改并从原描述和副本观察，同时
确认另一独立打开不变。随后恢复初始状态，继续原有数据、位置、枚举和卸载
引用断言。这里不把其他状态标志或并发 F_SETFL 的整体原子性当作已经证明。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| openat/目录状态所有者；x86_64 257；其余三架构 56 | v7.1 [do_dentry_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L963) | 每次目录打开拥有独立状态；随后设置或清除两位不影响另一打开 | sys_openat → add_to_fd → Directory::new → per-description AtomicU32 | 部分正确 | Linux/x86 普通目录实际状态切换及独立打开隔离通过；其他目标的本轮运行待补 |
| openat2/CACHED 目录状态所有者；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1182)、[do_dentry_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L963) | 缓存目录打开仍生成独立打开描述，保留初始非阻塞位及以后可变状态 | captured binding/reservation → open_cached_path → Directory::new → original-table try_install | 部分正确 | Linux/x86 缓存目录实际切换、另一打开不变及后续 getdents 通过；完整缓存策略和其他目标待补 |
| fcntl/F_GETFL 普通及缓存 File 的可变位；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 清除初始 O_NONBLOCK 后不能由旧打开参数重新返回；访问方式和大文件位保留 | sys_fcntl → get_file_like → mask original O_APPEND/O_NONBLOCK → File.nonblock/append | 部分正确 | 相同 C 的缓存文件旧实现失败三项；Linux/5451b16b07 x86 的全部两位状态、访问位、共享及独立打开通过；其他目标待补 |
| fcntl/F_SETFL 普通及缓存 File 的两位后置条件；x86_64 72；其余三架构 25 | v7.1 [setfl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L38) | 返回成功后的 GETFL 反映 O_APPEND/O_NONBLOCK，原访问方式不变 | sys_fcntl.F_SETFL → retained File → existing set_nonblocking/set_append → current-status projection | 部分正确 | Linux/x86 经 dup 设置四种状态后，原 fd 与副本一致，独立打开不变；其他 flags、错误顺序、整体并发原子性及目标待补 |
| fcntl/F_GETFL 普通及缓存 Directory 的可变位；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 目录的可变位也属于打开描述；O_DIRECTORY 和大文件位不能随切换消失 | sys_fcntl → Directory.open_flags/nonblocking/append → same AtomicU32 | 部分正确 | 同一 C 的两类目录各有四项旧失败；Linux/修后 x86 全部状态、独立打开及恢复通过；其他目标待补 |
| fcntl/F_SETFL 普通及缓存 Directory 的两位后置条件；x86_64 72；其余三架构 25 | v7.1 [setfl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L38) | 目录不能用空 setter 假装提交成功；更改与 dup 共享，不改变另一打开 | sys_fcntl.F_SETFL → Directory.set_nonblocking/set_append → set_status_flag atomic RMW | 部分正确 | Linux/x86 覆盖置位、清除、同时置位及恢复；O_PATH 命令门禁、其他 flags 与并发多位提交尚未证明 |

### 19.2 共享与委托

dup 和 F_GETFD 的实际链路对文件与目录相同，分别保留打开描述和 fd 表项
状态。采用 retained File 的设备包装会经过相同投影，但本轮没有在这些具体
端点执行状态切换，下面按各自辅助链保留证据缺口。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| dup/文件和目录状态共享；x86_64 32；其余三架构 23 | v7.1 [dup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/file.c) | 新 fd 指向同一打开描述，设置状态可从原 fd 观察，独立打开保持隔离 | sys_dup → dup_fd → get_file_like/add_file_like → same Arc<FileLike> | 部分正确 | Linux/x86 对普通及缓存文件、普通及缓存目录均实际经副本设置并从两端查询；其他目标与受控 fd 替换竞争待补 |
| fcntl/F_GETFD 文件和目录的独立标志；x86_64 72；其余三架构 25 | v7.1 [do_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c) | 状态变化不能修改原表项 FD_CLOEXEC；dup 的新 fd 保持自己的描述符标志 | sys_fcntl.F_GETFD → current_fd_table.get(fd).cloexec，独立于 File/Directory 状态 | 部分正确 | Linux/x86 四类旅程切换后原描述符标志保持，dup 为零；本轮其他目标及 exec 关闭行为待补 |
| fcntl/F_GETFL MountTableFile 的非阻塞清除；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 委托的实际打开描述负责非阻塞位，不能被最初参数重新补回 | sys_fcntl projection → MountTableFile.open_flags/nonblocking → retained File | 无法确认 | 委托调用链和通用 File 的红绿已核对；未在实际挂载表描述执行本轮状态旅程 |
| fcntl/F_GETFL UsbDeviceFile 的非阻塞清除；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | USB 打开描述保留自身当前非阻塞状态 | sys_fcntl projection → UsbDeviceFile.open_flags/nonblocking → base File | 无法确认 | 状态委托已核对；没有以实际 USB lease 运行本轮设置、清除及 I/O 后置条件 |
| fcntl/F_GETFL V4l2File 的非阻塞清除；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 视频打开描述的状态与 dup 共享，独立 session 不受影响 | sys_fcntl projection → V4l2File.open_flags/nonblocking → base File | 无法确认 | uvc 功能组合参与静态检查；没有以真实视频 session 执行本轮状态及排队行为 |
| fcntl/F_GETFL Card1File 的非阻塞清除；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 当前非阻塞状态由实际设备打开描述持有 | sys_fcntl projection → Card1File.open_flags/nonblocking → base File | 无法确认 | rknpu 功能组合参与静态检查；本轮没有实际 NPU 打开描述的直接运行 |
| fcntl/F_GETFL RgaFile 的非阻塞清除；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 图形加速打开描述返回自己当前的状态，不能使用原参数代替 | sys_fcntl projection → RgaFile.open_flags/nonblocking → base File | 无法确认 | rga 功能组合参与静态检查；没有实际图形请求 session 的本轮运行 |

其余原始标志生产者已核对：Socket/PacketSocket/NetlinkSocket/DmaBuf 的原始
标志不含非阻塞位；Pipe 已主动去掉它；MqDescriptor、Card0File 和 DrmKapiFile
有自己的实时标志及 getter。默认 open_flags 为零的对象没有这个原始位。
这些事实不代替各端点 I/O、异步通知、O_PATH 命令门禁或整体并发原子性证明。

## 20. 路径描述的命令准入

22015c0325 的 sys_fcntl 先保留原表和打开描述，再检查 O_PATH 命令集。固定
Linux 的 check_fcntl_cmd 在参数处理前拒绝受限命令，因此 File、Directory
及两种打开方式共用同一准入链。相同 C 在 Linux 通过，旧实现失败 86 项，
修后实际 x86 路径旅程为 1/1。此前阶段的调用链记录保留各自源码版本。

### 20.1 路径描述的命令与错误顺序

四类描述各检查 22 个拒绝命令，共 88 次观察。旧实现有 84 次返回值或错误
不符，另有两项缺失 fd 错误顺序，修后全部通过。两个尚未实现的查询明确
列出，不能把准入集合与具体命令能力混为一谈。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fcntl/O_PATH 受限命令；x86_64 72；其余三架构 25 | v7.1 [check_fcntl_cmd 与 fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L572) | 路径描述对 F_SETFL、F_SETOWN/F_GETOWN、pipe 两命令、seals 两命令、六种 POSIX/OFD 锁、F_SETSIG/F_GETSIG、F_SETOWN_EX/F_GETOWN_EX、1035 至 1038 和未知命令，在读取参数或分派前统一 EBADF | sys_fcntl → original table/description capture → open_flags O_PATH → admission rejection；表读锁已释放 | 部分正确 | 同一 C 在普通/缓存 File/Directory 各检查这 22 个命令，参数零；旧 88 次中错 84 次，Linux/22015c0325 x86 全部 EBADF；d2b3ee0ceb 四架构原程序均通过，受控 fd 替换仍待补 |
| fcntl/缺失 fd 优先级；x86_64 72；其余三架构 25 | v7.1 [fd_raw lookup](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L594) | 不存在的 fd 在命令和用户参数处理前 EBADF | sys_fcntl → captured table.get → BadFileDescriptor，不进入 dispatch 或用户复制 | 部分正确 | 相同 C 的未知命令及 1035/NULL 旧为 EINVAL、EFAULT，Linux/修后 x86 均 EBADF；受控关闭重用及其他目标待补 |
| fcntl/O_PATH F_GETFL；x86_64 72；其余三架构 25 | v7.1 [F_GETFL](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L473) | 允许读取路径描述保留的状态，不授予数据访问能力 | retained FileLike → O_PATH branch → path/directory/nofollow mask | 部分正确 | 四类路径描述的固定状态与后续原数据拒绝断言在 Linux/x86 通过；其他目标待补 |
| fcntl/O_PATH F_GETFD；x86_64 72；其余三架构 25 | v7.1 [F_GETFD](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L466) | 允许读取 fd 表项的 FD_CLOEXEC | admission → captured table.read/get(fd).cloexec | 部分正确 | 四类路径描述读取及两种复制后的独立标志在 Linux/x86 通过；受控表项重用及其他目标待补 |
| fcntl/O_PATH F_SETFD；x86_64 72；其余三架构 25 | v7.1 [F_SETFD](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L469) | 允许修改表项 FD_CLOEXEC，不修改共享打开描述 | admission → captured table.write/set_cloexec | 部分正确 | 四类路径描述切换并恢复原标志，Linux/x86 通过；并发槽号替换和其他目标待补 |
| fcntl/O_PATH F_DUPFD；x86_64 72；其余三架构 25 | v7.1 [F_DUPFD](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L457) | 复制同一打开描述，新 fd 的 CLOEXEC 清零 | admission → dup_fd_min(original table, retained description) → FileDescriptor | 部分正确 | 四类路径描述实际复制、状态相同及新标志为零在 Linux/x86 通过；已有普通 fd 最低号用例也通过，完整限额及竞争待补 |
| fcntl/O_PATH F_DUPFD_CLOEXEC；x86_64 72；其余三架构 25 | v7.1 [F_DUPFD_CLOEXEC](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L460) | 复制同一打开描述，新 fd 设置 CLOEXEC | admission → dup_fd_min(original table, retained description, true) | 部分正确 | 四类路径描述实际复制及 FD_CLOEXEC 在 Linux/x86 通过；完整 exec、限额和竞争矩阵待补 |
| fcntl/O_PATH F_CREATED_QUERY；x86_64 72；其余三架构 25 | v7.1 [F_CREATED_QUERY](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L454) | 允许查询描述创建事实，返回具体查询结果 | admission allows → existing default InvalidInput；查询本体未实现 | 不正确 | 静态调用链确认既有实现缺口；本轮没有宣称查询功能通过，Linux 6.8 宿主也不作为 v7.1 新命令运行证据 |
| fcntl/O_PATH F_DUPFD_QUERY；x86_64 72；其余三架构 25 | v7.1 [F_DUPFD_QUERY](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fcntl.c#L463) | 允许查询另一 fd 是否共享打开描述 | admission allows → existing default InvalidInput；比较本体未实现 | 不正确 | 静态确认准入后仍走既有 EINVAL，未借用 F_DUPFD 的成功结果作为该查询证据 |

### 20.2 保留描述的实际消费

普通描述的锁、pipe 和 seals 在准入后使用同一个 Arc；用户复制、具体类型
转换与等待不持有 fd 表保护。既有三个系统用例各自实际启动为 1/1，其中
锁生命周期检查 close/exit/exec 和 OFD 最后关闭后的等待者进展。这些结果
覆盖本次保活改动的正常使用，尚未制造查找后 fd 被重用的受控交错。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| fcntl/F_GETLK；x86_64 72；其余三架构 25 | v7.1 [fcntl_getlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | 用捕获描述的 inode 查询 POSIX 冲突并回写结果 | retained Arc → dispatch_fcntl → fcntl_getlk → lockable → original inode key | 部分正确 | test-dup-fcntl 实际查询/冲突通过；控制 fd 替换及完整 ABI 边界待补 |
| fcntl/F_SETLK；x86_64 72；其余三架构 25 | v7.1 [fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | 对捕获描述对应 inode 提交不等待的 POSIX 锁 | retained Arc → dispatch_fcntl → fcntl_setlk → lockable → POSIX owner | 部分正确 | test-dup-fcntl 与生命周期用例通过；未把已有锁协议概括为完整 Linux 兼容 |
| fcntl/F_SETLKW；x86_64 72；其余三架构 25 | v7.1 [fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | 等待者保活目标，不能带 fd 表读锁等待关闭者 | retained Arc → fcntl_setlk(wait) → existing wait loop，table guard absent | 部分正确 | test-fcntl-lock-lifecycle 的 close/exit/exec 唤醒实际通过；受控 fd 重用与其他目标待补 |
| fcntl/F_OFD_GETLK；x86_64 72；其余三架构 25 | v7.1 [fcntl_getlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | OFD 查询保留打开描述身份，区别于进程锁所有者 | retained Arc → fcntl_getlk → lockable → OFD identity | 部分正确 | test-dup-fcntl 的 OFD 查询和冲突实际通过；受控描述替换待补 |
| fcntl/F_OFD_SETLK；x86_64 72；其余三架构 25 | v7.1 [fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | OFD 非等待锁依附原打开描述 | retained Arc → fcntl_setlk → original OFD identity | 部分正确 | 既有 OFD 冲突和 churn 判据通过，最后关闭生命周期保留；完整竞争及 ABI 待补 |
| fcntl/F_OFD_SETLKW；x86_64 72；其余三架构 25 | v7.1 [fcntl_setlk](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | 阻塞 OFD 锁保活原描述，最终关闭后等待者能前进 | retained Arc → fcntl_setlk(wait) → original identity/wait queue | 部分正确 | 生命周期用例实际核对 OFD 最后关闭与等待者进展；受控重用及其他目标待补 |
| fcntl/F_GETPIPE_SZ；x86_64 72；其余三架构 25 | v7.1 [pipe_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c) | 在已捕获的 pipe 上读取容量，路径描述先 EBADF | admission → retained Arc.downcast_arc Pipe → capacity | 部分正确 | O_PATH 拒绝在同 C 通过，四目标 Clippy 覆盖类型分支；本轮未直接运行普通 pipe 容量查询 |
| fcntl/F_SETPIPE_SZ；x86_64 72；其余三架构 25 | v7.1 [pipe_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/pipe.c) | 修改捕获 pipe 的容量并返回实际容量，路径描述先 EBADF | admission → retained Arc.downcast_arc Pipe → set_pipe_size/resize | 部分正确 | O_PATH 拒绝及静态检查通过；既有容量单元未冒充本轮普通 syscall 运行，完整容量边界待补 |
| fcntl/F_GET_SEALS；x86_64 72；其余三架构 25 | v7.1 [memfd_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/memfd.c) | 从捕获的 memfd 描述读取 seal 状态 | retained Arc → Memfd::from_description → direct Memfd or original File MemfdRef → get_seals | 部分正确 | test-memfd-seals 实际状态旅程通过；路径 File/Directory 拒绝先于转换，其他目标与受控替换待补 |
| fcntl/F_ADD_SEALS；x86_64 72；其余三架构 25 | v7.1 [memfd_fcntl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/memfd.c) | seal 提交作用于捕获描述，后续写入和复制受其限制 | retained Arc → Memfd::from_description → add_seals → existing backend policy | 部分正确 | test-memfd-seals 的状态、写/复制/splice 限制和失败位置保持实际通过；完整 ABI/竞争待补 |
| flock/保活 helper 迁移；x86_64 73；其余三架构 32 | v7.1 [flock](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/locks.c) | 原 flock 操作校验后取得目标，描述身份与锁生命周期保持 | sys_flock → flock_op → existing operation validation → get_file_like → lockable(retained Arc) | 部分正确 | 只迁移共用 helper 的输入，未调整原校验顺序；test-dup-fcntl 实际 flock 与绑定挂载身份判据通过，完整 flock 协议待补 |

本阶段没有新增 Rust 单元、系统程序或默认启动。Kernel 一包标准库、四目标
Clippy 72/72、格式与三处 include-backed 检查通过，记录为
path_fcntl_admission。顺序准入结果不替代 F_SETFL 整体并发原子性、普通描述
的提示/异步通知策略或全部系统调用兼容性。

本节的原路径程序在 d2b3ee0ceb 四架构 CI 均返回成功。LoongArch 配置使用
`starry-run-system-tests --capture-failures`，成功程序的标准输出被抑制；实际
程序开始、成功终态及普通未知 fcntl 控制仍可核验。其余受控竞争、未实现查询
和具体 pipe 分支的证据缺口保留，整轮 CI 尚有独立的 Axvisor LoongArch 失败。

## 21. 原生可执行头准入

ElfImage 在沿程序头范围读取前拒绝错误原生机器类型及主映像对象类型。
下表分别列出两入口、两类映像及主映像类型；每行只涵盖注明的字段，
不据此推导 class、程序头结构或 Linux 不可回退点已经完整适配。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve/主映像原生机器类型；x86_64 59；其余三架构 221 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | 主映像 e_machine 不匹配当前原生架构时，读取程序头前返回 ENOEXEC | sys_execve → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Main → fixed header machine → InvalidExecutable；原映像保留 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execve/解释器原生机器类型；x86_64 59；其余三架构 221 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | PT_INTERP 完整原生头的 e_machine 不匹配时返回 ELIBBAD，短头仍先 EIO | sys_execve → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Interpreter → minimum native header → machine → InvalidElfInterpreter；原租约退休 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execve/主映像对象类型；x86_64 59；其余三架构 221 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | 主映像只接受 ET_EXEC 或 ET_DYN，ET_REL 在程序头读取和映像替换前返回 ENOEXEC | sys_execve → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Main → fixed header type → InvalidExecutable；未发布映像退休 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execve/解释器对象类型晚期失败；x86_64 59；其余三架构 221 | v7.1 [load_elf_interp](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c)、[exec_binprm](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | ET_REL 解释器在 begin_new_exec 之后被拒绝，调用者终止于 SIGSEGV，不能恢复原映像并返回 ELIBBAD | sys_execve → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → Interpreter；目前仍先完整构造新 UserImage，晚期失败协议未接入 | 无法确认 | Linux 早期探针两 API 均实际 SIGSEGV；最终回归不包含该输入，尚无 Starry 同条件晚期状态证明 |
| execveat/主映像原生机器类型；x86_64 322；其余三架构 281 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | 主映像 e_machine 不匹配当前原生架构时，读取程序头前返回 ENOEXEC | sys_execveat → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Main → fixed header machine → InvalidExecutable；原映像保留 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execveat/解释器原生机器类型；x86_64 322；其余三架构 281 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | PT_INTERP 完整原生头的 e_machine 不匹配时返回 ELIBBAD，短头仍先 EIO | sys_execveat → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Interpreter → minimum native header → machine → InvalidElfInterpreter；原租约退休 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execveat/主映像对象类型；x86_64 322；其余三架构 281 | v7.1 [load_elf_binary](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c) | 主映像只接受 ET_EXEC 或 ET_DYN，ET_REL 在程序头读取和映像替换前返回 ENOEXEC | sys_execveat → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → ElfImage::load/Main → fixed header type → InvalidExecutable；未发布映像退休 | 正确 | 同一 test-execve C：Linux 157/157，旧 x86 六项失败，修后四目标各 1/1；本行只涵盖注明字段及原生有效程序头夹具 |
| execveat/解释器对象类型晚期失败；x86_64 322；其余三架构 281 | v7.1 [load_elf_interp](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/binfmt_elf.c)、[exec_binprm](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | ET_REL 解释器在 begin_new_exec 之后被拒绝，调用者终止于 SIGSEGV，不能恢复原映像并返回 ELIBBAD | sys_execveat → do_execve/UserImageBuilder → load_user_app → load_user_app_with_depth/ExecutableFile → load_elf → Interpreter；目前仍先完整构造新 UserImage，晚期失败协议未接入 | 无法确认 | Linux 早期探针两 API 均实际 SIGSEGV；最终回归不包含该输入，尚无 Starry 同条件晚期状态证明 |

验证源码为 710411b5af，最终 C 与旧实现 2a4fa6348b 逐字一致。原私有 inode
旅程实际拒绝输入、恢复 writer 并重新执行 shell；没有新增系统程序或默认
QEMU 启动。标准库 Kernel 一包、两包增量 Clippy 83/83、四目标运行及格式
检查通过，原始失败和早期 Linux 解释器类型探针分别记录。
