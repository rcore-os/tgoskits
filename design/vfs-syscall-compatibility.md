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
| openat2/CACHED；四架构 437 | v7.1 [build_open_flags](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1293) | create/truncate/tmpfile 组合先返回 EAGAIN；其他请求只用有效缓存，需慢路径时 EAGAIN，有效负项可为 ENOENT | sys_openat2 校验变更类组合后，普通 CACHED 仍返回 OperationNotSupported；未进入阻塞 FsView | 不正确 | 完整非阻塞凭据、scope、fd、缓存、元数据、拓扑和最终转交尚未实现；不将拒绝所有请求算作完成 |

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
| umount2；x86_64 166；其余三架构 39 | v7.1 [path_umount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2035) | 普通卸载保留目标到拆树提交，其他实际使用者导致 EBUSY；expire、lazy detach 另有顺序和寿命 | sys_umount2 → owned target → retire_executable_cache → PathHandle.prepare_unmount → cache flush/busy → PathUnmount.commit；挂载树为 namespace 状态 | 部分正确 | x86 path-lookup 的 tmpfs 普通卸载通过；公开 AX 旅程证明活动查找结果 busy、释放后可卸载；271ba070e1 原 executable 程序四架构证明存活 busy、退出后可卸载；expire 与 F1-F8 授权未闭合 |
| mount；x86_64 165；其余三架构 40 | v7.1 [mount](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 解析并保持来源、目标到相应 mount/bind/move 提交，按实际 namespace 解释授权 | sys_mount → FsView → owned target/source；loop Direct 与 Overlay lower/upper/work 保存原 PathHandle；MountNamespace 持拓扑 | 部分正确 | x86 私有 tmpfs 安装和跨 mount 限制通过；cgroup/devpts 与 readonly 重配置的独立语义见第 8 节；F1-F8 及完整多操作数视图继续处理 |
| pivot_root；x86_64 155；其余三架构 41 | v7.1 [pivot_root](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4759) | new_root 与 put_old 按规定顺序解析并保持到拓扑事务及 root/cwd 修订 | sys_pivot_root → one saved FsBindingSnapshot → owned directories → NamespaceChange.pivot_root → FsContextSnapshot.pivot_root | 无法确认 | 当前迁移通过类型检查；既有组件 pivot 证据不能替代本阶段直接 syscall 的并发及错误顺序证明 |
| chdir；x86_64 80；其余三架构 49 | v7.1 [chdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L550) | 跟随路径、检查目录及 search 后更新共享 fs 的 cwd，保留已取得目录 | sys_chdir → saved binding/FsView.resolve_checked → NamespaceChange.prepare_cwd(PathHandle) → commit；FsContextState 保存 cwd | 部分正确 | x86 path-lookup 的 cwd 切换、挂载穿越及恢复通过；共享 CLONE_FS、并发根切换与其余目标仍需直接证明 |
| fchdir；x86_64 81；其余三架构 50 | v7.1 [fchdir](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L571) | 目录 fd 的实际对象经 search 检查后成为 cwd，目录改名不改其身份 | sys_fchdir → retained Directory.path_handle.clone → NamespaceChange.prepare_cwd → commit | 无法确认 | 本阶段未新增或选择直接 fchdir 的完整旅程；拥有型迁移不能单独作为系统兼容证明 |
| chroot；x86_64 161；其余三架构 51 | v7.1 [chroot](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L588) | 路径及 search 检查后核对 CAP_SYS_CHROOT，再更新共享 fs root，cwd 独立 | sys_chroot → saved binding/FsView → owned directory → NamespaceChange.prepare_root → commit | 无法确认 | 当前所有权转交已迁移；本阶段没有直接 chroot 的完整参数、权限及共享状态红绿证明 |

### 4.2 执行映像

装载入口消费拥有型路径，保持主映像、脚本解释器及 PT_INTERP 的实际对象。执行访问租约、页缓存和 MM 映射有各自寿命，完整退出与缓存退休仍需用真实 executable 卸载旅程判定。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| execve；x86_64 59；其余三架构 221 | v7.1 [do_open_execat](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L764) | 主映像与解释器保留实际对象，proc fd 路径必须经过真实挂载与链接授权，失败释放执行访问 | sys_execve → one FsView.resolve → load_user_app(PathHandle, FsView) → shebang/PT_INTERP resolve in supplied view → ExecutableFile/ELF_CACHE/CachedFile → AddrSpace | 部分正确 | 09ccf6cb2c 的 x86 完整 system 541/541、原 exec family 程序及私有解释器内核测试通过；同一 C 在 Linux 82/0，旧前缀分流两项失败；CLONE_FS 受控并发与 ELF 缓存写后失效仍未闭合 |
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
| symlinkat；x86_64 266；其余三架构 36 | v7.1 [filename_create](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L4939) | 按新名称的 dirfd 解析父目录，保存文字并授权实际创建槽位 | sys_symlinkat → with_fs → FsView.symlink → typed creation_name → backend | 部分正确 | 原完整权限程序覆盖 symlinkat；完整错误优先级仍未穷尽 |
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
| ioctl/MODE_SETCRTC 重新启用；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_vblank_on_config](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L1482) | 模式切换重设时间锚并增加一个序号；关闭到启用之间的时间不纳入新纪元计数 | sys_ioctl → retained KapiFile → FileOperations.unlocked_ioctl → drm_ioctl → drm_mode_setcrtc → atomic simple CRTC enable → drm_crtc_vblank_on_config → DrmVblankCrtc count/time | 部分正确 | 9b8e29fd84 的真实核心夹具关闭 100 个周期后验证 reset 增量 1；a8cc3944a1 的 LoongArch 原完整 DRM 程序 1/1；其余目标和真实 Linux DRM 运行对照未在本阶段闭合 |

### 11.2 首次查询

`CRTC_GET_SEQUENCE` 可以为没有引用的活动 CRTC 启用计时器，返回一致的序号与
边沿时间。无硬件计数器时，补算活动帧数采用最近整数取整。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| ioctl/CRTC_GET_SEQUENCE 首次活动查询；x86_64 16；其余三架构 29 | v7.1 [drm_crtc_get_sequence_ioctl](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L2006) 与 [drm_update_vblank_count](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/gpu/drm/drm_vblank.c#L339) | 按实际 CRTC 取得引用并读取一致的 count/time；时间戳推导帧数四舍五入，不能用向下取整的耗时上界拒绝合法结果 | sys_ioctl → KapiFile.ioctl → drm_ioctl → drm_crtc_get_sequence_ioctl → vblank_get/enable → drm_update_vblank_count → DrmVblankCrtc seqlock | 部分正确 | 原引用旅程精确断言 3/4 周期的活动时间得到总增量 2 及对应时间戳；实际 C 判据同一回放在 8d412dbfe7 失败、a8cc3944a1 通过，修后仍拒绝增量 102 和序号回退；LoongArch ioctl 程序 1/1，其他目标和真实 Linux DRM 运行对照仍有缺口 |

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
