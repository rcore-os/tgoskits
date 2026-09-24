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
| `openat2(2)` RESOLVE_* 约束（BENEATH/IN_ROOT/NO_XDEV/NO_SYMLINKS/NO_MAGICLINKS） | ✅ 已强制执行（`RESOLVE_CACHED` 暂不支持 dcache-only 查找、统一返回 EAGAIN；错误优先级对齐 `link_path_walk`） | `fs/ax-fs-ng/src/fs_core/{constraints.rs,context.rs}`、`file/open.rs`、`syscall/fs/fd_ops.rs` |
| `pivot_root(".", ".")` 惯用法（runc/docker 标准 pivot 流程） | ✅ 已支持（old root 堆叠于新根 `/`，`umount2(".", MNT_DETACH)` 收尾） | `fs/axfs-ng-vfs/src/mount/mod.rs` `pivot_mount`、`syscall/fs/mount.rs` |
| cgroup v2 设备控制器 `bpf(2)`（`BPF_PROG_TYPE_CGROUP_DEVICE` / `BPF_CGROUP_DEVICE`） | ❌ 不实现，整族显式返回 `EOPNOTSUPP`，不伪造设备策略生效；非 rootless runc 的探针据此禁用设备过滤 | `os/StarryOS/kernel/src/ebpf/device_controller.rs` |
| `/proc/<pid>/exe` magic link 直连后备文件 | ✅ 已实现（memfd 执行显示 `/memfd: (deleted)` 也能打开）；跨进程打开按 `PTRACE_MODE_READ_FSCREDS` 校验 fsUID/fsGID 三元组，非 dumpable 目标需 `CAP_SYS_PTRACE` | `syscall/fs/fd_ops.rs` `try_open_proc_exe` + `task/process_image.rs` `exe_location` |
| `prctl` PDEATHSIG / NO_NEW_PRIVS | ✅ 已实现 | `syscall/task/ctl.rs:407,578` |
| `copy_file_range` | ✅ 有真实实现；syscall 走 async 运行时（`block_on`/`poll_io`），无 Go netpoll M 线程阻塞风险 | `syscall/fs/io.rs:841` |
| ext4（`rsext4`）含 jbd2 | ✅ 可用（无 credit 机制，不存在 journal 中毒类问题） | `fs/rsext4` |

结论：**容器 ABI（命名空间、cgroup v2 接口、seccomp、capabilities、ptrace、进程/文件系统语义）层已经齐备**，多数功能比历史内核实现更完整。

## 3. 已知缺口（阻塞默认 docker 运行）

### 3.1 内核缺陷：ELF 加载校验缺失（已在本分支修复）

> 本分支（`debin/docker-startup`）已把 `kernel-elf-parser` vendor 到 `components/kernel-elf-parser` 并完成以下两项修复（4 架构自适应），详见该 crate 的提交与测试。

1. ~~execve 不校验 `e_machine`~~ **已修复**：`ELFHeadersBuilder::new` 校验 `e_machine`，非当前架构（aarch64/x86_64/riscv64/loongarch64 按 `target_arch` 适配）返回 `Err("unsupported ELF machine")`，经 `loader.rs` 映射为 `ENOEXEC`。

2. ~~无 PT_PHDR 覆盖程序头表的 ELF 触发内核 panic~~ **已修复**：`phdr()` 改为返回 `Option<usize>`，`aux_vector()` 仅在存在覆盖 `PT_PHDR` 的段时发射 `AT_PHDR`，不再 `expect` panic。

> 两处均移植自 x-kernel `boot/kernel_elf_parser` 的对应修复（x-kernel 提交 `c33a5c481`、`1491fd9c7`），并改为四架构自适应；新增单元测试 `test_machine_guard.rs` 覆盖异构机器拒绝与无 `PT_PHDR` 场景。

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

### Phase 0 — 内核补丁（已完成，见 3.1）

