# StarryNixOS Stage-2 实现进度

> 更新日期：2026-08-02
> 当前目标：StarryOS/x86_64 + NixOS-generated system closure + systemd stage 2
> 当前结论：已进入 NixOS activation 并启动 systemd，但尚未到达
> `multi-user.target` 已真实到达；`ramfs`、`statx` mount-root 与 mount-ID
> 分歧均已完成红绿修复。严格成功门槛尚未通过：当前首分歧是 systemd
> 创建根 cgroup `/` 返回 `EPERM`，导致服务进程无法 spawn，marker 未执行。

## 1. 定位与范围

本轮实现采用 x86_64-first。它覆盖一个独立、锁定、可验证的
NixOS userspace personality，不替换 StarryOS 内核，也不声称完整 NixOS
兼容。早期文档中的 aarch64-first 顺序已被本轮用户决策覆盖；aarch64
属于后续独立验证范围。

当前启动链为：

```text
QEMU q35/UEFI
  -> StarryOS x86_64 kernel
  -> 独立 NixOS ext4 rootfs
  -> NixOS-generated /init（PID 1）
  -> activation script
  -> systemd（PID 1）
  -> systemd API-filesystem 初始化
  -> systemd unit 加载与依赖事务
  -> multi-user.target（已到达）
  -> [当前阻塞] cgroup service spawn（EPERM）
  -> [尚未执行] starry-nixos-marker.service
```

明确不在本轮范围内：

- NixOS kernel、initrd、bootloader 和安装器；
- guest 内 `nixos-rebuild switch`、generation、reboot generation 和 rollback；
- udev、journald、D-Bus、networkd、resolved、logind 和桌面环境；
- 完整硬件管理、完整服务栈和第二架构；
- 将当前结果描述成完整 NixOS 支持。

## 2. 已完成实现

### 2.1 独立 app 与启动选择

已新增 `apps/starry/nixos/`，并保持 `apps/starry/nix/` 的 Alpine+Nix
路径不变。StarryOS 新增 opt-in `nixos` feature：

- 选中 `nixos` 时执行 NixOS-generated `/init`；
- 未选中时继续使用原有 `/bin/sh` 交互入口；
- 默认 app 行为未全局改成 systemd。

主要文件：

- `os/StarryOS/starryos/Cargo.toml`；
- `os/StarryOS/starryos/src/main.rs`；
- `apps/starry/nixos/build-x86_64-unknown-none.toml`；
- `apps/starry/nixos/qemu-x86_64.toml`。

### 2.2 AppOwned rootfs 边界

Axbuild 已增加显式的 app-owned rootfs preparation 模式：

- 调用 app 声明的 builder；
- 校验目标架构和最终镜像；
- 不复制默认 Alpine rootfs；
- 不执行 APK region 改写；
- 不做 Alpine overlay 注入；
- builder、镜像或目标不匹配时直接失败。

相关 Axbuild 回归覆盖 app-owned 选择、缺失/错误产物拒绝，以及现有
Alpine app 行为保持不变。

### 2.3 锁定的 NixOS 系统与镜像

`apps/starry/nixos/flake.nix` 和 `flake.lock` 生成最小 container-style
NixOS system closure。当前声明包括：

- `boot.isContainer = true`；
- 禁用 NixOS kernel、非必要服务和文档；
- 声明 hostname `starrynixos`；
- 声明 `starry` 用户；
- 声明 `coreutils`、`hello`、`procps`；
- 声明在 `multi-user.target` 后验证系统状态的 marker service/timer。

`build-rootfs.sh` 已实现：

- host architecture/target 检查；
- ext4、closure、activation 数据、system profile 和 provenance 检查；
- Alpine identity、`/etc/apk` 和 APK database 的负向检查；
- 非空候选的原子发布，失败时保留旧镜像；
- 相邻 manifest，绑定 flake lock、closure、systemd、target 和镜像哈希；
- `STARRY_NIXOS_REUSE_ROOTFS=1` 显式复用模式，供无 Nix 的 CI 容器在
  重新验证 ext4、lock、closure、target 和镜像哈希后启动已有镜像；
- 使用基础 `grep`，不要求容器额外提供 ripgrep。

当前已发布产物：

| 项目 | 当前值 |
|---|---|
| Flake lock SHA-256 | `e484df03c41a61badf4c0dddb62ef5c3c1c60a15cfc9e5b78f5477f8e1314ac4` |
| System closure | `/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2` |
| systemd | `260.2` |
| Image SHA-256 | `889eb200dccd74fa6e8e3f43d8fa9e37b586996b87dbef31acc93806aaf4bb55` |
| Starry target | `x86_64-unknown-none` |
| Managed image | `tmp/axbuild/rootfs/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img` |

### 2.4 严格的启动证据

QEMU 只有看到以下完整有序序列才可通过：

```text
STARRY_NIXOS_PHASE=pid1
STARRY_NIXOS_PHASE=activation
STARRY_NIXOS_PHASE=systemd
STARRY_NIXOS_PHASE=marker
STARRY_NIXOS_SYSTEM_PASSED
```

部分序列、failed unit、panic/fatal、显式 failure marker、提前退出和超时
都不能误判为成功。当前真实 app 运行没有产生完整序列，因此最终结果为
失败，未制造假 PASS。

### 2.5 直接基线探针

已增加：

```text
test-suit/starryos/qemu/system/starrynixos-stage2/
```

探针直接验证：

- PID 1 的 `/proc/1/cmdline` 可读；
- `/proc`、`/sys`、`/dev`、`/run` 可见；
- cgroup2 可挂载；
- `cgroup.procs` 可读取并包含进程；
- cgroup2 可卸载。

Podman/QEMU 结果：

```text
STARRY_NIXOS_BASELINE_PROBES_PASSED
STARRY_GROUPED_TESTS_PASSED
```

该探针证明上述内核基础路径可用，但不等价于 NixOS app 已到达
`multi-user.target`。

### 2.6 `/dev/fd` 首个内核兼容修复

初始真实启动在 NixOS stage-2 process substitution 处失败：

```text
/init: line 114: /dev/fd/63: Operation not permitted
```

按“先红测、再修复”增加：

```text
test-suit/starryos/qemu/system/bugfix-dev-fd-symlinks/
```

修复前确定性结果：

```text
PASS: /dev/fd -> /proc/self/fd
PASS: /dev/stdin -> /proc/self/fd/0
PASS: /dev/stdout -> /proc/self/fd/1
PASS: /dev/stderr -> /proc/self/fd/2
FAIL: open /dev/fd/3: errno=2 (No such file or directory)
```

根因不是四个静态链接，而是 `/proc/self/fd/<n>` 对匿名 pipe 的目标只
提供 `pipe:[inode]` 描述，普通 VFS pathname resolver 无法重新打开它。

当前修复：

- devfs 提供 `/dev/fd`、`stdin`、`stdout`、`stderr` 链接；
- `/proc/self/fd/N` 和 `/dev/fd/N` 可重新打开当前进程的 pipe endpoint；
- 新 file description 保留同一 pipe buffer；
- 独立维护 nonblocking 状态；
- 正确增加 reader/writer endpoint 引用，避免提前 EOF/SIGPIPE。

修复后结果：

```text
PASS: dynamic descriptor path /dev/fd/3
STARRY_DEV_FD_SYMLINKS_PASSED
STARRY_GROUPED_TESTS_PASSED
```

### 2.7 Grouped QEMU 测试环境修正

为使聚焦 system subcase 能独立运行：

- Axbuild 将选中的 C subcase 列表通过 `STARRY_GROUPED_C_SUBCASES`
  传给根级 `prebuild.sh`；
- `qemu/system/prebuild.sh` 只在选中 `apk-curl-equivalence` 时安装 curl；
- 避免与 StarryNixOS 无关的 APK 网络准备污染聚焦探针；
- 对该环境变量传递增加了 Axbuild 单元回归。

## 3. 构建和测试环境结论

### 3.1 Native Nix/Lix

NixOS closure 和 ext4 镜像必须在提供 Nix/Lix、gcc、e2fsprogs 的 native
开发环境构建。当前通过 `nix develop` 完成产物构建与发布。

### 3.2 Podman + `.ci-cache`

StarryOS/QEMU 验证采用项目镜像：

```text
ghcr.io/rcore-os/tgoskits-container:latest
```

有效约束：

- 仓库在容器内必须挂载到与宿主相同的绝对路径；生成的
  `tmp/axbuild/.starry.toml` 包含 rootfs 绝对路径，挂载到 `/workspace`
  会使 image registry 路径失效；
- `.ci-cache/{cargo,rustup,target,tmp}` 用于 Rust/QEMU 缓存；
- grouped rootfs extraction 需要 `fakeroot`；项目镜像当前未预装它，测试
  时通过 `.ci-cache/apt/{lists,archives}` 在临时容器安装；
- `--userns=keep-id:uid=0,gid=0` 可让容器内部满足 rootfs 操作，同时宿主
  文件仍归当前用户；Axbuild 仍会因 user namespace 不是完整 identity map
  而使用 `fakeroot`；
- 在宿主 Nix shell 直接编译 grouped C probe 会把 glibc loader 带入 guest；
  Podman 项目环境生成 musl-linked probe，避免 guest `not found` 假象。

## 4. 真实 NixOS app 的当前运行结果

最终 CI-like payload：

