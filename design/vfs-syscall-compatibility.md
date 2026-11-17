# 文件系统系统调用兼容性记录

## 1. 基准与证据

本记录随所有权迁移逐入口补齐。当前范围是接续记录第 9.31 至 9.33 节的 scoped 查找、proc 自指链接和拥有型结果，其他元数据、命名、挂载、凭据、映射与套接字入口继续按主计划核对。每行结论仅覆盖注明的参数与对象范围。

### 1.1 固定源码

Linux 基准为 v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。编号分别核对该版本的 x86 `syscall_64.tbl`、通用 `scripts/syscall.tbl` 和架构选择；Starry 核对 `syscall/mod.rs` 的真实分派。AArch64、RISC-V 与 LoongArch64 的通用 64 位入口采用 56/openat、61/getdents64、78/readlinkat、79/newfstatat、80/fstat、291/statx、437/openat2；Starry 对 79 的 Rust 枚举命名因目标而异。

### 1.2 运行条件

当前拥有型迁移源码为 `802004f30c`，测试夹具提交为 `272141b52a`。最终 18 包标准库和 AX/runtime/Kernel 的 106 项 Clippy 通过；x86_64 的 ownership、路径和 proc 程序分别为 107/107、45/45 和 1/1。以同一 ELF 运行完整 system 为 534/541，通过范围与七个失败分别保留。

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
| openat2/CACHED；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1293) | create/truncate/tmpfile 组合先返回 EAGAIN；其他请求只用有效缓存，需慢路径时 EAGAIN，有效负项可为 ENOENT | sys_openat2 校验变更类组合后，普通 CACHED 仍返回 OperationNotSupported；未进入阻塞 FsView | 不正确 | 完整非阻塞凭据、scope、fd、缓存、元数据、拓扑和最终转交尚未实现；不将拒绝所有请求算作完成 |

### 2.2 普通打开与链接句柄

普通打开沿用保存的 caller，但原有 self fd reopen 等消费者仍未全部完成对象式转交。自指链接作为普通文本 symlink 参与原会话的总预算。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| open；x86_64 2 | v7.1 [do_sys_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 普通路径参与同一总链接预算，跟随和创建由 flags 决定 | syscall/mod.rs → sys_open → sys_openat → with_fs → OpenOptions → LookupSession；仍有 self fd reopen 特例 | 部分正确 | x86 完整路径旅程含 legacy open 的 40/41 总预算和悬空创建；其余消费者继续迁移 |
| openat；x86_64 257；其余三架构 56 | v7.1 [do_sys_open](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c) | 绝对输入忽略 dirfd；O_PATH + O_NOFOLLOW 获取 self/thread-self 链接自身 | sys_openat → flags_to_options → FsView/LookupSession → add_to_fd；保存 caller 和实际 PathHandle | 部分正确 | x86、AArch64 的 self/thread-self 链接 fd 通过；x86 路径预算和创建通过；其他目标与 self fd reopen 的完整迁移待补 |
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

普通卸载把本次目标消费为 `PathUnmount`，只排除该操作实际持有的使用权。完整 system 批次仍观察到 bind-mounted executable 退出后卸载返回 EBUSY，须继续区分活动消费者与内部缓存；不能通过放宽 busy 判定规避。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| umount2；x86_64 166；其余三架构 39 | v7.1 [path_umount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2035) | 普通卸载保留目标到拆树提交，其他实际使用者导致 EBUSY；expire、lazy detach 另有顺序和寿命 | sys_umount2 → FsView.resolve → PathHandle.prepare_unmount → cache flush/busy → PathUnmount.commit；挂载树为 namespace 状态 | 部分正确 | x86 path-lookup 的 tmpfs 普通卸载通过；公开 AX 旅程证明活动查找结果 busy、释放后可卸载；bind executable 退出后仍 EBUSY，expire 与 F1-F8 授权未闭合 |
| mount；x86_64 165；其余三架构 40 | v7.1 [mount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 解析并保持来源、目标到相应 mount/bind/move 提交，按实际 namespace 解释授权 | sys_mount → FsView → owned target/source；loop Direct 与 Overlay lower/upper/work 保存原 PathHandle；MountNamespace 持拓扑 | 部分正确 | x86 私有 tmpfs 安装和跨 mount 限制通过；完整 system 中 cgroup/devpts 实例语义仍失败，F1-F8 及完整多操作数视图继续处理 |
| pivot_root；x86_64 155；其余三架构 41 | v7.1 [pivot_root](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4759) | new_root 与 put_old 按规定顺序解析并保持到拓扑事务及 root/cwd 修订 | sys_pivot_root → one saved FsBindingSnapshot → owned directories → NamespaceChange.pivot_root → FsContextSnapshot.pivot_root | 无法确认 | 当前迁移通过类型检查；既有组件 pivot 证据不能替代本阶段直接 syscall 的并发及错误顺序证明 |
| chdir；x86_64 80；其余三架构 49 | v7.1 [chdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L550) | 跟随路径、检查目录及 search 后更新共享 fs 的 cwd，保留已取得目录 | sys_chdir → saved binding/FsView.resolve_checked → NamespaceChange.prepare_cwd(PathHandle) → commit；FsContextState 保存 cwd | 部分正确 | x86 path-lookup 的 cwd 切换、挂载穿越及恢复通过；共享 CLONE_FS、并发根切换与其余目标仍需直接证明 |
| fchdir；x86_64 81；其余三架构 50 | v7.1 [fchdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L571) | 目录 fd 的实际对象经 search 检查后成为 cwd，目录改名不改其身份 | sys_fchdir → retained Directory.path_handle.clone → NamespaceChange.prepare_cwd → commit | 无法确认 | 本阶段未新增或选择直接 fchdir 的完整旅程；拥有型迁移不能单独作为系统兼容证明 |
| chroot；x86_64 161；其余三架构 51 | v7.1 [chroot](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L588) | 路径及 search 检查后核对 CAP_SYS_CHROOT，再更新共享 fs root，cwd 独立 | sys_chroot → saved binding/FsView → owned directory → NamespaceChange.prepare_root → commit | 无法确认 | 当前所有权转交已迁移；本阶段没有直接 chroot 的完整参数、权限及共享状态红绿证明 |

### 4.2 执行映像

装载入口消费拥有型路径，保持主映像、脚本解释器及 PT_INTERP 的实际对象。执行访问租约、页缓存和 MM 映射有各自寿命，完整退出与缓存退休仍需用真实 executable 卸载旅程判定。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve；x86_64 59；其余三架构 221 | v7.1 [do_execveat_common](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | 选定执行文件及解释器后，以实际 file/path 保持映像身份，退出后释放活动执行引用 | sys_execve → FsView.resolve → load_user_app(PathHandle) → ExecutableFile/ElfLoader/CachedFile → AddrSpace executable 与映射 | 部分正确 | x86 proc-pid-exe 的普通、bind 与 unlinked 对象检查通过；bind 执行文件退出后卸载仍 EBUSY，内部缓存退休继续处理 |
| execveat；x86_64 322；其余三架构 281 | v7.1 [execveat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c) | dirfd、AT_EMPTY_PATH 与 nofollow 决定实际执行对象及错误，保留描述和路径至装载 | sys_execveat → resolve_at 或 retained memfd path_handle → load_user_app → loader/MM | 无法确认 | 已迁移实际 PathHandle 消费；本阶段没有直接覆盖所有 fd 类型、flags 及错误优先级的系统回归 |
