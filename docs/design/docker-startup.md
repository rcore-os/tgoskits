# StarryOS (tgoskits) Docker 启动方案

## 1. 目标与范围

本文档给出在 X-Kernel 的 StarryOS 内核（tgoskits 仓库）客户机中**启动 docker 容器栈**的可执行路径与现状基线。

理解目标为：`dockerd → containerd → runc → 容器 init` 全链路在 StarryOS guest 内可用，容器进程可以以独立 PID/NET/MNT 命名空间 + cgroup + seccomp 约束运行，并能从预置镜像启动一个实际工作负载。

本方案的非目标（第一版不做）：

- Docker bridge 网络与端口发布（依赖 netfilter/iptables，当前内核未实现）；
- overlayfs（overlay2）存储驱动（内核当前只有 ext4/fat）；
- 构建（buildkit/docker build）与镜像仓库拉取（离线资产注入）；
- 用户命名空间内的非特权容器、cgroup v2 完整控制器（cpu/memory/cpuset）。

这些能力需要在后续增量中单独落地，各自有独立的验收标准。

## 2. 现状基线：已具备的容器 ABI 能力

以下结论基于对 `os/StarryOS/kernel`、`net/ax-net`、`fs/ax-fs-ng`、`components/ax-cgroup` 的代码核查（分支 `debin/docker-startup` 对应 `origin/dev` @ `eaa72c52b`）。