```bash
STARRY_NIXOS_REUSE_ROOTFS=1 \
  cargo xtask starry app qemu -t nixos --arch x86_64
```

已观察到：

1. app-owned 镜像通过 manifest 和 ext4 校验；
2. StarryOS 从 NVMe ext4 rootfs 启动；
3. NixOS-generated `/init` 作为 PID 1；
4. stage 2 越过原 `/dev/fd/63` 阻塞；
5. 执行大量 activation 操作，包括 `chown`、`chmod`、`install`、`mount`、
   `mkdir`、`ln`、`mv`、`perl` 和 `/etc` 设置；
6. 输出 `running activation script...`；
7. 后续输出 `starting systemd...`，systemd 实际作为 PID 1 运行。

修复前的首个精确分歧：

```text
mount: /run/keys: unknown filesystem type 'ramfs'.
Activation script snippet 'specialfs' failed (32)
```

activation 继续进行部分后续步骤，但整体返回非零。systemd 随后还报告：

```text
Failed to determine whether /proc is a mount point: Protocol driver not attached
Failed to determine whether /sys is a mount point: Protocol driver not attached
Failed to determine whether /dev is a mount point: Protocol driver not attached
Failed to determine whether /run is a mount point: Protocol driver not attached
Failed to determine whether /sys/fs/cgroup is a mount point: Protocol driver not attached
Failed to mount API filesystems.
Exiting PID 1...
```

按首分歧原则，已增加 `ramfs` Linux oracle 和聚焦红测，未先处理后续
systemd mount-point 检查，也未 mask activation 失败。

聚焦用例：

```text
test-suit/starryos/qemu/system/bugfix-ramfs-mount/
```

CI-like Podman/QEMU 红测结果：

```text
FAIL: mount(2) accepts the ramfs filesystem type: errno=19 (No such device)
STARRY_RAMFS_MOUNT_FAILED: 1 checks
STARRY_GROUPED_TEST_FAILED: one or more SMP system tests failed
```

该用例直接调用 `SYS_mount`，修复后还会验证 `RAMFS_MAGIC`、文件内容
读写和 `umount2`，因此不会因简单把字符串别名到错误 filesystem 身份而误绿。

修复包括：

- `mount(2)` 接受 `ramfs` 并创建独立 filesystem identity；
- `statfs(2)` 返回 Linux `RAMFS_MAGIC`（`0x858458f6`）；
- `ramfs` 与 `tmpfs` 都走内存文件系统的无边界 page-cache 路径；
- 保留现有 readonly 和 mount option 处理；
- `umount2(2)` 清理路径不需要语义修改。

第一次修复后，聚焦测试在 `mount`/`statfs` 通过后触发
`page cache should handle writing` panic；这暴露出 page cache 只按名字识别
`tmpfs`。增加 `tmpfs`/`ramfs` 分类单测并修正后，CI-like Podman/QEMU 绿测为：

```text
PASS: mount(2) accepts the ramfs filesystem type
PASS: statfs(2) reports RAMFS_MAGIC
PASS: create and write a regular file on ramfs
PASS: seek and read back the ramfs file
PASS: umount2(2) detaches ramfs
STARRY_RAMFS_MOUNT_PASSED
STARRY_GROUPED_TESTS_PASSED
```

受影响 crate 验证：

- `cargo test -p ax-fs-ng tmpfs_and_ramfs_use_unbounded_page_cache`：1/1；
- `cargo xtask clippy --package ax-fs-ng`：6/6；
- `cargo xtask clippy --package starry-kernel`：主机已通过前 23 个配置；
  aarch64 system 配置因主机缺少交叉 `gcc` 停止，后续改由项目 Podman
  环境补齐完整矩阵，不能把该环境失败记成代码通过或代码失败。

真实 app 复验确认原 activation 分歧已解除：

```text
Task(25, "activate") exit with code: 0
starting systemd...
```

systemd 随后成为 PID 1。新的首个精确分歧是它判断 API filesystem
挂载点时收到 `EUNATCH`（errno 49，strerror 为
`Protocol driver not attached`）：

```text
Failed to find module 'autofs4'
Failed to determine whether /proc is a mount point: Protocol driver not attached
Failed to determine whether /sys is a mount point: Protocol driver not attached
Failed to determine whether /dev is a mount point: Protocol driver not attached
Failed to determine whether /dev/shm is a mount point: Protocol driver not attached
Failed to determine whether /run is a mount point: Protocol driver not attached
Failed to determine whether /sys/fs/cgroup is a mount point: Protocol driver not attached
Failed to mount API filesystems.
Exiting PID 1...
```

本次真实运行仍正确以失败结束，没有出现 marker 或
`STARRY_NIXOS_SYSTEM_PASSED`。下一步先确认 systemd 在该路径调用的具体
syscall 及 StarryOS 返回 `EUNATCH` 的 owning subsystem，再增加确定性红测。

源码追踪已确认调用链：systemd 260.2 的 `is_mount_point_at()` 调用
`statx(2)`，并把 `STATX_ATTR_MOUNT_ROOT` 作为 mandatory attribute；其
`xstatx_full()` 在 `stx_attributes_mask` 未声明该能力时合成 `EUNATCH`。
StarryOS 当前只填充 `STATX_BASIC_STATS`，把 `stx_attributes_mask` 保持为
零，因此错误并非网络 endpoint 或 mountinfo 解析失败。

已增加聚焦用例：

```text
test-suit/starryos/qemu/system/bugfix-statx-mount-root/
```

它用 systemd 相同的 `AT_NO_AUTOMOUNT | AT_STATX_DONT_SYNC` 调用方式检查
根目录、`/proc`、`/proc/1` 和 `/run`。CI-like Podman/QEMU 修复前红测为：

```text
PASS: statx(2) succeeds for /
FAIL: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /
PASS: statx(2) succeeds for /proc
FAIL: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /proc
PASS: statx(2) succeeds for /proc/1
FAIL: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /proc/1
PASS: statx(2) succeeds for /run
FAIL: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /run
STARRY_STATX_MOUNT_ROOT_FAILED: 6 checks
STARRY_GROUPED_TEST_FAILED: one or more SMP system tests failed
```

Linux oracle 为 `statx(2)`：`stx_attributes_mask` 表示 VFS/filesystem 支持
哪些 attribute，`STATX_ATTR_MOUNT_ROOT` 表示该文件是 mount root。拟修复
边界只在 `statx` 返回值中公布这项能力，并从已解析的 VFS `Location`
判断 attribute 值；不伪造 systemd 特判，也不修改普通 `stat(2)` ABI。

该修复已实现并由同一 Podman/QEMU 用例转绿：

```text
PASS: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /
PASS: statx(2) reports the expected mount-root state for /
PASS: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /proc
PASS: statx(2) reports the expected mount-root state for /proc
PASS: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /proc/1
PASS: statx(2) reports the expected mount-root state for /proc/1
PASS: statx(2) reports STATX_ATTR_MOUNT_ROOT support for /run
PASS: statx(2) reports the expected mount-root state for /run
STARRY_STATX_MOUNT_ROOT_PASSED
STARRY_GROUPED_TESTS_PASSED
```

`/` 与 `/proc` 的 attribute 值为真，普通子目录 `/proc/1` 和未挂载的
`/run` 为假；四者都正确声明该 attribute 受支持。现在需要再次运行真实
StarryNixOS app，确认 systemd 越过该路径并捕获下一条首分歧。

真实 app 复验已确认 systemd 越过 API-filesystem 初始化，并进入 unit
搜索、preset 和默认 target 加载。新的首个精确分歧是安全解析 unit 搜索
路径及 `/etc/systemd/system/*` 符号链接时持续返回 `EUNATCH`，例如：

```text
Failed to resolve symlink /etc/systemd/system/basic.target pointing to
/nix/store/...-systemd-260.2/example/systemd/system/basic.target:
Protocol driver not attached
```

由此造成的终局是：

```text
Unit default.target not found.
Falling back to rescue.target.
Unit rescue.target not found.
Failed to load rescue.target.
Exiting PID 1...
```

这次推进证明 mount-root `EUNATCH` 已解除；新的同 errno 发生在不同调用
路径，不能复用原假设。下一步需要追踪 systemd 260.2 symlink chase 对
`statx` mount ID、`name_to_handle_at` 或 `openat2` 的具体要求，并以最小
符号链接红测锁定。

进一步追踪 systemd 260.2 `chase()` 已确认：它通过 `xstatx_full(...,
XSTATX_MNT_ID_BEST, ...)` 要求 `STATX_MNT_ID_UNIQUE` 或经典
`STATX_MNT_ID`。StarryOS 的 `statx` 尚未填写 mount ID；此外现有
`name_to_handle_at` 错把 filesystem device number 写入 `mount_id`，与
`/proc/self/mountinfo` 使用的 VFS mount ID 不一致。

聚焦用例已扩展 mount-ID 一致性检查，修复前 Podman/QEMU 红测为：

```text
FAIL: statx(2) returns a mount ID for /
FAIL: statx(2) returns a mount ID for /proc
FAIL: statx(2) returns a mount ID for /proc/1
FAIL: statx(2) distinguishes separate mounts
FAIL: statx(2) keeps one mount ID within a mount
FAIL: name_to_handle_at(2) mount ID matches statx(2) for /proc
STARRY_STATX_MOUNT_ROOT_FAILED: 6 checks
STARRY_GROUPED_TEST_FAILED: one or more SMP system tests failed
```