- [x] `e_machine` 校验：非当前架构 ELF 返回 `ENOEXEC`。
- [x] `AT_PHDR` 可选化：消除 `phdr()` 的 `expect` panic。
- [x] 验证：`cargo test -p kernel-elf-parser` 全绿 + `cargo xtask starry build`（aarch64）全量内核构建通过。

验收：arm64 guest 内 `execve` 一个 amd64 ELF 返回 126（shell 场景）而非内核崩溃；无 PT_PHDR 工具链二进制可正常启动。

### Phase 1 — guest 最小运行环境（已完成：`qemu/docker-guest-env`）

- [x] rootfs：Debian arm64 用户态；`/proc`、`/sys`、`/dev` 挂载，`/sys/fs/cgroup` 预建 cgroup2 挂载点。
- [x] 验证 `/proc/self/ns/*` 可读、`unshare` / `setns` 回退、`mount -t tmpfs`、`/dev/ptmx`、AF_UNIX `SCM_RIGHTS` 传递。
- [x] 配套内核修复：`/proc/filesystems` 补齐 ramfs/devpts/cgroup2/overlay、cgroup2 经 `fsopen` 挂载、net/ipc ns id 从 1 起始、axbuild 为 runtime-only rootfs 用例提取受管 Alpine 工具链 sysroot。

验收：`cargo xtask starry test qemu --arch aarch64 -c qemu/docker-guest-env` 全绿。

### Phase 2 — runc 单容器（已完成：`qemu/docker-runc-run`）

- [x] 静态部署官方 runc 1.1.15（构建时下载 + sha256 钉死）+ busybox OCI bundle（空 capabilities、pid/mount/uts/ipc ns、仅 /proc 挂载）。
- [x] 补齐 runc 依赖的内核缺口：starttime 渲染、`oom_score_adj` NUL 结尾写入、memfd 0777、匿名 fd `fchown/fchmod`、`/proc/<pid>/exe` magic link 直连后备文件、`bpf(2)` cgroup-device 命令显式拒绝（`EOPNOTSUPP`，不伪造设备策略生效）、`pivot_root(".", ".")`、`openat2` RESOLVE_* 约束强制（见 §2 表格）。
- [x] `runc run` busybox 容器：`echo`、退出码传播、uts/pid/mount 隔离生效；Stage B 启用 cgroups 后 `pids.max=2` 超限 `fork` 返回 EAGAIN 可观察。

验收：`cargo xtask starry test qemu --arch aarch64 -c qemu/docker-runc-run` 全绿（`DOCKER_RUNC_RUN_PASSED` + `DOCKER_RUNC_RUN_STAGE_B_OK`）。

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
| 内核 | cgroup pids 限制 | `pids.max` 超限 fork 返回 EAGAIN，计数平衡 |
| 内核 | seccomp FILTER | 白名单外 syscall 返回 `EPERM`/`SIGSYS` |
| runc | busybox `runc run`（`qemu/docker-runc-run`） | init 起停正确，ns 隔离可见，pids.max EAGAIN 可观察 |
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
| ELF 两个缺陷 | dockerd 启动崩溃/panic | Phase 0 先行修复（可移植 x-kernel 补丁） |
| 未知 syscall 缺口 | runc/containerd 中途失败 | 按内核 unimplemented 日志逐项补齐，每个缺口带独立验证 |
| 性能（vfs 驱动 + host 网络） | 不适合生产负载 | 依赖 overlayfs/iptables 增量后缓解 |

## 7. 参考

- x-kernel docker bring-up 补丁清单（23 个提交，可直接对照移植）：路径见 x-kernel 仓库 `debin/bug-fix2` 分支。
- 仓库既有设施：`components/guest-ip-protocol`（guest 网络链路）、`os/axvisor`（客户机虚拟化）、离线资产注入工具（`tools/`、`uapps/docker-offline` 为 x-kernel 侧实现，可参考改造）。
- Linux 语义基准：v6.12 内核手册页（unshare/setns/pivot_root/mount/clone3/seccomp）。