| 能力 | 状态 | 代码位置 |
| --- | --- | --- |
| PID/MNT/UTS/IPC/USER/CGROUP 命名空间 | ✅ 已实现 | `os/StarryOS/kernel/src/namespace/{pid 语义经 task、mnt.rs、uts.rs、ipc.rs、user.rs、cgroup.rs}` |
| 网络命名空间（per-process） | ✅ 已实现 | `namespace/net.rs`（NetNamespace、初始 ns 仅 loopback） |
| `unshare(2)` / `setns(2)` | ✅ 已实现（NsFd + PidFd 双通道，cap 校验） | `syscall/task/namespace.rs` |
| `/proc/<pid>/ns/<type>` 文件 | ✅ 已实现（inode=ns id，可 bind-mount） | `pseudofs/proc.rs` `"ns"` `SimpleDir::new_maker` |
| PID 命名空间身份（每层 ns 一个号） | ✅ 已实现 | `task/pid.rs`（`starry-pid-namespace-identity.md`） |
| cgroup v2 接口 + pids 控制器 | ✅ 已实现（controllers/subtree_control/pids.max 等） | `components/ax-cgroup`、`kernel/src/cgroup/mod.rs`、`sysfs.rs` 预建 `/sys/fs/cgroup` 挂载点 |
| seccomp（STRICT/FILTER/TSYNC/GET_ACTION_AVAIL） | ✅ 已实现 | `syscall/sys.rs:981` |
| capabilities（CAP_SYS_ADMIN/CAP_CHOWN 等校验） | ✅ 已实现 | `syscall/fs/mount.rs`、`syscall/task/ctl.rs`、`namespace.rs` 等 |
| ptrace | ✅ 已实现 | `syscall/task/ptrace.rs` |
| `clone3` / `pidfd_open` / `waitid(P_PIDFD)` | ✅ 已实现 | `syscall/task/clone.rs`、`syscall/fs/pidfd.rs`、`syscall/task/wait.rs:129` |
| `execveat(AT_EMPTY_PATH)` + memfd 执行 | ✅ 已实现（`/memfd:<name> (deleted)` 显示路径） | `syscall/task/execve.rs:116` |
| `pivot_root` + `umount2(MNT_DETACH)` | ✅ 已实现（Linux 语义） | `syscall/fs/mount.rs:870` |
| `mount(MS_SLAVE/MS_PRIVATE/MS_REC)` | ✅ 已实现 | `syscall/fs/mount.rs:53,629` |
| tmpfs | ✅ 已实现（`/dev/shm`/`/run` 可用） | `pseudofs/tmp.rs` |
| devpts（`/dev/ptmx` + `/dev/pts`） | ✅ 已实现 | `pseudofs/dev/mod.rs:551,560` |
| AF_UNIX `SCM_RIGHTS` fd 传递（含 stream） | ✅ 已实现（byte-marked cmsg、MSG_PEEK dup 语义） | `net/ax-net/src/unix/stream.rs` |
| `madvise` 提示字（NORMAL/RANDOM/SEQUENTIAL/WILLNEED） | ✅ 已接受（bbolt 依赖） | `syscall/mm/mmap.rs:997` |
| `epoll_pwait` sigsetsize 条件校验 | ✅ 与 Linux 一致（仅 sigmask 非空时校验） | `syscall/io_mpx/epoll.rs` `do_epoll_wait` |
| `/proc/<pid>/stat` starttime | ✅ 已填充（`ThreadAccounting::start_time_ns` 捕获，渲染为 ticks，单测覆盖） | `task/stat.rs`、`task/thread.rs` |
| `openat2(2)` RESOLVE_* 约束（BENEATH/IN_ROOT/NO_XDEV/NO_SYMLINKS/NO_MAGICLINKS） | ✅ 已强制执行（`RESOLVE_CACHED` 暂不支持 dcache-only 查找，任何携带该标志的打开统一返回 EAGAIN，调用方按 openat2(2) 约定去掉该标志重试；错误优先级对齐 `link_path_walk`） | `fs/ax-fs-ng/src/fs_core/{constraints.rs,context.rs}`、`file/open.rs`、`syscall/fs/fd_ops.rs` |
| procfs magic link（`exe`/`fd/N`/`ns/<type>`）在 `openat2` 空间约束下的跳转 | ⚠️ 有意保守：带 `RESOLVE_BENEATH`/`IN_ROOT`/`NO_XDEV` 时一律返回 `EXDEV`。本内核取不到 magic link 目标的对象级挂载身份，而 procfs link 的目标对象（可执行文件/管道/命名空间 inode）实际位于与 procfs 不同的挂载，Linux `nd_jump_link()` 对这种受限跳转同样返回 `EXDEV`；仅当目标与链接同挂载时 Linux 放行，本内核无此情形 | `fs/ax-fs-ng/src/fs_core/context.rs` `try_resolve_symlink_constrained` |
| `pivot_root(".", ".")` 惯用法（runc/docker 标准 pivot 流程） | ✅ 已支持（old root 堆叠于新根 `/`，`umount2(".", MNT_DETACH)` 收尾） | `fs/axfs-ng-vfs/src/mount/mod.rs` `pivot_mount`、`syscall/fs/mount.rs` |
| cgroup v2 设备控制器 `bpf(2)`（`BPF_PROG_TYPE_CGROUP_DEVICE` / `BPF_CGROUP_DEVICE`） | ❌ 不实现，整族显式返回 `EOPNOTSUPP`，不伪造设备策略生效；非 rootless runc 的探针据此禁用设备过滤 | `os/StarryOS/kernel/src/ebpf/device_controller.rs` |
| `/proc/<pid>/exe` magic link 直连后备文件 | ✅ 已实现（memfd 执行显示 `/memfd: (deleted)` 也能打开）；跨进程打开按 `PTRACE_MODE_READ_FSCREDS` 校验 fsUID/fsGID 三元组，非 dumpable 目标需 `CAP_SYS_PTRACE` | `syscall/fs/fd_ops.rs` `try_open_proc_exe` + `task/process_image.rs` `exe_location` |
| `prctl` PDEATHSIG / NO_NEW_PRIVS | ✅ 已实现 | `syscall/task/ctl.rs:407,578` |
| `copy_file_range` | ✅ 有真实实现；syscall 走 async 运行时（`block_on`/`poll_io`），无 Go netpoll M 线程阻塞风险 | `syscall/fs/io.rs:841` |
| ext4（`rsext4`）含 jbd2 | ✅ 可用（无 credit 机制，不存在 journal 中毒类问题） | `fs/rsext4` |

结论：**容器 ABI（命名空间、cgroup v2 接口、seccomp、capabilities、ptrace、进程/文件系统语义）层已经齐备**，多数功能比历史内核实现更完整。

## 3. 已知缺口（阻塞默认 docker 运行）

### 3.1 内核缺陷：ELF 加载校验缺失（未落地）

> 历史分支 `debin/docker-startup` 曾把 `kernel-elf-parser` vendor 到 `components/kernel-elf-parser` 并完成下述两项修复（移植自 x-kernel `boot/kernel_elf_parser` 的提交 `c33a5c481`、`1491fd9c7`，四架构自适应，含 `test_machine_guard.rs` 单测）；该 vendor 与测试**不在本仓库，也不在本 PR**，当前分支没有这些文件。

当前内核经 crates.io 依赖发布的 `kernel-elf-parser 0.3.4`，该版本**不包含**这两项修复：