拟修复使用 axfs-ng 已有、namespace-local 的 `Mountpoint::mount_id()`：
`statx.stx_mnt_id`、`name_to_handle_at` 和 mountinfo 将引用同一身份；
filesystem `device()` 继续只用于 `st_dev` 与 mountinfo major:minor。

mount-ID 修复已实现，同一 Podman/QEMU 用例转绿：

```text
PASS: statx(2) returns a mount ID for /
PASS: statx(2) returns a mount ID for /proc
PASS: statx(2) returns a mount ID for /proc/1
PASS: statx(2) distinguishes separate mounts
PASS: statx(2) keeps one mount ID within a mount
PASS: name_to_handle_at(2) mount ID matches statx(2) for /proc
STARRY_STATX_MOUNT_ROOT_PASSED
STARRY_GROUPED_TESTS_PASSED
```

`cargo check -p starry-kernel` 同时通过。下一步再次运行真实 app，确认
systemd unit symlink chase 是否越过，并继续记录首分歧。

真实 app 复验确认 mount-ID 修复生效：unit 搜索、符号链接解析、preset、
默认 target 加载均已越过，systemd 输出：

```text
Queued start job for default target Multi-User System.
[  OK  ] Reached target Multi-User System.
Startup finished in 15.315s.
```

这把 Stage 2 从“systemd 启动后退出”推进为真实到达 multi-user target。
但新的首个运行期分歧为：

```text
-.slice: Failed to create cgroup /: Operation not permitted
Failed to realize cgroups for queued unit init.scope, ignoring: Operation not permitted
```

随后依赖 cgroup 的 service/socket/mount 进程均出现：

```text
Failed to spawn 'start' task: Operation not permitted
Failed with result 'resources'.
```

`starry-nixos-marker.timer` 已启动，但 marker service 不能在该条件下 spawn，
所以没有 `STARRY_NIXOS_PHASE=marker` 或 `STARRY_NIXOS_SYSTEM_PASSED`；严格
matcher 正确以失败结束。当前不能把“到达 target”误写成完整 Stage 2 通过。

## 5. 当前功能边界

| 能力 | 状态 | 说明 |
|---|---|---|
| 独立 NixOS-generated rootfs | 已完成 | 无 Alpine/APK runtime dependency |
| 锁定 closure/provenance | 已完成 | manifest 绑定 lock、closure、systemd、image hash |
| `/init` 作为 PID 1 | 已完成 | x86_64 QEMU 实证 |
| NixOS early stage-2 | 已完成 | 已进入 activation |
| activation 基础文件操作 | 已完成当前范围 | `ramfs` 红绿 QEMU 回归通过，真实 activation 以 0 退出 |
| `/dev/fd/N` pipe reopen | 已完成 | 红绿 QEMU 回归 |
| systemd 二进制作为 PID 1 | 已完成 | 已实际启动后退出 |
| systemd API filesystems | 已越过 | `statx` mount-root 红绿回归及真实 app 复验通过 |
| systemd unit 加载 | 已越过 | mount-ID 红绿回归及真实 app 复验通过 |
| `multi-user.target` | 已到达 | 真实 systemd 输出已确认，但部分 units failed |
| systemd cgroup service spawn | 修复中 | 根 cgroup `/` 创建返回 `EPERM` |
| marker service | 未完成 | timer 已启动，service 未执行，无 success marker |
| 登录/交互 shell | 不在本轮范围 | getty 明确关闭 |
| generation/rebuild/switch | 不在本轮范围 | 后续 feature |
| aarch64 | 未验证 | x86_64-first 决策 |

因此当前最准确的表述是：

> StarryOS 上可完成 NixOS activation、加载 systemd units 并真实到达
> `multi-user.target` 的 x86_64 NixOS Stage-2 prototype；由于 cgroup
> service spawn 仍失败且 marker 未执行，尚未满足严格成功门槛。

## 6. 已完成验证

以下验证已通过：

- `apps/starry/nixos/build-rootfs.sh --self-test`；
- 锁定 NixOS closure 和 ext4 镜像构建/发布；
- default 与 `nixos` 两种真实 StarryOS entrypoint 编译；
- Axbuild app-owned rootfs 回归；
- Axbuild boot matcher 正向/负向回归；
- grouped C selected-subcase 环境回归；
- `cargo fmt --all --check`；
- `git diff --check`；
- `cargo xtask clippy --package axbuild`：1/1；
- `cargo xtask clippy --package starryos`：12/12，包括 `nixos` feature；
- `cargo xtask clippy --package starry-kernel`：此前 25/25；本次修改后主机
  23 项通过，完整矩阵待 Podman 补验；
- `cargo xtask clippy --package ax-fs-ng`：6/6；
- `qemu/system/starrynixos-stage2`；
- `qemu/system/bugfix-dev-fd-symlinks` 修复前红、修复后绿；
- `qemu/system/bugfix-ramfs-mount` 修复前红、修复后绿；
- `qemu/system/bugfix-statx-mount-root` 修复前红、修复后绿；
- 真实 `cargo xtask starry app qemu -t nixos --arch x86_64`：activation
  以 0 退出，systemd 在 API filesystem 挂载点判断处按预期失败。

尚未完成的收尾验证：

- 与本功能直接相关的既有 PID 1/cgroup system subcases 批量复跑；
- SpecKit `tasks.md` 的最终 T023-T034 证据核对与勾选；
- 完成上述核对后的最终工作区总结。

## 7. 下一步顺序

1. 在 Podman + `.ci-cache` 中补齐受影响 crate 的完整 clippy；
2. 追踪 systemd 创建根 cgroup `/` 返回 `EPERM` 的具体 syscall/操作；
3. 以最小 cgroup 红测锁定后修正 owning subsystem，再复跑真实 app；
4. 复跑 PID 1/cgroup 相关 grouped 回归；
5. 只有完整有序 phase 和 `STARRY_NIXOS_SYSTEM_PASSED` 出现后，才把
   `multi-user.target`/marker 标为完成。

systemd 260.2 的 `cg_create("/")` 会对 `/sys/fs/cgroup` 调用
`mkdir(2)`，并将 `EEXIST` 视为成功。最初怀疑是挂载覆盖路径的通用创建
语义，但 `cgroup-basic` 加入
`mkdir(cgroup2_mount_root) == -1 && errno == EEXIST` 后首次运行即通过：
Podman + `.ci-cache` 下结果为 44 pass、0 fail。这一反证说明普通 cgroup2
挂载根语义已经正确，实际 `EPERM` 更可能来自 `/sys/fs/cgroup` 的特定
挂载拓扑、systemd 使用的路径形态，或 `cg_create()` 中根目录创建之后的
操作。systemd 的 `mkdir_parents()` 还会检查 `/sys` 与 `/sys/fs`，因此已将
回归扩展为分别要求现有 `/sys`、静态 `/sys/fs`、静态
`/sys/fs/cgroup` 均返回 `EEXIST`；这比 `/tmp` 挂载点更贴近真实拓扑。
扩展后的红灯为 45 pass、2 fail：`/sys/fs` 和 `/sys/fs/cgroup` 都错误
返回 `EPERM`。根因是 `FsContext::create_dir()` 未先检查可见挂载树中的
最终目录项，静态伪文件系统的拒绝创建错误覆盖了 Linux 要求的
`EEXIST`。现已在 ax-fs-ng 创建边界加入 no-follow 存在性检查，等待同一
回归转绿。验证结果：`cargo fmt --all --check` 与
`cargo check -p ax-fs-ng` 通过；同一 Podman/QEMU `cgroup-basic` 从
45 pass、2 fail 转为 47 pass、0 fail。下一步复跑真实 NixOS app，确认
systemd service spawn 与 marker 是否随之恢复。

真实 app 复跑确认 cgroup 根目录分歧已消失：日志不再出现
`Failed to create cgroup /: Operation not permitted`，systemd 成功创建 slice、
socket 并进入 unit 启动阶段。新的首个严格失败是 executor 子进程退出
127，随后所有需派生进程的 units 报：

```text
Failed to spawn executor: No such file or directory
Failed to spawn 'start' task: No such file or directory
```

内核侧同时显示 `clone3` 子进程进入用户态后以 32512（shell 语义 127）
退出。`multi-user.target` 路径中的 marker timer 已启动，但 marker service
仍无法执行。当前下一步是核对 systemd 260.2 的 executor 二进制路径、
NixOS 镜像 closure 是否包含该路径，以及 StarryOS `execve`/`execveat`
边界返回 `ENOENT` 的具体位置；尚不能把 marker 或严格成功标为完成。

只读检查 ext4 镜像后确认
`systemd-minimal-260.2/lib/systemd/systemd-executor` 与完整
`systemd-260.2/lib/systemd/systemd-executor` 均存在且为可执行文件，排除
closure 漏文件。systemd 260.2 的 `exec_spawn()` 会把固定的 executor fd
格式化为 `/proc/self/fd/<n>` 后交给 `posix_spawn`。现已把
`open("/proc/self/exe", O_PATH|O_CLOEXEC)` 后通过 `/proc/self/fd/<n>`
`posix_spawn` 自身的场景加入 `bugfix-dev-fd-symlinks`。首次 Podman/QEMU
运行即通过，子进程打印 `STARRY_PROC_FD_EXECUTOR_CHILD_PASSED`；因此普通
`O_PATH + procfd + posix_spawn` 不是根因。与 systemd 路径仍有两个关键
差异待核对：Nix 构建的 executor ELF 解释器/依赖，以及 glibc
`pidfd_spawn`/`clone3(CLONE_INTO_CGROUP)` 路径（StarryOS 当前日志显示
`cgroup parameter not supported, ignoring`）。

进一步只读核对表明 executor 的 glibc ELF 解释器文件存在，RUNPATH 也覆盖
systemd 私有库与 glibc closure。现已补充与 glibc `pidfd_spawn` 更接近的
探针：挂载 cgroup2，打开 executor 与 cgroup fd，再以
`clone3(CLONE_VM|CLONE_VFORK|CLONE_CLEAR_SIGHAND|CLONE_PIDFD|CLONE_INTO_CGROUP)`
创建子进程并通过 `/proc/self/fd/<n>` exec。该探针将区分普通 procfd
执行与 clone3/cgroup 组合语义。

探针首次构建因 grouped C 的 musl sysroot 不含 `linux/sched.h` 而停止，
尚未形成目标红/绿证据；已改用稳定的 Linux UAPI `clone_args` 字段布局与
flag 数值，避免引入额外内核头依赖后重跑。

该直接 clone3 C 探针随后暴露出 `CLONE_VM|CLONE_VFORK` 共享调用栈会让
父分支变量被子分支覆盖，无法提供可靠判据，已移除。更直接的 Linux
procfd magic-link 语义探针现为：复制自身到临时可执行文件、以 `O_PATH`
固定 fd、删除原路径，再从 `/proc/self/fd/<n>` `posix_spawn`。StarryOS
当前 procfs 仅把 fd 映射回字符串路径，预计该用例会稳定返回 `ENOENT`；
而 systemd 固定 executor fd 的目的正是避免依赖原路径持续可解析。

Podman/QEMU 红灯已确认：普通仍存在路径的 procfd spawn 通过，但删除原
路径后的 spawn 稳定返回 `ENOENT`。现已在 `sys_execve` 识别当前进程
`/proc/self/fd/<n>` 与 `/dev/fd/<n>`，并通过 `AT_EMPTY_PATH` 语义直接
取得 fd 固定的文件 `Location`（同时保留 memfd 支持），不再把 procfd
降级成字符串路径重新解析。`cargo fmt --all --check` 与
`cargo check -p starry-kernel` 通过；同一 Podman/QEMU 回归已转绿，删除
原路径后的 executor 也成功打印 `STARRY_PROC_FD_EXECUTOR_CHILD_PASSED`。
下一步复跑真实 app，确认 systemd executor 与 marker。

真实 app 在 procfd magic-link 修复后再次前进：原先的
`Failed to spawn executor: No such file or directory` 已消失，executor
子进程能够进入用户态。当前新的首个严格失败为：

```text
modprobe@drm.service: Failed to spawn executor: Bad file descriptor
(modprobe): Failed to attach to cgroup
  /system.slice/system-modprobe.slice/modprobe@drm.service:
  No such file or directory
```

同类失败随后出现在 `modprobe@efi_pstore.service`、
`modprobe@fuse.service`、`systemd-journald.service` 与 `firewall.service`。
这说明 procfd executor 路径修复有效，当前边界已经收敛到 systemd 为
service 子进程准备/加入叶子 cgroup 时，目标目录尚不存在或不可见；
父进程最终把 executor 子进程失败映射为 `EBADF`。本轮严格 matcher 因
首个 `.service: Failed with result` 按设计退出，尚未到达 marker。下一步
先以 cgroup grouped 回归复现“创建层级后由 clone3/executor 加入叶子
cgroup”的最小语义，再决定修复 cgroup 创建、目录可见性或 clone3
`CLONE_INTO_CGROUP` 支持，不能把 matcher 放宽来掩盖该失败。

已在 `cgroup-basic` 加入不共享地址空间/调用栈的确定性
`clone3(CLONE_INTO_CGROUP)` 回归：父进程打开目标 cgroup2 目录 fd，子进程
从创建后首次运行起就必须能在该目录的 `cgroup.procs` 中看到自身。旧实现
稳定为 49 pass、1 fail，并同时打印
`sys_clone3: cgroup parameter not supported, ignoring`。现已实现该语义：
clone3 从打开的 cgroup2 目录取得稳定 `CgroupNode`，在子进程进入全局任务表
和调度队列之前，将 fork 成员关系直接提交到目标节点；非 cgroup2 fd 和未带
flag 的非零 cgroup 字段不会被静默接受。`cargo fmt --all`、
`cargo check -p starry-kernel` 通过，同一 Podman + `.ci-cache` QEMU 回归转为
50 pass、0 fail。下一步再次复跑真实 NixOS，确认 executor 是否越过
`Bad file descriptor` 并继续向 marker 收敛。

修复后的真实 NixOS 复跑进一步确认：内核不再打印“忽略 cgroup 参数”，
executor 子进程失败日志中的
`Failed to attach to cgroup ...: No such file or directory` 也已消失，说明
`CLONE_INTO_CGROUP` 的目标解析与原子成员归属确实生效。但 systemd 的
`pidfd_spawn`/executor 启动握手仍返回 `Bad file descriptor`，PID 1 随后
向已创建的 executor 子进程发送 `SIGTERM`；相关 jobs 等待 90 秒后以
`resources` 失败，严格 matcher 正确终止运行。当前首个剩余分歧已从
cgroup 层进一步缩小为 systemd-executor 的 fd 继承/反序列化契约，重点是
非 `CLOEXEC` serialization fd 在 glibc
`clone3(CLONE_VM|CLONE_VFORK|CLONE_PIDFD|CLONE_INTO_CGROUP)` 与 exec 之间
是否保持可用。marker 尚未执行，Stage 2 仍不能标记完成。

## 9. 分批提交边界（2026-08-02）

用户要求只提交已有绿色验证证据的进展，未通过项继续留在工作区：

- 可提交候选：已通过自身回归/检查的 app-owned rootfs 基础设施、
  StarryNixOS 有界启动基线，以及已完成红绿闭环的 ramfs、statx、sysfs
  mkdir、procfd 与 `CLONE_INTO_CGROUP` 修复；
- 提交前仍须满足仓库要求的修改后 targeted clippy，缺少该门槛的候选先补验；
- 不提交：当前尚未解决的 systemd-executor serialization fd `EBADF`，以及
  任何为诊断该问题而产生但未形成红绿证据的改动；
- `.envrc`、`.gitignore` 中的 direnv 设置等个人环境改动不属于本功能提交。

提交前门槛已补齐：`cargo fmt --all --check`、`git diff --check`、容器内
`cargo xtask clippy --package starry-kernel`（25/25）与
`cargo xtask clippy --package ax-fs-ng`（6/6）均通过。首批仅暂存 grouped C
prebuild 的 3 个文件，缓存区 `diff --check` 通过；两次 `git commit -S`
均在调用密钥 `6DD77ED03B85DD91` 后因桌面 pinentry 未确认而超时，未生成
任何提交对象。当前保持该批次原样暂存，不降级为未签名提交；其余绿色修复
仍未暂存，待私钥解锁后按 procfd、ramfs、statx、cgroup 分类继续签名提交。

私钥解锁后已完成五个分类签名提交，并逐个确认 GPG 状态为 `G`（Good
signature）：

- `9302b8a4a` `fix(axbuild): scope grouped prebuild to selected tests`
- `03ee98de2` `fix(starry-fs): support procfd descriptor reopening and exec`
- `977b4a19f` `fix(starry-fs): add Linux-compatible ramfs mounts`
- `42d0c3a98` `fix(starry-fs): report mount identity through statx`
- `96e32ca77` `fix(starry-cgroup): complete systemd cgroup setup semantics`

未通过最终 marker 的 StarryNixOS app、Stage 2 测试与 axbuild app 集成仍留在
未暂存工作区；systemd-executor serialization fd `EBADF` 继续作为当前收尾
边界。个人 `.envrc`/`.gitignore` 改动也未提交。

继续 T028 后，上游 systemd v260.2 固定提交
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的调用链表明，`pidfd_spawn()`
成功后 `posix_spawn_wrapper()` 会调用 `pidref_set_pidfd_consume()`；后者通过
`pidfd_get_pid()` 先尝试 `PIDFD_GET_INFO`，再回退读取
`/proc/self/fdinfo/<pidfd>` 的 `Pid:` 字段。该路径不存在时 systemd 明确把
`ENOENT` 转换为 `EBADF`。因此先前“serialization fd 未继承”的推测被新证据
取代，当前首个差异是 pidfd fdinfo 可观测语义。

已新增最低层回归 `test-suit/starryos/qemu/system/bugfix-pidfd-fdinfo/`：直接
`clone3(CLONE_PIDFD)`，在子进程仍存活时读取 pidfd 的 `Pid:` 与 `NSpid:`。
相同源码在宿主 Linux 通过；Podman + `.ci-cache` 中当前 StarryOS 稳定红灯为
`open /proc/self/fdinfo/3: ENOENT`，grouped runner 以 0/1 失败退出。首次容器
运行仅因镜像未预装 `fakeroot` 停在资产准备阶段；按既有方案使用
`.ci-cache/apt` 在临时容器安装后，红灯已实际进入 QEMU 并得到上述结果。