1. `e_machine` 无校验：非当前架构 ELF 的行为未定义（不保证 `ENOEXEC`）。
2. `phdr()`（`info.rs`）在找不到覆盖程序头表的段时 `expect` panic，且 `aux_vector()` 无条件发射 `AT_PHDR`——无 `PT_PHDR` 覆盖的 ELF 仍可能触发内核 panic。

两项修复仍以待实现项记录：把 x-kernel 的两个提交适配为四架构版本，向上游发布新版本或在内核 `mm/loader.rs` 增加等价校验，并补 `test_machine_guard.rs` 风格的回归。

### 3.2 存储：无 overlayfs

`fs/ax-fs-ng/src/fs` 仅提供 ext4/fat。dockerd/containerd 默认 overlay2 snapshotter 不可用。

- 本方案第一版使用 **dockerd `--storage-driver=vfs`**（containerd 对应 `native` snapshotter）：可用、语义简单，但慢、占空间。
- 后续增量：在 ax-fs-ng 增加 overlayfs（upper/lower/merged 层叠）后切回 overlay2。

### 3.3 网络：无 netfilter/iptables

内核无 netfilter/iptables/网桥/veth。Docker bridge 网络、端口发布、NAT 均不可用。

- 本方案第一版使用 **`--network=host`**（容器共享客户机网络栈）或 `--network=none`。
- 后续增量：netfilter 表 + veth/bridge 设备，或接受"仅 host 网络"作为交付边界。

### 3.4 无仓库内 docker 验证记录

当前 tgoskits 无 docker/containerd/runc 在 guest 内实际跑通的记录。第 5 节的验证矩阵即为补上该证据的入口。

## 4. 分阶段启动路径

### Phase 0 — 内核补丁（未完成，见 3.1）

- [ ] `e_machine` 校验：非当前架构 ELF 返回 `ENOEXEC`。
- [ ] `AT_PHDR` 可选化：消除 `phdr()` 的 `expect` panic。
- [ ] 验证：`cargo test -p kernel-elf-parser` 覆盖异构机器拒绝与无 `PT_PHDR` 场景 + 全量内核构建。

验收（未取得）：arm64 guest 内 `execve` 一个 amd64 ELF 返回 126（shell 场景）而非内核崩溃；无 PT_PHDR 工具链二进制可正常启动。本 PR 的两个 Docker 用例不触达这两类输入，不能作为本项证据。

### Phase 1 — guest 最小运行环境（已完成：`qemu-docker/docker-guest-env`）

- [x] rootfs：Debian arm64 用户态；`/proc`、`/sys`、`/dev` 挂载，`/sys/fs/cgroup` 预建 cgroup2 挂载点。
- [x] 验证 `/proc/self/ns/*` 可读、`unshare` / `setns` 回退、`mount -t tmpfs`、`/dev/ptmx`、AF_UNIX `SCM_RIGHTS` 传递。
- [x] 配套内核修复：`/proc/filesystems` 补齐 ramfs/devpts/cgroup2/overlay、cgroup2 经 `fsopen` 挂载、net/ipc ns id 从 1 起始、axbuild 为 runtime-only rootfs 用例提取受管 Alpine 工具链 sysroot。

验收：`cargo xtask starry test qemu --arch aarch64 -c qemu-docker/docker-guest-env` 全绿。

### Phase 2 — runc 单容器（已完成 Stage A：`qemu-docker/docker-runc-run`）

- [x] 静态部署官方 runc 1.1.15（构建时下载 + sha256 钉死）+ busybox OCI bundle（空 capabilities、pid/mount/uts/ipc ns、仅 /proc 挂载）。
- [x] 补齐 runc 依赖的内核缺口：starttime 渲染、`oom_score_adj` NUL 结尾写入、memfd 0777、匿名 fd `fchown/fchmod`、`/proc/<pid>/exe` magic link 直连后备文件、`bpf(2)` cgroup-device 命令显式拒绝（`EOPNOTSUPP`，不伪造设备策略生效）、`pivot_root(".", ".")`、`openat2` RESOLVE_* 约束强制（见 §2 表格）。
- [x] `runc run` busybox 容器（Stage A，rootless）：`echo`、退出码传播、uts/pid/mount 隔离生效。
- [ ] Stage B（cgroups 启用后 `pids.max=2` 超限 `fork` 返回 EAGAIN）：本内核不实现 cgroup 设备控制器，非 rootless runc 的 cgroup v2 设备策略初始化失败，因此 Stage B 暂不可执行、由用例显式跳过；待实现真实设备控制器后再作为验收项。