随后在 procfs 的进程目录增加动态 `fdinfo/`，并让 pidfd 条目报告目标
`Pid:`/`NSpid:`；线程 pidfd 保留线程 ID，进程 pidfd 使用进程 ID，其他有效
描述符暂时提供空的 fdinfo 文件而不伪造类型字段。同一 Podman + `.ci-cache`
QEMU 回归已由 0/1 红灯转为 1/1 绿色，输出
`PASS: pidfd fdinfo Pid=41 NSpid=41` 和
`STARRY_PIDFD_FDINFO_PASSED`。`cargo fmt --all` 也已在该容器环境通过；下一步
执行 `starry-kernel` targeted clippy，并复跑真实 StarryNixOS，确认 systemd
executor 是否越过原先的 `EBADF`。

`cargo xtask clippy --package starry-kernel` 已在 Podman 中完成全部 25/25 检查，
0 失败；仅出现依赖 `memchr` 的 future-incompat 提示，不是本次代码告警。当前
最低层回归、格式化和 owning crate clippy 三项门槛均已通过，开始复跑真实
StarryNixOS app。

由于工作区没有可复用镜像，已通过 app 自有 `build-rootfs.sh` 和固定
`flake.lock` 首次构建并发布真实 x86_64 NixOS ext4 镜像。产物通过 e2fsck、
provenance、system profile、activation 数据与 Alpine 污染检查；manifest
记录 systemd 260.2、系统闭包
`/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2`
和镜像 SHA-256
`c791e3cc6c0f4c4b4feaf09e2dd3f9212ff62af50c72d6c5c92a456c9b73c18e`。
下一步在 Podman 中以 `STARRY_NIXOS_REUSE_ROOTFS=1` 复用该受检产物运行真实
app。

真实 app 已确认越过原先的 `Failed to spawn executor: Bad file descriptor`：
systemd 成功创建并执行 service executor，进入 `Multi-User System` 的实际 unit
启动。新的首个分歧是 systemd executor 调用 `prctl(PR_GET_SECUREBITS)` 与
`prctl(PR_SET_SECUREBITS)` 时 StarryOS 对 option 27/28 返回 `EINVAL`，导致
`modprobe@drm.service`、`firewall.service` 在 `SECUREBITS` step 退出；随后需要
凭据设置的 unit 也在 `CREDENTIALS` step 得到 `ENOSYS`。另有 udev kernel
netlink socket 得到 `EPROTONOSUPPORT`。本轮严格 matcher 尚在等待卡住的 unit
超时，marker 未到达；下一最低层红绿闭环先处理 securebits ABI，不放宽 matcher
或屏蔽 unit。

严格 matcher 最终在约 7 分 30 秒后以
`systemd-journald.service: Failed with result 'timeout'` 正式拒绝该次运行；相关
executor 在 SIGTERM/SIGKILL 后仍未被 systemd 观察为退出。该终态与前述
securebits/credentials 失败一致，且全程未再出现 pidfd `EBADF`。因此 pidfd
fdinfo 修复自身已完成红绿与真实 app 验证，但 Stage 2 的 T028 仍保持未完成，
继续针对 `PR_GET_SECUREBITS`/`PR_SET_SECUREBITS` 建立下一组直接 ABI 红测。

已在 `syscall-test-capset` 增加 securebits 子进程隔离用例，覆盖默认值、
`PR_SET_SECUREBITS`/`PR_GET_SECUREBITS` 往返，以及 locked bit 不可清除。Podman
+ `.ci-cache` QEMU 中原有 20 项通过，新 C4 在首个
`PR_GET_SECUREBITS` 稳定返回 `EINVAL`，最终 20 pass、1 fail、grouped 0/1，
形成可归因的下一轮红测。

现已把 securebits 纳入共享 credential snapshot，实现 option 27/28 的 set/get、
`CAP_SETPCAP` 权限、有效位校验、locked bit 不可逆约束，并让
`NO_SETUID_FIXUP`、`KEEP_CAPS` 和 `NO_CAP_AMBIENT_RAISE` 参与现有 capability
状态迁移。相同 Podman QEMU 用例已由 20 pass、1 fail 转为 21 pass、0 fail，
grouped 1/1 绿色；`cargo fmt --all` 通过。下一步补跑 `starry-kernel` targeted
clippy，再复跑真实 app 确认 executor 越过 `SECUREBITS` step 后的首个差异。

securebits 修改后的 `cargo xtask clippy --package starry-kernel` 已在 Podman 中
再次通过全部 25/25 检查（0 失败）；依赖 `memchr` 的 future-incompat 提示保持
不变。开始复用已校验的 NixOS 镜像进行下一次真实 app 启动。

真实 app 复跑中，`PR_GET_SECUREBITS` 已不再出现 unsupported 日志，但
`PR_SET_SECUREBITS` 仍使 firewall executor 返回 `EINVAL`；最低层回归使用的
未使用参数均为零，而 systemd 的实际 prctl 调用形态可能携带非零未使用寄存器。
Linux 对该 option 只消费 securebits 值，因此下一步以宿主 Linux 对照补充非零
尾随参数用例，确认当前 StarryOS 是否因过度校验 arg3-arg5 而拒绝兼容调用。

已将 C4 补强为向 `PR_SET_SECUREBITS` 传入非零 arg3-arg5；StarryOS QEMU
稳定从 21/0 回到 20/1，唯一失败为
`set securebits ignores unused trailing arguments: EINVAL`。这与真实 systemd
executor 的失败完全对齐，确认本轮实现对 Linux 忽略参数的校验过严。为避免
重复等待无新信息的 7 分钟 unit timeout，第二次真实 app 在记录该首个差异后
仅终止本轮 QEMU PID，产物与工作区未删除。

已移除 `PR_GET_SECUREBITS`/`PR_SET_SECUREBITS` 对 Linux 不消费的尾随参数校验，
保留 arg2 有效位、权限与锁语义检查。补强后的同一 QEMU 用例再次由 20/1
转为 21/0，明确覆盖非零 arg3-arg5，`cargo fmt --all` 通过。下一步需要再跑
targeted clippy 与真实 app；只有后者确认 `SECUREBITS` step 消失后，才继续处理
`CREDENTIALS` 分歧。

最终 securebits 参数语义后的 `cargo xtask clippy --package starry-kernel` 已再次
通过 25/25（0 失败），开始第三次真实 app 复跑以确认 `SECUREBITS` 消失。

第三次真实 app 启动已确认 securebits 修复生效：日志中不再出现
`PR_GET_SECUREBITS`/`PR_SET_SECUREBITS` unsupported 或
`Failed at step SECUREBITS`，`firewall.service` executor 已实际运行到
`xtables-nft-multi`（其后因尚未支持的应用/协议能力退出）。这证明 systemd
executor 已越过 securebits 设置阶段。Stage 2 marker 仍未出现；为避免在已知
unit 超时上再次空等约 7 分钟，本轮在收集到边界证据后只终止了对应 QEMU
进程，没有删除镜像或工作区产物。当前首个可归因边界转为
`Failed at step CREDENTIALS: Function not implemented`；此前同一调用窗口有明确
`Unimplemented syscall: keyctl`，下一步从固定 systemd 260.2 调用链和 StarryOS
syscall 分派两侧确认是否为 session keyring 语义，再建立最低层红绿回归。

固定 systemd 260.2 源码进一步推翻了 keyctl 假设：`setup_keyring()` 明确忽略
`keyctl(KEYCTL_JOIN_SESSION_KEYRING)` 的 `ENOSYS`；`CREDENTIALS` 状态来自更早的
`exec_setup_credentials()`。该路径调用 `fsopen("tmpfs")`、`fsconfig()`、
`fsmount()` 和 `move_mount()` 建立只读凭据文件系统，并且只在权限错误时回退
普通目录，故 StarryOS 当前 `fsopen -> ENOSYS` 会直接成为
`Failed to set up credentials: Function not implemented`。不以伪造 `EPERM` 诱导
回退，也不实现无关 keyctl。

已新增精确回归
`test-suit/starryos/qemu/system/bugfix-new-mount-api-credentials/`，复现 systemd
使用的 tmpfs filesystem-context 配置、detached mount、挂载前填充、只读
reconfigure 和 `MOVE_MOUNT_F_EMPTY_PATH` 附着序列。首次交叉编译因精简 sysroot
不含 `linux/mount.h` 停在构建阶段，已改为测试内固定 Linux UAPI 常量；随后
Podman + `.ci-cache` QEMU 实际红灯为首个 `fsopen("tmpfs")` 返回 `ENOSYS`，
输出 `STARRY_NEW_MOUNT_API_CREDENTIALS_FAILED`，grouped 0/1。现有 VFS 已能创建
内存文件系统、表达独立 mountpoint 与目录 fd，但尚缺 detached-root 的受控附着
操作；下一步实现仅覆盖该已测 tmpfs/ramfs 新 mount API 子集，并继续让
`fspick`/`open_tree` 等未实现入口返回 `ENOSYS`。

受限实现现已落到 VFS/syscall 边界：filesystem-context fd 仅接受 tmpfs/ramfs；
StarryOS 不具备 tmpfs 配额核算，因此 systemd 的 `noswap` 探测返回 `EINVAL`，
让其按上游代码改用 ramfs，而不是虚假接受 `size`/`nr_inodes` 约束。ramfs
context 支持 `mode=0700`、create、fsmount、挂载前目录访问、只读 reconfigure，
并由带权限标记的原始 detached-mount fd 才能执行空路径 `move_mount`，普通重开
目录 fd 不继承该权限。`MOUNT_ATTR_NOSYMFOLLOW` 仍以 `EINVAL` 触发 systemd 的
既有兼容重试，`fspick`/`open_tree` 仍为 `ENOSYS`。旧的 mount-api 回归已相应
调整为只要求后两者保持 `ENOSYS`。

使用工作区本地 Rust/Cargo 缓存完成 `cargo fmt --all`，并以离线模式通过
`cargo check -p starry-kernel`，无编译告警；`git diff --check` 也通过。首次在
宿主执行 xtask clippy 因宿主缺少 `pkg-config` 可执行文件停在
`libudev-sys` 构建脚本，尚不构成 clippy 结果。原计划的 Podman 格式化/QEMU
复跑因本次提权审批服务额度耗尽而未启动；在用户再次授权可用前，不把实现
记为绿色，也不运行真实 NixOS app。

2026-08-02 用户再次明确同意使用 Podman。后续验证继续采用 Podman +
`.ci-cache/`，先执行 `starry-kernel` targeted clippy，再运行
`bugfix-new-mount-api-credentials` 与旧 `bugfix-bug-mount-api-enosys` QEMU
回归；只有这些门槛通过后才复用已校验的 NixOS 镜像运行真实 app。当前
filesystem-context 实现仍属于未验证工作区，不进入签名提交。

Podman + `.ci-cache/` 验证已通过 `cargo fmt --all --check`，并完成
`cargo xtask clippy --package starry-kernel` 的 25/25 检查，0 失败；仅有依赖
`memchr` 的 future-incompat 提示。requirements checklist 为 16/16 完成。
下一门槛是让新的 credentials mount API 回归从既有红灯转为绿色。

新的 QEMU 回归已运行到完整 mount API 序列：`fsopen`、`fsconfig`、`fsmount`、
detached fd 重开、挂载前创建/写入、只读 reconfigure 与 `move_mount` 均通过；
当前唯一失败是附着后通过目标路径读取凭据文件得到 `ENOENT`。只读写入检查仍
正确返回 `EROFS`。这把问题缩小到 detached mount 附着后的 VFS 路径可见性，
而不是 syscall 分派、context 状态或只读属性；当前实现继续保持未验证、不提交。

根因进一步定位到 `ax-fs-ng` 的 `openat(dirfd, ".")`：当 `dirfd` 指向没有父
mount location 的 detached root 时，`resolve_parent(".")` 返回
`InvalidInput`，旧逻辑无条件退回进程 `/`，导致测试实际把凭据写进全局根目录；
mount 附着本身没有丢数据。现已改为在该分支解析原路径本身，使相对 `.` 保持
调用方提供的 current directory，而绝对 `/` 仍解析到进程 root。下一步用同一
QEMU 回归验证该修复，并补跑 `ax-fs-ng` 与 `starry-kernel` targeted clippy。

修复后的同一 Podman QEMU 回归已由 1 项失败转为全部通过：附着后的凭据文件
可按目标路径读取，内容保持一致，且只读 reconfigure 后创建仍返回 `EROFS`；
最终输出 `STARRY_NEW_MOUNT_API_CREDENTIALS_PASSED` 和 grouped 1/1 绿色。
`cargo fmt --all --check` 同时通过。下一步复跑旧 mount API unsupported 回归，
并执行 `ax-fs-ng` targeted clippy，防止有限支持误扩展到 `fspick/open_tree`。

旧回归已通过：`fsopen("tmpfs")` 进入受限支持，而 `fspick` 与 `open_tree`
继续精确返回 `ENOSYS`，结果 3/0、grouped 1/1。`cargo xtask clippy --package
ax-fs-ng` 的 6/6 检查也全部通过。当前最低层门槛已满足，开始复用既有受检
NixOS 镜像运行真实 Stage 2 app，确认 systemd 是否越过 `CREDENTIALS`。

真实 app 已确认越过 `CREDENTIALS`：本轮日志未再出现
`Failed at step CREDENTIALS`，相关 executor 已进入实际 unit 工作负载。
Stage 2 marker 仍未出现，当前仍被 journald/sysctl、内核模块和 firewall 等
unit 的超时/协议缺口阻塞；本轮有界运行在等待终态时被用户中止，因此 T028
保持未完成，该 app 结果不作为成功提交依据。

用户更新提交策略：每个已验证成功的独立进展可以立即普通提交，当前不要求
`git commit -S`，用户之后补签名；未通过或仍在验证的内容继续不提交。后续先
按文件归属拆分 pidfd fdinfo、securebits、credentials mount API 三类已验证
成果，避免把 StarryNixOS app、Stage 2 marker 或个人 `.envrc`/`.gitignore`
改动混入提交。

pidfd fdinfo 分类已普通提交为 `11790a00e`
(`fix(starry-fs): expose pidfd fdinfo metadata`)。首次未显式禁用签名时被
`commit.gpgsign=true` 自动触发 GPG pinentry 并超时，未产生提交；随后使用
`git commit --no-gpg-sign` 成功。提交仅包含 pidfd 实现与对应 QEMU 回归，
未包含其他工作区内容。

securebits 分类已普通提交为 `4d1d1c561`
(`fix(starry-cred): implement securebits prctl semantics`)。提交仅包含
credential snapshot、`PR_GET/SET_SECUREBITS` 语义与 `syscall-test-capset`
回归；对应 QEMU 21/0 和 `starry-kernel` 25/25 clippy 证据已在提交前完成。

credentials mount API 提交前补跑 `cargo xtask clippy --package axfs-ng-vfs`，
3/3 检查全部通过。至此该分类已有新 QEMU 回归 1/1、旧 unsupported 回归
1/1、`starry-kernel` 25/25、`ax-fs-ng` 6/6、`axfs-ng-vfs` 3/3 和 rustfmt
证据，可以独立普通提交。

credentials mount API 分类已普通提交为 `3bf901b56`
(`fix(starry-fs): support credential mount contexts`)。提交仅包含 bounded
`fsopen/fsconfig/fsmount/move_mount`、detached mount VFS/`openat(".")` 修复及
新旧 QEMU 回归；未包含仍未通过 marker 的 StarryNixOS app 集成。

2026-08-02 在当前 HEAD 上重新用 Podman + `.ci-cache` 运行真实
`STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch
x86_64`。本轮再次确认 `CREDENTIALS` 已越过，systemd 能加载 units、排队默认
target 并进入实际 service 工作负载；此前 pidfd、securebits 和 credentials
mount context 三项修复没有回退。

同时发现 credentials mount API 的有限支持改变了 util-linux 2.42.2 的
mount 路径：`fsopen(2)` 不再返回 `ENOSYS` 后，NixOS activation 的
`specialfs` 使用新 mount API，并在为内存文件系统应用 VFS mount attributes
时两次调用尚未实现的 `mount_setattr(2)`。日志为：

```text
Unimplemented syscall: mount_setattr
Activation script snippet 'specialfs' failed (32)
Task(..., "activate") exit with code: 256
```

systemd 随后仍能启动并输出
`Queued start job for default target Multi-User System.`，但 marker 未执行；
运行继续被 journald/sysctl、kernel module、firewall 等 unit 长时间等待阻塞，
因此在收集约 87 秒证据后主动终止 QEMU，runner 正确报告未匹配 success regex。
日志还显示 marker unit 的 `StandardOutput=console` /
`StandardError=console` 被 systemd 260.2 拒绝解析并忽略，这会使默认日志路径
继续依赖尚未工作的 journal，属于 app 配置收尾问题。

当前首个需要闭环的精确内核边界调整为 `mount_setattr(2)`：

1. 先增加直接覆盖 detached mount fd、`AT_EMPTY_PATH` 和
   `MOUNT_ATTR_*` 的 QEMU 红测；
2. 只实现 util-linux/NixOS 当前实际需要的有限属性集合和错误检查；
3. fmt、`starry-kernel`/相关 VFS crate targeted clippy、同一回归转绿；
4. 再次运行真实 app，确认 activation 恢复为 0；
5. 成功后先更新本文档，再用 `git commit --no-gpg-sign` 独立提交。

本次状态检查没有修改内核或测试代码，也没有产生新提交。

已新增
`test-suit/starryos/qemu/system/bugfix-mount-setattr/`，直接执行
`fsopen("ramfs") -> fsconfig -> fsmount -> mount_setattr -> move_mount`
序列。修复前该用例稳定得到 `mount_setattr: ENOSYS`，输出
`STARRY_MOUNT_SETATTR_FAILED`；实现 syscall 分派和有限属性支持后，短结构及
未知 lookup flags 均返回 `EINVAL`，detached mount 属性应用和附着成功。

首次修复后复跑暴露了第二个真实缺口：`mount_setattr` 已成功，但
`statfs.f_flags` 没有合并 mountpoint 的 `nosuid/nodev` 状态。测试最初还错误
要求不存在于 Linux `statfs` 可见 flags 集合中的 `ST_STRICTATIME`，现已删除
该虚构断言，同时继续用 `MOUNT_ATTR_STRICTATIME` +
`attr_clr=MOUNT_ATTR__ATIME` 覆盖 util-linux 的真实输入形态。内核
`statfs/fstatfs` 路径现将 mountpoint 的 readonly、nosuid、nodev、noexec、
noatime 和 relatime 状态转换为 Linux `ST_*` 可见值；strictatime 仍通过
`/proc/*/mountinfo` 的 mount options 表达，不伪造 `statfs` 位。