验收：`cargo xtask starry test qemu --arch aarch64 -c qemu-docker/docker-runc-run` 全绿（Stage A；成功标记 `DOCKER_RUNC_RUN_STAGE_A_ONLY_PASSED`，Stage B 跳过）。

### Phase 3 — containerd（ctr 验证）

- [ ] 部署 containerd，`native` snapshotter + 预导入镜像（`ctr images import` 离线 tar）。
- [ ] `ctr run` 拉起容器，验证 shim（ttrpc over AF_UNIX SCM_RIGHTS）路径。

验收：ctr 能列出/启动/停止容器；shim 生命周期完整（无 `ttrpc: closed` 类错误）。

### Phase 4 — dockerd

- [ ] 部署 dockerd + docker CLI，`--storage-driver=vfs --iptables=false --bridge=none`，`DOCKER_HOST` 就绪。
- [ ] `docker load` 预置镜像（离线 tar 注入，参考仓库现有 guest-ip/离线资产工具链）。
- [ ] `docker run --network=host <image> <cmd>` 实际工作负载。
- [ ] `docker ps` / `docker logs` / `docker exec` 基本生命周期操作。

验收：dockerd 无 `Unimplemented` 服务；容器进程在独立 PID 命名空间 + cgroup + seccomp 约束下运行；`docker run hello` 可复现。

## 5. 验证矩阵

| 层 | 验收项 | 判定标准 |
| --- | --- | --- |
| 内核 | amd64 ELF exec | 返回 ENOEXEC，无崩溃 |
| 内核 | 无 PT_PHDR ELF exec | 正常加载，无 panic |
| 内核 | `unshare`/`setns` 全部 6 类 ns | 语义与 Linux 一致，EPERM/EINVAL 路径正确 |
| 内核 | cgroup pids 限制 | Stage B 暂不可执行（设备控制器未实现，非 rootless runc cgroup 设备策略初始化失败） |
| 内核 | seccomp FILTER | 白名单外 syscall 返回 `EPERM`/`SIGSYS` |
| runc | busybox `runc run`（`qemu-docker/docker-runc-run`） | Stage A：init 起停正确，ns 隔离可见；Stage B（pids.max EAGAIN）因设备控制器未实现而跳过 |
| runc | 内核语义探针（starttime/oom NUL/memfd/pipe fchown/bpf 设备控制器拒绝） | `docker-runc-run-probe` 全部断言通过 |
| 内核 | openat2 RESOLVE_*（`qemu/system/bugfix-openat2-resolve-constraints`） | 合规路径成功，越界 EXDEV/ELOOP，错误优先级与 Linux 一致 |
| containerd | `ctr run` | shim 生命周期完整 |
| dockerd | `docker run --network=host` | 容器内进程对外可见、日志可回收 |
| dockerd | `docker exec`/`docker ps` | 基本生命周期操作可用 |

## 6. 风险与后续增量

| 风险/缺口 | 影响 | 后续路线 |
| --- | --- | --- |
| 无 overlayfs | 镜像层效率低、空间放大 | ax-fs-ng overlayfs（upper/lower/merged）后切 overlay2 |
| 无 netfilter/iptables | 只能 host/none 网络，无端口发布 | netfilter 框架 + veth/bridge，或把 host 网络固化为交付边界 |
| ELF 两个缺陷 | dockerd 启动崩溃/panic | 待实现：x-kernel 补丁（`c33a5c481`、`1491fd9c7`）四架构适配，见 3.1/Phase 0；本 PR 未包含 |
| 未知 syscall 缺口 | runc/containerd 中途失败 | 按内核 unimplemented 日志逐项补齐，每个缺口带独立验证 |
| 性能（vfs 驱动 + host 网络） | 不适合生产负载 | 依赖 overlayfs/iptables 增量后缓解 |

## 7. 参考

- x-kernel docker bring-up 补丁清单（23 个提交，可直接对照移植）：路径见 x-kernel 仓库 `debin/bug-fix2` 分支。
- 仓库既有设施：`components/guest-ip-protocol`（guest 网络链路）、`os/axvisor`（客户机虚拟化）、离线资产注入工具（`tools/`、`uapps/docker-offline` 为 x-kernel 侧实现，可参考改造）。
- Linux 语义基准：v6.12 内核手册页（unshare/setns/pivot_root/mount/clone3/seccomp）。