当前 Podman + `.ci-cache` 证据已绿色：

```text
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/bugfix-mount-setattr

PASS: mount_setattr rejects a short mount_attr structure
PASS: mount_setattr rejects unknown lookup flags
PASS: mount_setattr applies VFS attributes to a detached mount
PASS: move_mount attaches the attributed mount
PASS: statfs reports the attached mount
PASS: statfs exposes nosuid and nodev
STARRY_MOUNT_SETATTR_PASSED
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

该分类仍未提交：下一门槛是 `starry-kernel` targeted clippy、旧 credentials
mount API 回归，以及真实 StarryNixOS app 确认 activation 不再因
`mount_setattr` 失败。

后续 Podman 验证已通过：

- `cargo fmt --all --check`；
- `cargo xtask clippy --package starry-kernel`：25/25，0 失败；
- `cargo xtask starry test qemu --arch x86_64 -c
  qemu/system/bugfix-new-mount-api-credentials`：17 项全部通过，
  `STARRY_NEW_MOUNT_API_CREDENTIALS_PASSED`，grouped 1/1。

这证明新增的 `statfs` mountpoint flags 合并没有破坏此前 credentials detached
mount 的创建、填充、只读 reconfigure、附着和路径可见性。下一步运行真实
StarryNixOS app；在确认 activation 不再报 `mount_setattr` 前仍不提交。

2026-08-02 的真实 app 进一步定位到 util-linux 新挂载 API 的 tmpfs 配置边界。
`/run` 的精确调用序列为：

```text
fsopen("tmpfs")
fsconfig(FSCONFIG_SET_STRING, "source", "tmpfs")
fsconfig(FSCONFIG_SET_STRING, "mode", "755")
fsconfig(FSCONFIG_SET_STRING, "size", "25%")
fsconfig(FSCONFIG_CMD_CREATE)
```

修复前最后一步稳定返回 `EOPNOTSUPP`。util-linux 官方
`libmount/src/hook_mount.c` 表明只有新 API 初始化阶段记录到 `ENOSYS` 才会
回退 legacy `mount(2)`；CREATE 阶段失败不会回退，因此不能通过调整 errno
绕过。Linux `mm/shmem.c::shmem_parse_one` 使用 `memparse` 接受字节值及
`K/M/G/T/P/E` 后缀，也接受相对 `totalram_pages()` 的百分比。

现有 `MemoryFs` 已增加受限 tmpfs：

- `fsconfig("size", ...)` 解析百分比、纯字节值和 Linux 内存单位后缀；
- `FSCONFIG_CMD_CREATE` 将 size 上限传入 tmpfs，而不是静默忽略；
- `statfs` 按该上限报告 blocks/free blocks；
- 文件逻辑长度增长按 filesystem 级原子计数执行保守配额检查，超限返回
  `ENOSPC`，truncate 与 inode 释放回收计数；
- 尚未实现的 `nr_inodes` 上限继续保持显式不支持；systemd 的 `noswap`
  探测仍按既有语义返回 `EINVAL` 并回退 ramfs。

同一 QEMU 用例先得到确定红测：

```text
PASS: fsconfig accepts the NixOS /run tmpfs size
FAIL: fsconfig creates the sized NixOS /run tmpfs: errno=95 (Not supported)
STARRY_GROUPED_TEST_FAILED
```

修复后 `bugfix-mount-setattr` 扩展为 22 项并全部通过，其中包括：

```text
PASS: fsconfig creates the sized NixOS /run tmpfs
PASS: fsmount creates the sized detached tmpfs
PASS: move_mount attaches the sized tmpfs
PASS: statfs preserves the 25% tmpfs size limit
STARRY_MOUNT_SETATTR_PASSED
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

首次兼容性复跑还发现 parser 未接受旧 credentials 回归中的 `size=1M`，该用例
精确失败为 `errno=22`。补齐 Linux 单位后缀后，
`bugfix-new-mount-api-credentials` 再次 17/17、grouped 1/1 通过。最终质量
门槛为：

- `cargo fmt --all --check`：通过；
- `cargo xtask clippy --package starry-kernel`：25/25，0 失败；
- `bugfix-mount-setattr`：22/22，grouped 1/1；
- `bugfix-new-mount-api-credentials`：17/17，grouped 1/1。

真实 StarryNixOS app 的 `/run` mount（Task 48）和 `/run/keys` mount（Task 52）
现均退出 0，证明 sized tmpfs 与 source 处理已进入真实 activation 路径。但
`specialfs` 仍返回 32，当前更早的首个失败已收敛为 `/dev/pts`（Task 40）：

```text
mount: /dev/pts: unknown filesystem type 'devpts'.
Task(40, "mount") exit with code: 8192
Activation script snippet 'specialfs' failed (32)
Task(25, "activate") exit with code: 256
```

原因是 util-linux 已选择新 mount API，而当前 `fsopen` 只接受 tmpfs/ramfs；
legacy `mount(2)` 虽已有 devpts 实现，但 `fsopen("devpts")` 返回 `ENODEV`
使用户态判定该文件系统不存在。systemd 仍能启动并排队 Multi-User target，
但 marker 未出现。因此 sized tmpfs 分类可以独立提交，T028 仍保持未完成；
下一处 owning subsystem 是 devpts 的新 mount API context。

sized tmpfs、`mount_setattr` 和 mountpoint `statfs` flags 分类已普通提交为
`84f048b8c`（`fix(starry-fs): support sized memory mounts`）。提交仅包含已通过
聚焦回归、credentials 回归、fmt、`starry-kernel` clippy 和真实 `/run` 路径
验证的 4 个内核文件、聚焦测试与本文档；未包含 marker 尚未通过的 app 集成。

已新增独立聚焦用例
`test-suit/starryos/qemu/system/bugfix-devpts-new-mount-api/`，直接执行：

```text
fsopen("devpts")
fsconfig(source/mode/gid/ptmxmode/newinstance)
FSCONFIG_CMD_CREATE
fsmount
move_mount
```

用例还将验证 devpts magic、`ptmxmode` 以及新实例中 PTY slave 的 mode/gid。
2026-08-02 在 Podman CI 容器中安装 staging 所需的 `fakeroot` 后，当前内核
得到确定红测：

```text
FAIL: fsopen creates a devpts filesystem context: errno=19 (No such device)
STARRY_DEVPTS_NEW_MOUNT_API_FAILED: 1 checks
STARRY_GROUPED_TEST_FAILED
```

红点与真实 NixOS `/dev/pts` failure 一致，证明 owning boundary 是 devpts
filesystem context 注册/参数解析，而不是 legacy `mount(2)` 或测试环境。

内核现已将 devpts 纳入既有 filesystem context，并复用
`DevPtsOptions`/`new_devptsfs`：

- `fsopen("devpts")` 返回受 capability 和 CLOEXEC 约束的 context fd；
- `fsconfig` 支持 `source`、`mode`、`gid`、`ptmxmode` 和 flag 型
  `newinstance`；
- `FSCONFIG_CMD_CREATE` 创建独立 `DevPtsMount::NewInstance`，不改变 legacy
  `mount(2)` 共享 initial instance 的兼容路径；
- `fsmount` 保留 devpts 自身 root metadata，同时复用 detached mount、
  mount attributes 和 `move_mount` 流程。

同一 Podman 聚焦用例已由红转绿，19/19 检查通过：

```text
PASS: fsopen creates a devpts filesystem context
PASS: fsconfig creates the configured devpts instance
PASS: fsmount creates a detached devpts mount
PASS: move_mount attaches the devpts mount
PASS: statfs exposes the devpts filesystem identity
PASS: ptmxmode applies through the new mount API
PASS: mode applies through the new mount API
PASS: gid applies through the new mount API
STARRY_DEVPTS_NEW_MOUNT_API_PASSED
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

兼容性与质量门槛随后全部通过：

- legacy `qemu/system/test-devpts-newinstance` 仍为 13 项全部通过，覆盖 private
  instance、initial instance 共享、PTY allocator 和 controlling tty；
- `cargo fmt --all -- --check`：通过；
- `cargo xtask clippy --package starry-kernel`：25/25，0 失败。

真实 StarryNixOS app 复跑进一步证明该实现进入 activation 主路径：

```text
Task(40, "mount") exit with code: 0
Task(48, "mount") exit with code: 0
Task(52, "mount") exit with code: 0
Task(25, "activate") exit with code: 0
Queued start job for default target Multi-User System.
```

其中 Task 40 是 `/dev/pts`，Task 48 是 `/run`，Task 52 是 `/run/keys`。
因此 devpts 新 mount API 分类已满足独立提交条件。最终
`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，T028 保持未完成。

当前首个边界已转移到 systemd unit 启动阶段：

```text
[FAILED] Failed to listen on udev Kernel Socket.
systemd-journald.service: Failed at step OOM_ADJUST ... Invalid argument
systemd-journalctl.socket: Starting timed out.
```

同时 `systemd-modules-load` 启动的 `efi_pstore`、`drm` 和 `fuse` 模块 unit
持续处于无期限 start job。后续应先将最早、可独立复现的 unit 行为收敛为
新的确定红测，再修改 owning subsystem；不能用 marker 未出现的完整启动日志
直接推测修复，也不能把这些 unit 的失败宽泛屏蔽后宣称 T028 完成。

对 `OOM_ADJUST` 的只读定位表明，systemd 260.2 为 journald 写入
`/proc/self/oom_score_adj`，其值是带换行的负数。Linux
`oom_score_adj` 文本 ABI 接受 `-1000..=1000` 的十进制值；procfs 写入通常
携带结尾换行。StarryOS 当前直接对完整 write buffer 调用 Rust
`parse::<i32>()`，因此 `-250\n` 稳定被解析为 `EINVAL`，且尚未校验 Linux
规定的取值范围。

已新增独立 grouped 回归
`test-suit/starryos/qemu/system/bugfix-oom-score-adj/`，直接验证：

- `-250\n` 写入成功并可读回；
- `1001\n` 和 `-1001\n` 均以 `EINVAL` 拒绝。

下一步先在当前内核运行该用例取得确定红灯；红灯成立前不修改 procfs。

首次 Podman + `.ci-cache` QEMU 已进入 guest 并得到确定红灯：

```text
FAIL: oom_score_adj accepts a newline-terminated signed value:
      errno=22 (Invalid argument)
FAIL: oom_score_adj reports the adjusted value
STARRY_OOM_SCORE_ADJ_FAILED: 2 checks
STARRY_GROUPED_TEST_FAILED
```

首次用例中的越界输入也带换行，因此它们因同一个 parser 缺陷返回 `EINVAL`，
不能证明范围校验。现已将两项越界输入改为不带换行的 `1001` 与 `-1001`，
下一次修复前运行应同时暴露 parser 和范围两个独立缺口。

修正后的用例在同一 Podman QEMU 环境得到 4 项确定失败：

```text
FAIL: oom_score_adj accepts a newline-terminated signed value: EINVAL
FAIL: oom_score_adj reports the adjusted value
FAIL: oom_score_adj rejects values above 1000
FAIL: oom_score_adj rejects values below -1000
STARRY_OOM_SCORE_ADJ_FAILED: 4 checks
STARRY_GROUPED_TEST_FAILED
```

这证明当前实现同时缺少 procfs 文本 ABI 的尾随换行处理和 Linux
`-1000..=1000` 范围校验。实现仅在 `pseudofs/proc.rs` 的
`oom_score_adj` write 路径去除尾随 ASCII 空白、解析有符号十进制并检查范围；
不会借此扩展 StarryOS 的 OOM 策略、权限或继承模型。

修复后的 Podman 验证已通过：

```text
PASS: oom_score_adj accepts a newline-terminated signed value
PASS: oom_score_adj reports the adjusted value
PASS: oom_score_adj rejects values above 1000
PASS: oom_score_adj rejects values below -1000
STARRY_OOM_SCORE_ADJ_PASSED
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

同一容器中 `cargo fmt --all -- --check` 通过，
`cargo xtask clippy --package starry-kernel` 也完成 25/25、0 失败。该分类
已经满足最低层回归与 owning crate 质量门槛；下一步复用已校验的 NixOS
rootfs 运行真实 app，确认 journald 是否越过 `OOM_ADJUST`，并继续记录新的
首个终端边界。marker 出现前仍不提交该分类，也不勾选 T028。

真实 StarryNixOS app 已确认该分类进入 systemd executor 路径：

```text
Task(40, "mount") exit with code: 0
Task(48, "mount") exit with code: 0
Task(52, "mount") exit with code: 0
Task(25, "activate") exit with code: 0
Queued start job for default target Multi-User System.
```

本次日志不再出现 `OOM_ADJUST`；journald 越过该阶段后，新的精确失败为：

```text
systemd-journald.service: Failed to drop capabilities: Invalid argument
systemd-journald.service: Failed at step CAPABILITIES ... Invalid argument
systemd-journalctl.socket: Starting timed out. Stopping.
```

因此 `oom_score_adj` 分类具备独立提交条件。完整 app 在取得上述终端证据后
手动停止，以免 `efi_pstore`、`drm`、`fuse` 等无期限模块 jobs 继续占用
QEMU；`STARRY_NIXOS_SYSTEM_PASSED` 未出现，T028 保持未完成。下一处内核
候选已收敛为 systemd executor 的 capability drop ABI，必须先从
`capset(2)`/securebits/ambient capability 调用序列建立新的直接红测。

systemd 260.2 固定提交
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的
`capability_bounding_set_drop()` 逐项调用
`prctl(PR_CAPBSET_DROP, capability)`。Linux 的该 `prctl` option 只消费
capability 参数：Linux v6.12 的 `security/commoncap.c::cap_task_prctl()`
在该分支只把 `arg2` 传给 `cap_prctl_drop()`。StarryOS 当前却额外要求
`arg3..arg5` 全为 0。由于
`prctl(2)` 是 variadic 接口，systemd 不会为未消费参数提供零值，这正好解释
真实 app 中的 `Failed to drop capabilities: Invalid argument`。

现有 `syscall-test-capset` 曾反向断言尾随参数非零应得到 `EINVAL`。该断言已按
Linux ABI 修正为：使用非零尾随参数 drop `CAP_SETUID` 必须成功，并由
`PR_CAPBSET_READ` 观察到 capability 已从 bounding set 移除。Podman +
`.ci-cache` 聚焦 QEMU 在当前内核得到确定红灯：

```text
FAIL | child | PR_CAPBSET_DROP ignores unused trailing args |
       errno=22 (Invalid argument)
FAIL | C1: PR_CAPBSET_DROP removes a bounding cap |
       errno=22 (Invalid argument)
CAPSET_HAS_FAILURES
DONE: 20 pass, 1 fail
STARRY_GROUPED_TEST_FAILED
result: 0/1 case(s) passed
```

因此 owning boundary 已精确收敛到
`os/StarryOS/kernel/src/syscall/task/ctl.rs` 的 `PR_CAPBSET_DROP` 参数校验。
下一步只移除对未消费 `arg3..arg5` 的零值要求，保留 capability 范围检查和
`CAP_SETPCAP` 权限检查；不扩展 capability 模型或其他 `prctl` option。

内核修复已按上述边界完成：`PR_CAPBSET_DROP` 仍校验 capability 编号和
`CAP_SETPCAP`，但不再检查 Linux 未消费的 `arg3..arg5`。同一 Podman
聚焦用例已由红转绿：

```text
PASS | C1: PR_CAPBSET_DROP removes a bounding cap
CAPSET_ALL_PASSED
DONE: 21 pass, 0 fail
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

容器内 `cargo fmt --all` 同时通过。宿主直接运行格式化时，rustup 尝试删除
只读 `/home/user0/.rustup` 下的 update hash 而失败；改用项目 CI 容器及
`.ci-cache` 的 `CARGO_HOME`/`RUSTUP_HOME` 后解决，不需要修改宿主工具链。
下一步运行 `starry-kernel` 定向 clippy，再用真实 NixOS app 验证
`CAPABILITIES` 边界是否消失。

`cargo xtask clippy --package starry-kernel` 已在同一 Podman 环境完成：
25/25 checks 通过，0 失败。当前 capability 分类已满足最低层回归、格式化和
owning crate 质量门槛；剩余提交条件是实际 NixOS app 越过
`Failed at step CAPABILITIES`，并记录新的首个边界或最终 marker。

首次 app 命令未设置 artifact 复用开关，CI 镜像因没有 `nix` 而在进入 QEMU
前退出。随后使用
`STARRY_NIXOS_REUSE_ROOTFS=1` 复跑；该路径仍完整校验现有 manifest、
`flake.lock` hash、x86_64 closure、ext4 内容和 image hash，不绕过 artifact
provenance：

```text
StarryNixOS rootfs reused after manifest validation:
/workspace/tmp/axbuild/rootfs/rootfs-x86_64-nixos.img/rootfs-x86_64-nixos.img
```

真实 app 已确认 capability 修复进入 systemd executor 主路径：

- activation Task 25 退出 0，`/dev/pts`、`/run`、`/run/keys` mount 仍退出 0；
- systemd 排队 `Multi-User System`；
- 日志不再出现 `Failed to drop capabilities` 或
  `Failed at step CAPABILITIES`；
- `systemd-journald` 进程已进入自身初始化，并报告新的精确失败：

```text
systemd-journald[89]: 1 unknown file descriptors passed, closing.
systemd-journald[89]: Failed to enable SO_TIMESTAMP: Protocol not available
Task(89, "systemd-journald") exit with code: 256
```

本轮日志保存为
`.ci-cache/tmp/starrynixos-after-capbset-drop.log`。取得约 64 秒证据后手动
停止 QEMU，避免 `efi_pstore`、`drm`、`fuse`、firewall 等无期限 jobs 持续
运行。因此 capability drop 分类已经满足独立提交条件；新的 owning boundary
转移到 socket `SO_TIMESTAMP` 支持。`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，
T028 保持未完成，下一分类必须先为 `SO_TIMESTAMP` 建立确定红测。

## 8. 关联文档

- `silicalet/TODO.md`：总体路线与长期门槛；
- `silicalet/003-starryos-nixos-optionB-research.md`：早期方案 B 调研；
- `silicalet/NIXOS-COMPAT-RESEARCH.md`：Linux ABI/NixOS 对照研究；
- `specs/004-add-starry-nixos/`：本轮规范、计划、任务和契约；
- `apps/starry/nixos/README.md`：用户侧构建和运行边界；
- `apps/starry/nixos/compatibility.md`：精确运行证据和兼容性账本。
