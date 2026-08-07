# StarryNixOS Stage-2 实现进度

> 更新日期：2026-08-03
> 当前目标：StarryOS/x86_64 + NixOS-generated system closure + systemd stage 2
> 当前结论：已进入 NixOS activation 并启动 systemd，但尚未到达
> 严格成功门槛。`multi-user.target` 已进入启动事务，`ramfs`、`statx`
> mount-root/mount-ID、cgroup、procfd exec、clone3 cgroup placement、
> Unix socket timestamp、proc sysctl hostname 和 Unix listener introspection
> 分歧均已完成红绿修复。当前 journald 已识别 systemd 传入的 Varlink listener，
> 进入 handoff/notification datagram 处理；后续 journal socket 和 journald
> 启动超时，marker 未执行。

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
  -> multi-user.target 启动事务
  -> journald 越过 SO_TIMESTAMP、hostname 与 Varlink listener 识别
  -> [当前阻塞] journal socket/journald 启动超时
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

2026-08-02 复核发现，该策略此前只记录在本文档，正式 SpecKit 工件中尚未
体现，且 `plan.md` 仍保留“No commit or PR is part of this plan”的旧表述。
现已同步到 `spec.md`、`plan.md` 和 `tasks.md`：Linux ABI 修复须完成确定红测、
语义修复、同一回归转绿、格式化与受影响 crate 的 targeted clippy；若修复由
StarryNixOS 实际启动发现，还须确认真实启动越过对应首个失败边界。满足这些
门槛的独立改动可用 `git commit --no-gpg-sign` 分类提交；未通过、仍在验证或
与该成果无关的工作区改动不得混入提交。最终 marker 未出现只阻止 T028/整体
Stage 2 完成，不阻止已经独立闭环的兼容性修复提交。

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

### 7.13 `SO_TIMESTAMP` Linux 语义与红测设计（2026-08-02）

当前 journald 首个失败是
`setsockopt(syslog_fd, SOL_SOCKET, SO_TIMESTAMP, 1)` 返回
`ENOPROTOOPT`。本轮以 Linux v6.18 固定提交
`7d0a66e4bb9081d75c82ec4957c50034cb0ea449` 为语义基线：

- `net/core/sock.c::sock_set_timestamp()` 维护每 socket 的
  `SOCK_RCVTSTAMP` 状态，`getsockopt(SO_TIMESTAMP)` 只在同一旧 timeval
  ABI 选项启用时返回真；
- `net/unix/af_unix.c::unix_dgram_sendmsg()` 在接收端当时启用
  `SO_TIMESTAMP` 时，于 skb 加入接收队列之前调用 `__net_timestamp()`；
- `unix_dgram_recvmsg()` 在接收端仍启用该选项时，通过
  `__sock_recv_timestamp()` 输出 `SOL_SOCKET/SCM_TIMESTAMP`，body 为
  `struct timeval`；若启用与报文接收并发、skb 尚无时间戳，Linux 明确在
  `__sock_recv_timestamp()` 中以当前时间补戳；`MSG_PEEK` 对同一排队报文
  重复返回同一时间戳；
- x86_64 UAPI 中 `SO_TIMESTAMP == SO_TIMESTAMP_OLD == 29`，
  `SCM_TIMESTAMP == SO_TIMESTAMP`。

Starry 当前 `ax-net` Unix `DgramTransport::Packet` 已拥有数据报 payload、
原有 cmsg 和发送者地址，且 send 路径在 `try_send(packet)` 后才唤醒接收端；
因此真实入队时间应保存在该 Packet 中。备选方案“在 `recvmsg` 返回时调用
`wall_time()`”会把所有读取时间伪装为接收时间，已排除；但 Linux 对
“关闭时入队、读取前启用”的竞争窗口确实要求读取时补戳，因此实现必须区分
正常入队时间和该有界 fallback。把 Starry syscall 时间对象反向注入通用网络层
也没有必要，因为 `ax-net` 已依赖 `ax-hal`，可以在 Unix transport 入队边界
取得墙钟时间。

计划新增聚焦回归
`test-suit/starryos/qemu/system/bugfix-socket-timestamp/`，验证：

1. Unix datagram socket 的 `SO_TIMESTAMP` 初始关闭，可启用、读回和关闭；
2. 启用后，排队报文的 `recvmsg` 返回合法
   `SOL_SOCKET/SCM_TIMESTAMP` `struct timeval`；
3. 发送后延迟读取，时间戳仍落在发送/入队窗口，而不是读取时刻；
4. 关闭时入队、读取前再启用的报文按 Linux race fallback 获得读取时补戳；
5. 启用时入队但读取前关闭时不交付时间戳；
6. `MSG_PEEK` 与随后消费同一报文时返回完全相同的时间戳。

先在宿主 Linux 运行同一测试确认 oracle，再用 Podman + `.ci-cache` 在当前
Starry 内核取得确定红测。红灯成立前不修改 socket 实现，也不提交该分类。

宿主 Linux oracle 已通过全部 30 个断言：

```text
=== Results: 30 passed, 0 failed ===
STARRY_SOCKET_TIMESTAMP_PASSED
STARRY_GROUPED_TEST_PASSED: bugfix-socket-timestamp
```

首次 Podman 构建因 musl 的 `CMSG_NXTHDR` 宏在 `-Werror` 下产生
signedness warning，尚未进入 QEMU。测试只允许并期待一个 timestamp cmsg，
因此改为直接检查 `CMSG_FIRSTHDR`，避免引入与测试目标无关的宏告警；修正后
同一 Linux oracle 仍为 30/30。

随后 Podman + `.ci-cache` 聚焦 Starry QEMU 回归已取得确定红灯，测试确实
进入 guest，且失败精确来自尚未实现的 `SO_TIMESTAMP`：

```text
FAIL: SO_TIMESTAMP defaults to disabled: value=-1 len=4 errno=92 (Protocol not available)
FAIL: enable SO_TIMESTAMP: errno=92 (Protocol not available)
FAIL: SO_TIMESTAMP reports enabled: value=-1 len=4 errno=92 (Protocol not available)
...
=== Results: 14 passed, 11 failed ===
STARRY_SYSTEM_TEST_FAILED: /usr/bin/starry-test-suit/bugfix-socket-timestamp status=1
result: 0/1 case(s) passed
```

基础 Unix datagram 的发送、接收、关闭状态无 cmsg、排队和 `MSG_PEEK`
数据路径均能运行；11 个失败覆盖 option round-trip、timestamp cmsg、
enable-after-enqueue fallback 和 peek/consume timestamp 一致性。因此红测
已把实现边界限制在 Unix datagram/seqpacket 的 socket option、Packet 入队
时间元数据和 syscall cmsg 序列化，不需要改 matcher 或掩盖其他错误。

实现已保持在上述边界内：

- `ax-net` 新增通用 socket-level timestamp ancillary payload，但只有 Unix
  datagram/seqpacket transport 接受 `ReceiveTimestamp` option；
- 每个接收端持有独立的原子开关，`Bind`/`Channel` 只引用目标接收端状态；
- Packet 在 `try_send` 前按目标接收端当时状态保存 `wall_time()`；
- recv 时按当前开关决定是否交付；若开启但 Packet 无时间戳，只执行 Linux
  定义的 enable-after-enqueue fallback，并把补戳写回 Packet；
- syscall 层将该内部元数据序列化为
  `SOL_SOCKET/SCM_TIMESTAMP` `struct timeval`。

Podman + `.ci-cache` 质量门槛已通过：

```text
cargo fmt --all
ax-net clippy: 3/3 checks passed
starry-kernel clippy: 25/25 checks passed
```

同一 Starry QEMU 聚焦回归已由红转绿：

```text
=== Results: 30 passed, 0 failed ===
STARRY_SOCKET_TIMESTAMP_PASSED
STARRY_GROUPED_TEST_PASSED: bugfix-socket-timestamp
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

这证明 option round-trip、正常入队时间、关闭时不交付、
enable-after-enqueue fallback、enable-then-disable 抑制，以及
`MSG_PEEK`/消费同一时间戳均符合固定 Linux oracle。接下来复跑现有 Unix
ancillary/seqpacket 回归，再用真实 NixOS app 确认 journald 越过该边界；
在真实工作负载证据取得前不提交此分类。

相邻既有回归也已在同一 Podman 环境顺序通过：

```text
bugfix-bug-recv-qos-cmsg: 27 passed, 0 failed
syscall-test-seqpacket: 73 passed, 0 failed
```

前者确认新增 socket-level cmsg downcast 未破坏 IPv4 `IP_TOS` 和 IPv6
`IPV6_TCLASS` ancillary 交付；后者确认新增接收端 timestamp 状态未破坏
seqpacket 的消息边界、截断、`MSG_PEEK`、connect/accept、`SCM_RIGHTS`
和 `MSG_CMSG_CLOEXEC` 语义。当前剩余提交门槛仅为真实 NixOS app 越过
journald `SO_TIMESTAMP` 初始化边界。

真实 app 已使用 manifest 校验后的既有 rootfs 复跑：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64
```

日志保存为
`.ci-cache/tmp/starrynixos-after-so-timestamp.log`。本轮不再出现
`Failed to enable SO_TIMESTAMP: Protocol not available`；journald 已继续到：

```text
systemd-journald[89]: 1 unknown file descriptors passed, closing.
systemd-journald[89]: Collecting audit messages is disabled.
systemd-journald[89]: Failed to open /proc/sys/kernel/hostname: No such file or directory
Task(89, "systemd-journald") exit with code: 256
```

因此 `SO_TIMESTAMP` 分类已同时满足确定红绿回归、Linux oracle、格式化、
两个 owning crate 的 clippy、相邻 ancillary/seqpacket 回归和真实工作负载
越界证据，可以作为独立绿色成果提交。新的首个 owning boundary 是 procfs
sysctl `/proc/sys/kernel/hostname`；本轮在取得该证据后停止容器，避免
`efi_pstore`、`drm`、`fuse` 等无期限 start jobs 继续运行。
`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，T028 保持未完成；下一分类仍须先建
确定红测。

### 7.14 `/proc/sys/kernel/hostname` 红测设计（2026-08-02）

真实 journald 已把下一边界收敛到读取 `/proc/sys/kernel/hostname` 返回
`ENOENT`。Linux v6.18 的 `kernel/utsname_sysctl.c` 在 `kernel` sysctl
目录注册 `hostname`，数据来自调用者当前 UTS namespace 的
`utsname()->nodename`，通过字符串 sysctl handler 读取时输出当前 hostname
并带一个尾随换行。该值必须与 `uname(2).nodename`、`gethostname(2)` 和
`sethostname(2)` 操作的同一 UTS namespace 状态一致，不能在 procfs 中再
维护一份静态副本。

Starry 已在 `axnsproxy::UtNamespace::nodename` 中保存每 UTS namespace 的
hostname，`sys_uname` 和 `sys_sethostname` 均使用该字段；缺口仅是
`os/StarryOS/kernel/src/pseudofs/proc.rs` 的 `/proc/sys/kernel` 映射没有
暴露它。计划新增
`test-suit/starryos/qemu/system/bugfix-proc-sys-kernel-hostname/`，先验证：

1. `gethostname(2)` 和 `uname(2).nodename` 一致；
2. `/proc/sys/kernel/hostname` 可打开且表现为普通文件；
3. 读取内容精确等于当前 hostname 加一个 `\n`，无 NUL padding；
4. 读至 EOF 后可 `lseek` 回起点，并重复获得相同内容。

同一测试先在宿主 Linux 运行作为 oracle，再在当前 Starry 内核取得确定
`ENOENT` 红灯。红灯成立前不修改 procfs 实现。

宿主 Linux oracle 已通过 12/12。当前 Starry QEMU 聚焦用例取得预期红灯：

```text
PASS: gethostname succeeds
PASS: uname succeeds
PASS: gethostname matches uname nodename
FAIL: open /proc/sys/kernel/hostname: errno=2 (No such file or directory)
=== Results: 3 passed, 1 failed ===
STARRY_GROUPED_TEST_FAILED: bugfix-proc-sys-kernel-hostname
result: 0/1 case(s) passed
```

因此 UTS syscall 状态本身已工作，缺口精确位于 procfs kernel sysctl 目录。
下一步只新增一个动态只读节点：每次读取复制调用者当前 UTS namespace 的
`nodename` 到输出并追加换行；不新增全局 hostname，不修改 `uname`、
`sethostname` 或 namespace clone 逻辑。

### 7.15 `/proc/sys/kernel/hostname` 聚焦绿测（2026-08-02）

已在 `os/StarryOS/kernel/src/pseudofs/proc.rs` 的 `/proc/sys/kernel`
目录增加动态只读 `hostname` 节点。每次打开/读取时从当前 task 的
`nsproxy.uts_ns.nodename` 取得当前 UTS namespace hostname，截断首个 NUL
后追加一个换行；procfs 不保存第二份 hostname 状态。

实现过程中，最初的 `c_char as u8` 转换在 x86_64 可用，但完整
`starry-kernel` clippy 的 aarch64 system 配置将其判定为
`unnecessary_cast`，因为不同架构上的 `c_char` signedness 不同。最终改用
`to_ne_bytes()[0]` 复制底层单字节表示，并重新执行完整矩阵，没有跳过
aarch64 配置。

Podman + `.ci-cache` 验证结果：

```text
cargo fmt --all: passed
starry-kernel clippy: 25/25 checks passed
bugfix-proc-sys-kernel-hostname:
  PASS: gethostname succeeds
  PASS: uname succeeds
  PASS: gethostname matches uname nodename
  PASS: open /proc/sys/kernel/hostname
  PASS: hostname sysctl is a regular file
  PASS: read hostname sysctl
  PASS: hostname sysctl has exact length
  PASS: hostname sysctl equals current hostname plus newline
  PASS: hostname sysctl has no NUL padding
  PASS: hostname sysctl reaches EOF
  PASS: hostname sysctl seeks to start
  PASS: hostname sysctl repeats the same UTS value
  === Results: 12 passed, 0 failed ===
  STARRY_PROC_SYS_HOSTNAME_PASSED
  result: 1/1 case(s) passed
```

该聚焦回归已完成确定性 `ENOENT` 红灯到 12/12 绿灯。下一步复跑现有 UTS
namespace 回归，随后用同一已验证 NixOS 镜像确认 journald 越过
`/proc/sys/kernel/hostname`；真实 workload 越界前不提交本分类。

现有 `qemu/system/syscall-test-namespace` 也已在相同 Podman 环境通过：

```text
UTS / PID / USER namespace: 13 pass, 0 fail
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

其中 UTS 子用例确认 child `unshare(CLONE_NEWUTS)` 并 `sethostname` 后只改变
child hostname，parent hostname 保持不变。该结果证明新增 procfs 节点没有
破坏现有 UTS namespace 复制和隔离路径。

真实 StarryNixOS 复验也已越过该边界：

```text
Hostname set to <starrynixos>.
systemd-journald[89]: 1 unknown file descriptors passed, closing.
systemd-journald[89]: Collecting audit messages is disabled.
Task(89, "systemd-journald") exit with code: 256
```

与修复前相比，`Failed to open /proc/sys/kernel/hostname: No such file or
directory` 已消失，systemd 成功读取并设置 `starrynixos` hostname，journald
继续执行到原失败点之后。因此真实 workload 越界门槛已满足。

本次 app 中多个非核心 systemd jobs 标记为 `no limit`，在取得越界证据后
停止 Podman 容器，最终退出码 137 是有界停止结果，不是通过或内核崩溃。
完整日志保存于：

```text
.ci-cache/tmp/starrynixos-after-proc-hostname.log
```

新的首个可见问题是 journald 在 audit-disabled 信息后以状态 1 退出，但现有
日志没有给出失败 syscall、errno 或资源对象。当前证据不足以设计一个必然失败
且指向正确拥有子系统的回归，因此不猜测修复；下一步需要 syscall 级观测或最小
复现先把该退出收敛到具体 Linux 语义。`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，
T028 保持未完成。

### 7.16 journald Varlink 监听 fd 识别红测设计（2026-08-02）

继续读取与镜像一致的 systemd v260.2 固定 commit
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 后，journald 初始化顺序已经
收敛：

```text
Collecting audit messages is disabled.
  -> manager_open_varlink()
  -> manager_map_seqnum_file()
  -> manager_open_kernel_seqnum()
  -> manager_open_hostname()
```

真实日志在 audit-disabled 之前还报告：

```text
systemd-journald[89]: 1 unknown file descriptors passed, closing.
```

`manager_init()` 使用
`sd_is_socket_unix(fd, SOCK_STREAM, 1, varlink_socket, 0)` 识别 systemd
传入的 Varlink 监听 fd。systemd 同一固定 commit 中的实现依次要求：

1. `fstat(fd)` 报告 `S_IFSOCK`；
2. `getsockopt(SOL_SOCKET, SO_TYPE)` 报告 `SOCK_STREAM`；
3. `getsockopt(SOL_SOCKET, SO_ACCEPTCONN)` 在 `listen(2)` 前为 0、之后为 1；
4. `getsockname(2)` 报告匹配的 `AF_UNIX` pathname 和长度。

Linux man-pages `socket(7)` 明确规定 `SO_ACCEPTCONN` 是只读 `int`，未监听时
返回 0，已由 `listen(2)` 标记为接受连接时返回 1。Linux v6.18 固定 commit
`7d0a66e4bb9081d75c82ec4957c50034cb0ea449` 的
`net/core/sock.c:sock_getsockopt()` 以 `sk->sk_state == TCP_LISTEN` 生成该值。

Starry 现有 socket option dispatch 已支持 `SO_TYPE`、`SO_PROTOCOL` 和
`SO_DOMAIN`，但没有 `SO_ACCEPTCONN`；因此 systemd 的监听 fd 判定会在第三步
得到 `ENOPROTOOPT`。下一步新增
`test-suit/starryos/qemu/system/bugfix-unix-listener-introspection/`，使用同一
C 程序先跑宿主 Linux oracle，再跑当前 Starry：

- 创建 pathname `AF_UNIX/SOCK_STREAM` socket；
- 验证 socket inode 和 `SO_TYPE`；
- 验证 `SO_ACCEPTCONN` 在 `listen` 前为 0、之后为 1，且 `optlen` 精确；
- 验证 `getsockname` 的 family、pathname 和返回长度；
- 验证 duplicated listener fd 保留相同 introspection 结果。

只有当前 Starry 在 `SO_ACCEPTCONN` 处取得确定红灯后，才修改 socket option
实现；不能把 journald status 1 直接等同于该推断并先写修复。

Podman 中的宿主 Linux oracle 已执行同一程序并通过全部 14 项检查：

```text
=== Results: 14 passed, 0 failed ===
STARRY_UNIX_LISTENER_INTROSPECTION_PASSED
```

测试最初使用 `strncpy` 复制 pathname，宿主编译器在项目的 `-Werror` 配置下以
`-Wstringop-truncation` 拒绝构建。测试随后改为先验证长度，再用精确
`memcpy` 复制包含终止 NUL 的 pathname；这只修正测试构造方式，不改变被测
socket 语义。

当前 Starry 聚焦测试也已取得确定红灯：

```text
PASS: create Unix stream socket
PASS: fstat reports a socket inode
PASS: SO_TYPE reports SOCK_STREAM
FAIL: SO_ACCEPTCONN is zero before listen: errno=92 (Protocol not available)
PASS: bind pathname Unix socket
PASS: listen on Unix stream socket
FAIL: SO_ACCEPTCONN is one after listen: errno=92 (Protocol not available)
PASS: getsockname reports bound pathname
PASS: duplicate listener fd
FAIL: duplicated fd remains a listening socket: errno=92 (Protocol not available)
=== Results: 11 passed, 3 failed ===
```

失败严格局限于三次 `SO_ACCEPTCONN` 查询，均为 Linux 对该 option 不支持时的
`ENOPROTOOPT`；`fstat`、`SO_TYPE`、`bind`、`listen`、`getsockname` 和 fd
duplicate 路径均已通过。因此红测已经把修改范围限定到 socket option
introspection 及其底层监听状态查询，下一步可以开始实现，不需要改动
`getsockname`、fd 复制或 Unix pathname 逻辑。

继续补充 bind/listen 状态边界后，Linux oracle 通过 17/17；当前 Starry 红测
扩展为 12/17，除四次 `SO_ACCEPTCONN` 查询返回 `ENOPROTOOPT` 外，还确认
pathname Unix stream 在只 bind、未 listen 时错误地允许 connect 成功。该结果
证明不能只在 syscall 层伪造 option 值，必须修正拥有连接队列的 Unix transport
状态机。

实现将只读 `SocketOps::is_listening()` 作为 transport-independent 查询：

- TCP 直接读取现有 `State::Listening`；
- Unix stream 与 seqpacket 由 transport 持有并与 namespace bind slot 共享同一
  `AtomicBool`；
- `listen()` 以 Release 发布监听状态，`connect()`、`accept()` 和
  `SO_ACCEPTCONN` 以 Acquire 读取；
- Unix bind 只准备地址和连接队列，不再等价于进入监听状态；
- datagram、raw 等不支持监听的 socket 使用 trait 默认值 0。

这避免了 syscall 层和 transport 层各维护一份 listener 状态，同时恢复 Linux 的
bind 后、listen 前 `connect()` 返回 `ECONNREFUSED` 语义。

Podman + `.ci-cache` 聚焦绿测已通过：

```text
PASS: SO_ACCEPTCONN is zero before listen
PASS: SO_ACCEPTCONN stays zero after bind
PASS: connect is refused before listen
PASS: SO_ACCEPTCONN is one after listen
PASS: duplicated fd remains a listening socket
=== Results: 17 passed, 0 failed ===
STARRY_UNIX_LISTENER_INTROSPECTION_PASSED
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

首次绿测构建在 `opt.rs` 缺少 `SocketOps` trait import 时按编译错误停止；补齐
导入并重新执行后才取得上述结果。当前仍需完成 `ax-net`、`starry-kernel`
targeted clippy、相邻 accept/seqpacket 回归和真实 StarryNixOS 越界验证，完成前
不提交该分类。

质量门禁和相邻回归现已完成：

```text
ax-net targeted clippy: 3/3 checks passed
starry-kernel targeted clippy: 25/25 checks passed
qemu/system/syscall-test-accept4: 29 pass, 0 fail
qemu/system/syscall-test-seqpacket: 73 pass, 0 fail
```

`starry-kernel` clippy 包含两个 aarch64 system 配置，未因本次 x86_64 workload
只验证单架构而跳过。accept4 回归确认未 listen 的 Unix stream 仍返回 `EINVAL`，
以及 stream/seqpacket 的 listen/accept 正常；seqpacket 回归确认连接式
bind/listen/connect/accept 和消息、SCM_RIGHTS 语义均未退化。

clippy 初次尝试没有进入代码检查，因为先前临时容器生成的 workspace `target/`
缓存不可写。最终没有递归修改权限，而是将 `CARGO_TARGET_DIR` 隔离到
`.ci-cache/target` 后完成全部 3+25 项检查。grouped 相邻回归在 cache miss 时
需要 fakeroot，最终使用与 Ubuntu 24.04 容器 glibc 匹配的临时安装完成资产提取；
该差异仅影响测试准备，不改变 QEMU 内核或 guest 语义。

真实 StarryNixOS workload 越界验证现已完成。使用同一已验证镜像和
Podman + `.ci-cache` 运行 180 秒后，完整日志计数为：

```text
unknown file descriptors passed: 0
Collecting audit messages: 1
Received handoff timestamp: 7
lacking valid credential: 2
STARRY_NIXOS_SYSTEM_PASSED: 0
```

旧的 `1 unknown file descriptors passed, closing.` 已完全消失，证明
`sd_is_socket_unix()` 已通过 `SO_ACCEPTCONN` 和 Unix listener 状态检查。
journald 随后进入 handoff timestamp 与 notification datagram 处理，新的可见
诊断和终态为：

```text
Received handoff timestamp message without valid credentials. Ignoring.
Got notification datagram lacking valid credential information, ignoring.
systemd-journalctl.socket: Starting timed out. Stopping.
systemd-journalctl.socket: Failed with result 'timeout'.
systemd-journald.service: start operation timed out. Terminating.
```

外层 `timeout` 以 124 结束本次有界运行；这不是 Starry 内核崩溃或测试通过。
当前证据足以确认本分类跨越真实触发边界，因此 listener introspection 修复可以
作为独立、已验证成果提交。上述 credential datagram 与 socket timeout 只记录为
下一诊断边界；在建立新的 Linux oracle 和确定红测前，不据此猜测内核修复。
日志位于 `.ci-cache/tmp/starrynixos-after-so-acceptconn.log`。
`STARRY_NIXOS_SYSTEM_PASSED` 未出现，T028 保持未完成。

### 7.17 systemd notification credential 红测设计（2026-08-02）

真实 workload 越过 Varlink listener 后连续报告：

```text
Received handoff timestamp message without valid credentials. Ignoring.
Got notification datagram lacking valid credential information, ignoring.
```

固定 systemd v260.2 commit
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的对应调用链已经确认：

- `src/core/manager.c` 为 handoff timestamp `AF_UNIX/SOCK_DGRAM`
  socketpair 的接收端启用 `SO_PASSCRED`；
- 同文件为 service notification pathname datagram socket 启用
  `SO_PASSCRED`；
- `manager_dispatch_handoff_timestamp()` 从 `recvmsg` 控制消息查找
  `SOL_SOCKET/SCM_CREDENTIALS`，缺失或 PID 无效时打印第一条诊断；
- `src/shared/notify-recv.c:notify_recv_with_fds()` 同样要求有效
  `SCM_CREDENTIALS`，缺失时打印第二条诊断；
- journald 的 syslog/native datagram sockets 也启用 `SO_PASSCRED` 并使用
  同一 `struct ucred` 布局。

Starry 当前实现存在可直接验证的语义缺口：

- Unix stream/datagram 的 `SetSocketOption::PassCredentials` 为空分支，没有保存
  receiver 状态；
- `GetSocketOption::PassCredentials` 不写回实际值；
- Unix packet 只保存 payload、显式 cmsg、sender address 和 timestamp，没有
  发送时 credential snapshot；
- syscall `recvmsg` 只序列化 `SCM_RIGHTS`、IP cmsg 和 `SCM_TIMESTAMP`，没有
  `SCM_CREDENTIALS`。

因此下一步新增
`test-suit/starryos/qemu/system/bugfix-unix-passcred/`，先用同一 C 程序执行
Linux oracle，再在当前 Starry 建立确定红灯。测试范围限定为 systemd 当前使用的
Unix datagram 自动 credential 传递：

- `SO_PASSCRED` 初始值、启用回读和禁用回读；
- socketpair 在 `fork()` 后由 child 发送，receiver 必须得到发送时 child 的
  PID、real UID 和 real GID，不能使用 socket 创建者 PID；
- `MSG_PEEK` 和随后消费同一 datagram 都必须观察相同 credential；
- receiver 启用 `SO_PASSCRED` 但控制缓冲区为零时必须设置 `MSG_CTRUNC`；
- 禁用后普通 datagram 不应携带 `SCM_CREDENTIALS`。

显式发送自定义 `SCM_CREDENTIALS` 涉及 capability、PID namespace 和
credential override 校验，不是当前 systemd 诊断的必要前提，本红测不扩展到该
独立语义。只有 Linux oracle 通过且当前 Starry 在上述自动传递路径确定失败后，
才修改 `ax-net`/syscall cmsg 边界。

Podman 中的 Linux oracle 已通过全部 23 项：

```text
=== Results: 23 passed, 0 failed ===
STARRY_UNIX_PASSCRED_PASSED
```

测试第一次进入 grouped C 交叉构建时，musl 的 `CMSG_NXTHDR` 宏在项目
`-Werror` 下触发 `-Wsign-compare`。由于测试控制缓冲区只容纳一个
`struct ucred`，测试改为直接验证 `CMSG_FIRSTHDR`；修正后重新执行 Linux
oracle 仍为 23/23。

当前 Starry 的同一测试得到确定红灯：

```text
PASS: SO_PASSCRED is disabled initially
PASS: enable SO_PASSCRED on receiver
FAIL: SO_PASSCRED enable state reads back
PASS: receive first child datagram
FAIL: receive SCM_CREDENTIALS automatically
FAIL: SCM_CREDENTIALS reports sending child PID
FAIL: SCM_CREDENTIALS reports sender real UID
FAIL: SCM_CREDENTIALS reports sender real GID
FAIL: MSG_PEEK reports credentials
FAIL: MSG_PEEK preserves sender PID
FAIL: consume after MSG_PEEK reports credentials
FAIL: peek and consume report identical credentials
PASS: receive datagram without control buffer
FAIL: missing credential control space sets MSG_CTRUNC
PASS: SO_PASSCRED disable state reads back
PASS: disabled SO_PASSCRED emits no credentials
=== Results: 13 passed, 10 failed ===
```

因此修改范围已经收敛到：

1. receiver-owned `SO_PASSCRED` 状态及其 get/set；
2. 每次 send syscall 取得发送线程的 real PID/UID/GID snapshot；
3. Unix 消息在目标 receiver 启用该选项时附加自动 credential cmsg；
4. syscall `recvmsg` 将内部 credential cmsg 序列化为
   `SOL_SOCKET/SCM_CREDENTIALS`；
5. 控制缓冲区容不下自动 credential 时设置 `MSG_CTRUNC`。

不得使用 socket 创建者 PID 代替发送时 PID，也不得在 syscall receive 层查询
当前 receiver credential 伪造 sender identity。

### 7.18 Unix passcred 首轮修复验证与真实 workload 差分（2026-08-02）

上述聚焦用例已从 13/23 转为全部通过：

```text
=== Results: 23 passed, 0 failed ===
STARRY_UNIX_PASSCRED_PASSED
STARRY_GROUPED_TESTS_PASSED
```

当前实现完成了 receiver-owned `SO_PASSCRED` get/set、发送时 real
PID/UID/GID snapshot、Unix datagram/seqpacket/stream 自动
`SCM_CREDENTIALS`、`MSG_PEEK` 保留和 `MSG_CTRUNC`。质量门禁及相邻回归均
通过：

```text
ax-net targeted clippy: 3/3 checks passed
starry-kernel targeted clippy: 25/25 checks passed
qemu/system/syscall-test-cmsg-cloexec: 62/62
qemu/system/test-unix-cmsg-byte-marks: passed
qemu/system/test-unix-msg-peek: 16/16
qemu/system/syscall-test-seqpacket: 73/73
```

真实 StarryNixOS app 复跑仍报告：

```text
Received handoff timestamp message without valid credentials. Ignoring.
```

固定 systemd v260.2 源码进一步确认 handoff sender 在
`src/core/exec-invoke.c` 使用 `write(handoff_timestamp_fd, ...)`，而不是
`send(2)`/`sendmsg(2)`。Starry 的普通 `write(2)` 当前通过
`Socket::write -> send(..., SendOptions::default())`，没有注入发送线程
credential；现有聚焦用例的 child sender 则使用 `send(2)`，因此首轮绿测没有
覆盖 systemd 的实际发送入口。

下一步在同一用例增加 Unix datagram socket 上的直接 `write(2)` credential
断言，先确认 Linux oracle 通过且当前 Starry 稳定失败，再让 socket file
`write(2)` 复用与 send syscall 相同的发送 credential snapshot。该差分闭环和
真实 app 越界完成前，本分类不提交，T028 保持未完成。

该扩展测试现已形成预期差分：

```text
Linux oracle: 26 passed, 0 failed
Starry before fix: 24 passed, 2 failed
FAIL: write(2) automatically reports SCM_CREDENTIALS
FAIL: write(2) credentials report sending child PID
```

红测日志为 `.ci-cache/tmp/unix-passcred-write-red.log`。修复将当前线程的 real
PID/UID/GID snapshot 集中到 `Socket::with_current_sender_credentials()`，
`send*` syscall 和 socket file `write(2)`/`writev(2)` 都通过该边界构造
`SendOptions`；后续必须重新完成聚焦测试、Clippy、相邻回归和真实 NixOS app
验证。

聚焦修复后已转绿：

```text
=== Results: 26 passed, 0 failed ===
STARRY_UNIX_PASSCRED_PASSED
STARRY_GROUPED_TESTS_PASSED
```

日志为 `.ci-cache/tmp/unix-passcred-write-green.log`。`cargo fmt --all -- --check`
已通过；当前仍需完成受影响 crate 的 Clippy、相邻 socket 回归和真实 workload
越界验证。

`starry-kernel` targeted Clippy 随后通过全部 25 项检查，包含两个 aarch64
system 配置；日志为
`.ci-cache/tmp/unix-passcred-write-clippy-starry-kernel.log`。下一步继续执行
相邻 Unix cmsg/peek/seqpacket 回归和真实 app。

相邻 Unix socket QEMU 回归全部通过：

```text
syscall-test-cmsg-cloexec: 62 passed, 0 failed
test-unix-cmsg-byte-marks: 18 passed, 0 failed
test-unix-msg-peek: 16 passed, 0 failed
syscall-test-seqpacket: 73 passed, 0 failed
```

日志为 `.ci-cache/tmp/unix-passcred-write-adjacent.log`。当前分类只剩真实
StarryNixOS app 越界验证；`git diff --check` 已通过。

真实 StarryNixOS app 已使用复用且重新校验的 NixOS rootfs 完成 240 秒有界
复跑。完整日志计数为：

```text
Received handoff timestamp message without valid credentials: 0
Got notification datagram lacking valid credential information: 0
STARRY_NIXOS_SYSTEM_PASSED: 0
```

这证明 systemd 的 handoff `write(2)` 和 notification datagram 均已越过原始
`SO_PASSCRED`/`SCM_CREDENTIALS` 触发边界。运行继续进入 systemd unit 启动事务，
当前终态仍是：

```text
systemd-journalctl.socket: Starting timed out. Stopping.
systemd-journalctl.socket: Failed with result 'timeout'.
systemd-journald.service: start operation timed out. Terminating.
systemd-sysctl.service: start operation timed out. Terminating.
```

外层有界运行以 124 结束；日志位于
`.ci-cache/tmp/starrynixos-after-passcred-write.log`。因此 Unix passcred 分类
满足独立提交条件，但完整 Stage-2 acceptance 仍未通过，T028 保持未完成。
journald/sysctl timeout 是下一诊断边界，必须重新建立确定红测后再修改。

### 7.19 signalfd epoll 唤醒边界与红测设计（2026-08-02）

passcred 修复后的真实日志显示，超时并不是 journald/sysctl 进程仍在执行：

```text
Task(89, "systemd-journald") exit with code: 256
Send signal SIGCHLD to process 1
Task(91, "systemd-sysctl") exit with code: 256
Send signal SIGCHLD to process 1
```

但上述退出后 PID 1 没有再次出现
`waitid(P_ALL, 0, ..., WEXITED|WNOHANG|WNOWAIT)`，最终 systemd 仍将两个 PID
视为 active 并在超时后发送 SIGTERM/SIGKILL。固定 systemd v260.2 commit
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的调用链为：

1. PID 1 阻塞 `SIGCHLD`，通过 `signalfd` 加入 `sd-event` epoll；
2. signalfd 就绪后启用 deferred `manager_dispatch_sigchld()`；
3. dispatcher 先用 `waitid(P_ALL, ..., WNOWAIT)` 观察 zombie，再用
   `waitid(P_PID, ...)` 回收。

Starry 当前 `send_signal_to_process()` 会在 process-directed signal 入队后唤醒
每个线程的 `Thread::signalfd_waker`；但 `Signalfd::register()` 只把 poll
waker 注册到 `Signalfd` 自己的 `poll_rx`。这两个 `PollSet` 没有连接，因此：

- 信号在 `epoll_wait()` 前已经 pending 时，`Signalfd::poll()` 仍可偶然看到；
- `epoll_wait()` 已阻塞后才到达的 signal 只唤醒 thread `signalfd_waker`，
  无法唤醒注册在私有 `poll_rx` 上的 event loop。

这与当前日志中“PID 1 曾处理一批 SIGCHLD，后续退出不再触发 waitid”一致。
下一步新增
`test-suit/starryos/qemu/system/bugfix-signalfd-epoll-wakeup/`，直接验证：

1. parent 先阻塞 `SIGCHLD`，创建 nonblocking signalfd 并注册到 epoll；
2. child 延迟退出，确保 parent 已进入 `epoll_wait()`；
3. epoll 必须因 signalfd 的 `EPOLLIN` 返回；
4. signalfd 必须读到该 child 的 `SIGCHLD`；
5. 随后按 systemd 相同的 `waitid(WNOWAIT)` + `waitid(P_PID)` 顺序观察并回收
   child。

先运行同一 Linux oracle，再在当前 Starry 取得确定红灯。红灯成立前不修改
signalfd/poll 实现，也不据 journald timeout 扩展其他 syscall。

Linux oracle 已通过全部 10 项：

```text
PASS: epoll wakes when blocked SIGCHLD reaches signalfd
PASS: read SIGCHLD from signalfd
PASS: waitid WNOWAIT observes child after signalfd wake
PASS: waitid P_PID reaps observed child
=== Results: 10 passed, 0 failed ===
```

Starry 首次 grouped 准备因 cache miss 且 CI 镜像缺少 `fakeroot`，未进入 guest；
改用一次性 root-mapped Podman 容器安装 `fakeroot` 后取得确定红灯：

```text
FAIL: epoll wakes when blocked SIGCHLD reaches signalfd
PASS: read SIGCHLD from signalfd
PASS: signalfd reports exiting child
PASS: waitid WNOWAIT observes child after signalfd wake
PASS: waitid P_PID reaps observed child
=== Results: 9 passed, 1 failed ===
```

日志为 `.ci-cache/tmp/signalfd-epoll-red.log`。信号入队、signalfd dequeue 和
systemd 使用的两步 waitid 均正常，唯一失败是等待中的 epoll 没有收到 signal
arrival wakeup。因此实现范围限制在 `Signalfd::register()`：保留私有
`poll_rx` 处理 mask update/剩余 pending signal，同时注册当前线程的
`signalfd_waker` 处理新 signal 到达；不修改 signal queue、waitid 或 epoll
通用逻辑。

实现后同一 Podman + `.ci-cache` 聚焦用例已转绿：

```text
PASS: epoll wakes when blocked SIGCHLD reaches signalfd
PASS: read SIGCHLD from signalfd
PASS: signalfd reports exiting child
PASS: waitid WNOWAIT observes child after signalfd wake
PASS: waitid P_PID reaps observed child
=== Results: 10 passed, 0 failed ===
STARRY_SIGNALFD_EPOLL_WAKEUP_PASSED
STARRY_GROUPED_TESTS_PASSED
```

`cargo fmt --all -- --check` 同时通过，日志为
`.ci-cache/tmp/signalfd-epoll-green.log`。随后 `starry-kernel` targeted Clippy
通过全部 25 项检查，包含两个 aarch64 system 配置：

```text
clippy summary: 1 package(s), 25 check(s), 1 package(s) passed, 0 package(s) failed
passed checks: 25, failed checks: 0
```

日志为 `.ci-cache/tmp/signalfd-epoll-clippy-starry-kernel.log`。相邻 QEMU 回归
也已通过：

```text
qemu/system/syscall-test-signalfd4: 44 passed, 0 failed
qemu/system/bugfix-bug-sigwaitinfo-blocked-sigchld: 3 passed, 0 failed
qemu/system/zombie-bugfix-bug-waitid-basic: passed
qemu/system/bugfix-bug-epoll-topology: 32 passed, 0 failed
```

对应日志为：

- `.ci-cache/tmp/signalfd-epoll-regression-signalfd4.log`
- `.ci-cache/tmp/signalfd-epoll-adjacent-regressions.log`
- `.ci-cache/tmp/signalfd-epoll-regression-epoll-topology.log`

曾尝试使用 `syscall-test-epoll-eventfd` 作为通用 epoll 回归，但该用例当前安装
在 `starry-known-fail`，显式选择后正常测试目录为空，因此没有将其失败计入代码
回归证据，改用已启用的 epoll topology 用例。

当前分类只剩真实 StarryNixOS workload 越界验证：必须确认 child exit 后 PID 1
重新执行 `waitid`，且原 journald/sysctl timeout 消失或转移到新的精确边界。
该真实运行完成前不提交，T028 保持未完成。

真实 StarryNixOS app 随后使用复用且通过 manifest 校验的 rootfs 运行。journald
退出后，PID 1 已在约 0.24 秒内重新进入 systemd 的 child reap 流程：

```text
[ 40.705929] Task(89, "systemd-journald") exit with code: 256
[ 40.756948] Send signal SIGCHLD to process 1
[ 40.992712] sys_waitid <= idtype: 0, id: 0,
              options: WNOHANG | WEXITED | WNOWAIT
[ 41.286211] sys_waitid <= idtype: 1, id: 89, options: WEXITED
[ 41.287224] sys_waitid <= idtype: 0, id: 0,
              options: WNOHANG | WEXITED | WNOWAIT
```

systemd 不再把已经退出的 journald 长时间保留为 active；它立即报告并回收该
进程，然后进入重启逻辑：

```text
systemd-journald.service: Main process exited, code=exited, status=1/FAILURE
systemd-journald.service: Failed with result 'exit-code'.
systemd-journald.service: Scheduled restart job, restart counter is at 1.
```

这证明真实 workload 已跨过 signalfd/epoll 的原始触发边界，旧的 journald
start timeout 已消失。app runner 的 fail regex 在约 41 秒遇到即时 service
failure 后主动结束运行，因此本次没有继续观察 `systemd-sysctl` 的终态，也未
出现 `STARRY_NIXOS_SYSTEM_PASSED`。日志为
`.ci-cache/tmp/starrynixos-after-signalfd-epoll.log`。

当前日志中 journald 退出前出现 `Unimplemented syscall: keyctl (tid=89)`，但
仅凭相邻时序不能认定它就是退出根因；下一分类必须先建立 Linux/Starry
确定差分或取得更直接的用户态错误证据。signalfd 分类已满足独立提交门槛，
但 T028 仍保持未完成。

### 7.20 journald 即时退出的下一诊断边界（2026-08-02）

固定 systemd v260.2 commit
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的 `setup_keyring()` 明确将
`KEYCTL_JOIN_SESSION_KEYRING` 返回 `ENOSYS` 视为“不支持 kernel keyring”，
只记录 debug 后继续启动。因此当前 Starry 的 `keyctl` ENOSYS 不是 journald
退出的直接原因，不应据此实现完整 keyring syscall。

journald 最后一条可见用户态消息为：

```text
systemd-journald[89]: Collecting audit messages is disabled.
```

在固定源码中，该消息之后的初始化顺序是：

1. `manager_open_varlink()` 接管继承的 Unix stream listener；
2. 创建、`posix_fallocate()` 并 `MAP_SHARED` 映射主 seqnum 文件；
3. 打开 kernel seqnum 和 `/proc/sys/kernel/hostname`；
4. 注册 signals 和 memory-pressure event；
5. 读取 cgroup root；
6. 打开 runtime/system journal 文件。

`sd_varlink_server_listen_fd()` 对继承 listener 的同步操作限于设置
`O_NONBLOCK`、`FD_CLOEXEC`、可忽略失败的 `SO_PASSRIGHTS`，以及把 listener
注册到 event loop。现有日志仍没有暴露上述哪一步返回了错误。下一步不修改
syscall，而是在 NixOS service 配置中临时设置
`SYSTEMD_LOG_TARGET=console` 和 `SYSTEMD_LOG_LEVEL=debug`，重建 app-owned
rootfs 后取得 journald 自身的精确错误消息。诊断结束后再决定是否保留该配置；
没有确定 Linux/Starry 差分前不实现候选 syscall。

第一次尝试直接在 `ghcr.io/rcore-os/tgoskits-container:latest` 中不复用
rootfs 执行：

```text
cargo xtask starry app qemu -t nixos --arch x86_64
```

在进入 QEMU 前由 app-owned builder 明确失败：

```text
StarryNixOS artifact error: required command 'nix' is unavailable
```

该 CI 容器不包含 Nix，因此不能承担锁定 NixOS artifact 的构建；这不是
StarryOS 或 journald 的行为结果。后续应在具备 Nix 的 x86_64 宿主按同一
`build-rootfs.sh` 重建并验证 artifact，再在 Podman 中设置
`STARRY_NIXOS_REUSE_ROOTFS=1` 运行 QEMU。日志保存在
`.ci-cache/tmp/starrynixos-journald-console-debug.log`。

### 7.21 journald syscall trace 与 `SO_SNDBUF` 候选排除（2026-08-02）

宿主使用锁定 flake 和显式 e2fsprogs 工具路径成功重建并发布 app-owned
rootfs：

```text
system=/nix/store/d7fqs6pm0jw30yq0wbrpahvzaynm67h0-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=5d6adfe63faec9e0fd1cf654864cccf1a36bbd8e65a6f663d84c23db9964a9d9
```

构建日志为 `.ci-cache/tmp/starrynixos-host-rootfs-rebuild.log`。临时
`SYSTEMD_LOG_LEVEL=debug` / `SYSTEMD_LOG_TARGET=console` unit drop-in 已确认
进入新 artifact，但 journald 仍未在 console 输出直接错误。全局 kernel Debug
日志量过大，180 秒仅推进到 guest 4.5 秒，已终止；日志为
`.ci-cache/tmp/starrynixos-journald-kernel-debug.log`。

随后在 syscall dispatcher 中临时仅跟踪进程名 `systemd-journald`，取得完整
退出前调用链，日志为
`.ci-cache/tmp/starrynixos-journald-syscall-trace.log`。其中
`signalfd4` 更新已有 fd 返回 `EINVAL` 后 journald 仍继续执行，因此不能据此
认定退出原因：

```text
signalfd4(-1, ..., SFD_CLOEXEC|SFD_NONBLOCK) = 10
signalfd4(10, ..., SFD_CLOEXEC|SFD_NONBLOCK) = EINVAL
```

退出前可见新建 Unix datagram socket 后读取发送缓冲区：

```text
socket(AF_UNIX, SOCK_DGRAM|SOCK_CLOEXEC, 0) = 3
getsockopt(3, SOL_SOCKET, SO_SNDBUF, ...) = ENOPROTOOPT
setsockopt(3, SOL_SOCKET, SO_SNDBUF, ...) = 0
getsockopt(3, SOL_SOCKET, SO_SNDBUF, ...) = ENOPROTOOPT
setsockopt(3, SOL_SOCKET, SO_SNDBUFFORCE, ...) = ENOPROTOOPT
exit_group(1)
```

Starry syscall option 映射已经把 `SO_SNDBUF` 解析为
`GetSocketOption::SendBuffer`，而 `net/ax-net/src/unix/stream.rs` 已实现该
查询，`net/ax-net/src/unix/dgram.rs` 当前没有对应分支。因此
`ENOPROTOOPT` 的拥有边界初步收敛到 ax-net Unix datagram socket option
实现，而不是 syscall 常量或 option 解码层。但锁定 systemd v260.2 源码确认
这些调用来自 `fd_inc_sndbuf()`，其返回值在 journald 的 notify socket 和日志
发送路径中被显式忽略；trace 中两组重复调用也发生在错误报告/退出清理阶段。
因此 `SO_SNDBUF` 虽是独立 Linux 语义缺口，却不是本次 journald 退出根因，
当前不修改 ax-net。

同一 trace 中更早且未被忽略的失败是：

```text
signalfd4(-1, ..., SFD_CLOEXEC|SFD_NONBLOCK) = 10
epoll_ctl(..., fd=10, EPOLL_CTL_ADD, ...) = 0
signalfd4(10, ..., SFD_CLOEXEC|SFD_NONBLOCK) = EINVAL
```

第二次 `signalfd4` 失败后，journald 立即删除已注册 event source、关闭 fd 并
退出。固定 systemd v260.2 的 `sd_event_add_signal()` 会为同一优先级复用一个
signalfd；新增第二个 signal 时以原 fd 和
`SFD_NONBLOCK|SFD_CLOEXEC` 更新合并后的 mask。该失败从
`manager_setup_signals()` 返回并终止 `manager_new()`。

Linux v6.18 `do_signalfd4()` 只拒绝未知 flag。`ufd != -1` 时它更新
`ctx->sigmask` 并唤醒 waiters；合法的 `SFD_CLOEXEC`/`SFD_NONBLOCK` 不得
返回 `EINVAL`，也不改变已有 fd 的 descriptor/status flags。宿主 Linux
聚焦 oracle 全部通过：

```text
=== Results: 10 passed, 0 failed ===
STARRY_SIGNALFD_MASK_UPDATE_FLAGS_PASSED
```

新增
`test-suit/starryos/qemu/system/bugfix-signalfd-mask-update-flags/` 后，当前
Starry 取得确定红灯：

```text
FAIL: update existing signalfd with valid flags returns same fd: errno=22
FAIL: systemd-style repeated flags update returns same fd: errno=22
=== Results: 8 passed, 2 failed ===
```

日志为 `.ci-cache/tmp/signalfd-mask-update-flags-red.log`。实现范围限制在
`os/StarryOS/kernel/src/syscall/fs/signalfd.rs`：已有 fd 时仅更新 mask，
不再拒绝 `SFD_CLOEXEC`，也不根据本次 flags 改写 nonblocking 状态。

修复后的聚焦 Starry 回归已取得：

```text
=== Results: 10 passed, 0 failed ===
STARRY_SIGNALFD_MASK_UPDATE_FLAGS_PASSED
```

日志为 `.ci-cache/tmp/signalfd-mask-update-flags-green.log`。相邻
`qemu/system/syscall-test-signalfd4` 随后取得 `43 pass, 1 fail`；唯一失败
来自旧测试仍断言“更新已有 signalfd 时传 `SFD_CLOEXEC` 返回 `EINVAL`”，
与 Linux v6.18 和本次宿主 oracle 相反，因此属于旧测试契约错误，不是修复
回归。该次日志为 `.ci-cache/tmp/signalfd4-adjacent.log`。

尝试在宿主直接编译并执行完整旧测试时，还发现其 127 字节短读场景使用了
实际仅 64 字节的目标对象；glibc fortify 在 syscall 前以 buffer overflow
终止进程。相邻测试需同时把该对象扩大到 127 字节，才能安全地作为 Linux
差分用例运行。两项测试修正都只恢复既有 Linux ABI 契约，不扩大 kernel
实现范围。

修正旧测试后，Starry 相邻回归取得：

```text
DONE: 44 pass, 0 fail
STARRY_SYSTEM_TEST_PASSED: /usr/bin/starry-test-suit/test-signalfd4
```

日志为 `.ci-cache/tmp/signalfd4-adjacent-green.log`。同一源码在宿主 Linux
取得 `43 pass, 1 fail`；本次相关的已有-fd flags 场景已经通过，剩余差异是
`write(signalfd)` 在 Linux 返回 `EINVAL`，而现有 Starry/测试预期
`EBADF`。该差异与 journald 的 mask update 触发边界无关，未在本次扩大
kernel 修复范围，留作独立分类。

上一项已提交的 epoll 唤醒回归也保持通过：

```text
=== Results: 10 passed, 0 failed ===
STARRY_SIGNALFD_EPOLL_WAKEUP_PASSED
```

日志为 `.ci-cache/tmp/signalfd-epoll-wakeup-adjacent.log`。`cargo fmt --all
-- --check` 通过，`cargo xtask clippy --package starry-kernel` 的 25 个检查
全部通过，日志为
`.ci-cache/tmp/starry-kernel-signalfd-mask-update-clippy.log`。

撤销临时 journald Debug 环境变量后，宿主使用锁定 flake 重建并发布正式
app-owned rootfs：

```text
system=/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=c791e3cc6c0f4c4b4feaf09e2dd3f9212ff62af50c72d6c5c92a456c9b73c18e
```

构建日志为
`.ci-cache/tmp/starrynixos-host-rootfs-rebuild-signalfd.log`。随后在 Podman
中设置 `STARRY_NIXOS_REUSE_ROOTFS=1` 运行真实 app，出现：

```text
[  OK  ] Started Journal Service.
```

journald 继续处理 journal flush 和错误报告，证明真实 workload 已越过原先
第二次 `signalfd4` 更新失败并退出的触发边界。本次系统终态仍失败，新的直接
runner 终止证据为：

```text
Task(91, "systemd-sysctl") exit with code: 15
[FAILED] Failed to start Apply Kernel Variables.
```

该 unit 在 1 分 30 秒期限后被终止。同期 journald 报告 journal 文件
`ENOENT` 和目录 `fstatvfs` 的 `EISDIR` 差异，但它已成功启动，且当前 runner
首先命中的是 `systemd-sysctl.service` failure；后续必须先取得 sysctl unit
的精确阻塞 syscall/状态边界，不能把 journald 错误直接当作本轮下一根因。
真实 app 日志为
`.ci-cache/tmp/starrynixos-signalfd-mask-update-real-app.log`。

本轮仅直接改变 `signalfd4`：未知 flags 和 `sigsetsize` 校验保持原路径；
新建 fd 仍应用 `SFD_CLOEXEC`/`SFD_NONBLOCK`；已有 fd 仅更新 signal mask，
合法创建 flags 不改变 descriptor/status flags。`STARRY_NIXOS_SYSTEM_PASSED`
仍未出现，因此 T028 保持未完成，但该 signalfd4 修复已满足独立提交门槛。

### 7.22 `systemd-sysctl` 精确失败路径诊断（2026-08-02）

固定 systemd 260.2 源码确认，无显式参数时 `systemd-sysctl` 只枚举
`/etc/sysctl.d`、`/run/sysctl.d`、`/usr/local/lib/sysctl.d`、
`/usr/lib/sysctl.d` 和可选 `sysctl.extra` credential；它不会无条件递归
扫描整个 `/proc/sys`。此前仅限该进程的 syscall entry/return trace 已证明
程序主要执行 O_PATH 路径追踪，并最终主动或被 unit timeout 终止，但由于用户
指针未解码，无法确定配置文件和错误文本。日志为
`.ci-cache/tmp/starrynixos-systemd-sysctl-trace.log`。

本轮在 `sys_openat()` 和 `sys_writev()` 中加入仅用于诊断、限定任务名的临时
路径和 UTF-8 payload 输出。第一次使用完整进程名匹配没有命中，因为 StarryOS
任务名实际截断为 `"(systemd-sysct)"`；该运行在 systemd 90 秒 unit timeout
后收到 SIGTERM，日志为
`.ci-cache/tmp/starrynixos-systemd-sysctl-path-trace.log`。将临时匹配改为
截断名片段后，Podman + `.ci-cache` 真实 app 复跑取得首个精确非忽略错误：

```text
systemd-sysctl openat: ... path="50-coredump.conf", flags=0o12400000
systemd-sysctl writev: fd=2, payload=Ok(
    "Failed to chase '/etc/sysctl.d/50-coredump.conf': Invalid argument\n"
)
```

该配置项在生成的 `/etc` closure 中是绝对 symlink：

```text
/etc/sysctl.d/50-coredump.conf
  -> /nix/store/kj963xfalaj4pcgpza1dy16qpl51j3k7-50-coredump.conf
```

随后读取配置并应用 sysctl 时出现的缺项均由 systemd 明确标记为
`ignoring`，不是 unit 失败根因：

```text
Couldn't write '16' to 'kernel/sysrq', ignoring: No such file or directory
Couldn't write '1' to 'kernel/core_uses_pid', ignoring: No such file or directory
Couldn't write '2' to 'net/ipv4/conf/default/rp_filter', ignoring: No such file or directory
Couldn't write '0' to 'net/ipv4/conf/default/accept_source_route', ignoring: No such file or directory
```

因此当前首个兼容性边界收敛为 systemd path-chase 对绝对 symlink 的
O_PATH/O_NOFOLLOW、`statx(AT_EMPTY_PATH)` 或 `readlinkat` 组合语义，而不是
缺少任意一个 `/proc/sys` 键。完整命中日志为
`.ci-cache/tmp/starrynixos-systemd-sysctl-path-trace-2.log`。下一步必须继续
收敛到产生 `EINVAL` 的具体 syscall，建立同一绝对 symlink 追踪序列的 Linux
oracle 和确定性 Starry 红测后，才能修改 owning subsystem。所有临时路径/
payload 诊断不得进入提交。

进一步加入仅限 `openat/statx/readlinkat/close` 的临时 dispatcher
entry/return trace 后，错误已精确到 `readlinkat`：

```text
openat(..., "50-coredump.conf", O_PATH|O_NOFOLLOW|...) = 7
statx(7, "", AT_EMPTY_PATH, ...) = 0
readlinkat(..., "50-coredump.conf", ..., 4096) = EINVAL
```

日志为 `.ci-cache/tmp/starrynixos-systemd-sysctl-path-trace-3.log`。对应
symlink target 长度恰好为 60 字节：

```text
/nix/store/kj963xfalaj4pcgpza1dy16qpl51j3k7-50-coredump.conf
```

Starry 的 `readlinkat` 通过 axfs-ng `Location::read_link()` 读取 ext4 inode；
其下层 `components/rsext4/src/file/io.rs::read_symlink_target()` 当前仅按
`size <= 60` 把 `i_block[15]` 当作 inline target。对于宿主生成的这个 60
字节 symlink，读取结果不是有效 UTF-8，最终映射为 `EINVAL`。这给出一个可
确定验证的候选边界：ext4 fast-symlink 判定不能只依赖 `size <= 60`，还必须
与 inode 是否实际分配数据块一致。修复前先用 59/60/61 字节 target 建立
Linux oracle 和 Starry 红测；若只有 60 字节边界失败，再修改 rsext4 owning
subsystem。临时 dispatcher/openat/writev trace 随后全部撤销。

### 7.23 ext4 60 字节 symlink 确定性红测（2026-08-02）

新增 grouped QEMU 回归：

```text
test-suit/starryos/qemu/system/bugfix-ext4-symlink-60-byte-target/
```

fixture 不是由 Starry guest 创建，而是在 CMake install 阶段写入宿主 overlay，
随后由 grouped asset pipeline 的 e2fsprogs `debugfs` 1.47.0 注入 ext4 镜像。
因此 59/60/61 字节链接使用宿主 ext4 工具选择的真实磁盘表示，不会被当前
`rsext4::create_symbol_link()` 的边界行为掩盖。

同一 C 二进制先在 Ubuntu 24.04 CI 容器的 Linux 上运行。关闭仅适用于 ext4
镜像的 `st_blocks` 编码断言后，lstat、`openat(O_PATH|O_NOFOLLOW)`、
`statx(AT_EMPTY_PATH)` 和 `readlinkat(dirfd, name)` 对 59/60/61 字节 target
均通过，共 13/13。Linux oracle 目录和输出保留在：

```text
.ci-cache/tmp/starry-symlink-oracle.CyA0wY/
```

Starry 使用 Podman + `.ci-cache` 的实际命令：

```text
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/bugfix-ext4-symlink-60-byte-target
```

首次运行因 CI 容器缺少 `fakeroot`，在 rootfs extraction 阶段停止，未到 guest，
不计作产品红测。临时容器刷新 Ubuntu 索引并安装 `fakeroot` 后，同一命令进入
guest，得到确定性 15/16：

```text
PASS: 59-byte fixture uses the expected ext4 block encoding
PASS: readlinkat returns exact 59-byte target
PASS: 60-byte fixture uses the expected ext4 block encoding
FAIL: readlinkat returns exact 60-byte target: errno=22 (Invalid argument)
PASS: 61-byte fixture uses the expected ext4 block encoding
PASS: readlinkat returns exact 61-byte target
Results: pass=15 fail=1
```

完整红测日志：

```text
.ci-cache/tmp/starry-ext4-symlink-60-red.log
```

Linux ext4 文档把长度小于 60 字节的 target 描述为 `i_block` 内 fast symlink；
Linux v6.18 `ext4_inode_is_fast_symlink()` 则以 inode 实际 block 占用（扣除
EA inode block）和 inline-data flag 判断读取表示，而不是仅按 `i_size`
猜测。当前 rsext4 的 `size <= 60` 读取判据与宿主工具生成的 60 字节
block-backed symlink 冲突。下一步仅修改 `components/rsext4`：读取时结合
`blocks_count() == 0` 判定 fast symlink，并把新建 fast symlink 的边界收敛为
`target_len < 60`；随后用同一红测验证。

修复仅涉及 rsext4 owning subsystem：

```text
components/rsext4/src/file/io.rs
components/rsext4/src/file/create.rs
```

读取 fast symlink 现在同时要求 `size <= 60` 和 `inode.blocks_count() == 0`；
新建 fast symlink 的长度边界改为严格 `< 60`。同一 Podman + `.ci-cache`
Starry 回归随后通过 16/16：

```text
PASS: readlinkat returns exact 59-byte target
PASS: readlinkat returns exact 60-byte target
PASS: readlinkat returns exact 61-byte target
Results: pass=16 fail=0
STARRY_GROUPED_TEST_PASSED: bugfix-ext4-symlink-60-byte-target
STARRY_GROUPED_TESTS_PASSED
```

完整绿测日志：

```text
.ci-cache/tmp/starry-ext4-symlink-60-green.log
```

该结果已完成确定性红→绿，但在 targeted fmt/clippy、相邻回归和真实
StarryNixOS app 跨越 `systemd-sysctl.service` 边界前仍不提交。

rsext4 定向格式化已在 CI 容器执行；`cargo xtask clippy --package rsext4`
的 3 个配置全部通过：

```text
rsext4 (base)
rsext4 (feature: USE_MULTILEVEL_CACHE)
rsext4 (feature: axtest)
```

clippy 日志：

```text
.ci-cache/tmp/rsext4-clippy-ext4-symlink.log
```

随后在同一 Podman + `.ci-cache` 环境串行运行相邻 Starry grouped 回归：

```text
bugfix-bug-readlinkat-zero-size: 12 passed / 0 failed
bugfix-bug-linkat-flags-symlink: 17 passed / 0 failed
bugfix-bug-ext4-dir-ops: 151 passed / 0 failed
```

对应日志：

```text
.ci-cache/tmp/starry-ext4-symlink-adjacent-bugfix-bug-readlinkat-zero-size.log
.ci-cache/tmp/starry-ext4-symlink-adjacent-bugfix-bug-linkat-flags-symlink.log
.ci-cache/tmp/starry-ext4-symlink-adjacent-bugfix-bug-ext4-dir-ops.log
```

focused regression、fmt、owning-crate clippy 和相邻回归均已满足；独立提交前
只剩真实 StarryNixOS workload 跨越原 `systemd-sysctl.service` 触发边界。

使用已有 rootfs manifest 复用同一正式镜像执行真实 app：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 \
cargo xtask starry app qemu -t nixos --arch x86_64
```

完整日志：

```text
.ci-cache/tmp/starrynixos-ext4-symlink-60-real-app.log
```

该运行中 stage 2 activation 和 systemd manager 初始化均已通过；
`systemd-sysctl` 在约 26.1 秒启动，持续执行到约 56.8 秒后才以退出码 1
结束。原先首个 `50-coredump.conf` 绝对 symlink 的 60 字节
`readlinkat(...)=EINVAL` 不再出现，后续 journald、内核模块加载和其他
workload 均继续推进。因此真实 workload 已跨越本修复对应的 ext4 symlink
触发边界，rsext4 修复满足独立提交门槛。

但该 unit 仍最终报告：

```text
Task(91, "systemd-sysctl") exit with code: 256
[FAILED] Failed to start Apply Kernel Variables.
```

当前日志没有保留足够的用户态 stderr/payload，不能把退出码 1 归因于
`keyctl`、缺失 sysctl 节点或其他候选项。下一轮需重新启用仅限
`systemd-sysctl` 的最小路径/payload 诊断，取得新的首个非忽略错误后再建立
确定性红测；不得根据时间相邻日志直接修改 syscall。由于
`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成。

### 7.24 `systemd-sysctl` stderr payload 与退出状态分离（2026-08-02）

为确认 ext4 60 字节 symlink 修复后的真实 `systemd-sysctl` 失败原因，在
`sys_writev()` 已有的单次用户缓冲区复制之后临时记录任务名匹配
`systemd-sysct` 的 stderr payload，并使用 Podman + `.ci-cache` 复用受检
rootfs 运行：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 \
cargo xtask starry app qemu -t nixos --arch x86_64
```

完整日志：

```text
.ci-cache/tmp/starrynixos-systemd-sysctl-writev-after-symlink.log
```

真实运行中 `systemd-sysctl` 共输出 15 条 `writev(fd=2)` 诊断，涉及：

```text
kernel/core_pattern
kernel/core_pipe_limit
fs/suid_dumpable
kernel/sysrq
kernel/core_uses_pid
net/ipv4/conf/default/rp_filter
net/ipv4/conf/default/accept_source_route
net/ipv4/conf/default/promote_secondaries
fs/protected_hardlinks
fs/protected_symlinks
fs/protected_regular
fs/protected_fifos
vm/mmap_rnd_bits
vm/mmap_rnd_compat_bits
fs/inotify/max_user_instances
```

每条消息均为：

```text
Couldn't write ... ignoring: No such file or directory
```

最后一条 payload 是：

```text
Couldn't write '524288' to 'fs/inotify/max_user_instances', ignoring:
No such file or directory
```

随后任务主动以退出码 1 结束：

```text
Task(91, "systemd-sysctl") exit with code: 256
[FAILED] Failed to start Apply Kernel Variables.
```

因此之前“最后 78 字节 stderr 是新的非忽略错误”的假设不成立：用户可见
stderr 全部明确标记为 `ignoring`，但内部返回值聚合仍保留了失败。当前不能
任选一个缺失 sysctl 节点补实现，也不能把相邻 `keyctl` 日志当作根因。下一步
应撤销临时 payload 追踪，核对固定 systemd 260.2 的 sysctl 配置解析和返回值
聚合路径，找出哪类带忽略前缀的写入在 Linux 上仍能导致最终退出 1，并为该
最小语义建立 Linux oracle 与确定性 Starry 红测。`STARRY_NIXOS_SYSTEM_PASSED`
仍未出现，T028 保持未完成。

固定 systemd v260.2 commit
`f1d0952a125b96b7ab2f1ff29a87448ade8ac29b` 的实现确认：

- `sysctl_write_or_warn()` 对非 strict 模式的缺失节点记录上述
  `ignoring: ENOENT` 后返回 0；
- `apply_all()` 只聚合负值；
- `DEFINE_MAIN_FUNCTION(run)` 只把 `run()` 的负返回值映射为进程退出 1。

随后在隔离 Podman 中运行镜像同版本的
`systemd-minimal-260.2/lib/systemd/systemd-sysctl`，显式传入同一
`50-coredump.conf`、`50-default.conf`、`55-nixos-aslr-entropy.conf` 和
`60-nixos.conf`，Linux 结果为：

```text
SYSTEMD_SYSCTL_EXIT=0
```

这证明同一 systemd 版本和同一配置内容本身能够成功结束；Starry 的退出 1
来自配置文件枚举/追踪/读取路径中保留的负结果，不能通过屏蔽 unit 或删减配置
解决。现有 dispatcher trace 中所有配置内容读取的 `read(2)` 均成功并到达
EOF；下一步继续解码该进程唯一一次普通 `write(2)` 及其 fd 目标，并检查配置
枚举阶段是否存在未进入 stderr `writev(2)` 的错误通道。

对普通 `write(2)` 的后续定向追踪确认，`systemd-sysctl` 只成功写入两个
已实现的 procfs sysctl 节点：

```text
fd=3, path=/proc/sys/kernel/pid_max, payload="4194304\n"
fd=3, path=/proc/sys/vm/max_map_count, payload="1048576\n"
```

完整日志：

```text
.ci-cache/tmp/starrynixos-systemd-sysctl-write-fd-trace.log
```

普通 `write(2)` 因而不是未捕获的错误日志通道。`60-nixos.conf` 中其余配置项
既没有进入成功写入，也没有全部形成 `ignoring` 诊断；下一步需要核对实际
rootfs 内四个配置文件的内容与哈希，并对照 systemd 的有序配置表、覆盖和
过滤规则，确定这些键是在正常规则下被消除，还是 Starry 的配置读取/枚举语义
导致了差异。上述追踪代码已经撤销，不进入提交。

### 7.25 rootfs 配置同一性与 debug 扰动边界（2026-08-02）

使用 Podman 中的 `debugfs` 只读导出正式 rootfs
`c791e3cc6c0f4c4b4feaf09e2dd3f9212ff62af50c72d6c5c92a456c9b73c18e`
内四个实际 sysctl 配置文件，并与宿主固定 Nix store 比较 SHA-256：

```text
07a4ab4381de93122ffe76ae5749949515d41b145ab50f169bf987f09a5acc77  50-coredump.conf
5f836b672f2e83426b0fb2379379c454886d7b66865771a9d25707ed96fcc64f  50-default.conf
fb3cadf6a7f2716555ff9e87a39e2df786c6da0494349ddf485d07c5dc720e36  55-nixos-aslr-entropy.conf
e055c7f04e47f5c79cd642f80b3a6b22d87ba3566361a0550db53d3c6a0944ba  60-nixos.conf
```

四项均逐字节一致，排除发布 rootfs 的配置内容漂移。结合固定 systemd
`apply_all()` 的插入顺序，`fs.inotify.max_user_instances` 之后本应继续处理
`60-nixos.conf` 新增的 `max_user_watches`、`kptr_restrict`、`pid_max` 等键；
现有日志没有完整反映这一顺序，因此缺失日志不能解释为配置不存在。

为取得 systemd 内部 debug 信息，曾临时为 `systemd-sysctl.service` 设置
`SYSTEMD_LOG_LEVEL=debug` 和 `SYSTEMD_LOG_TARGET=console`，宿主重建临时
artifact：

```text
system=/nix/store/3q5pyjj1h0f4qq7as09246i370rq7f3m-nixos-system-starrynixos-starry-nixos-stage2
image_sha256=05366cb2267f1ab1de9cabf74be8e5f292743c69756a58b7417f3dba5c582ca6
```

构建和运行日志：

```text
.ci-cache/tmp/starrynixos-systemd-sysctl-debug-rootfs-rebuild.log
.ci-cache/tmp/starrynixos-systemd-sysctl-debug-run.log
```

该运行没有输出预期的 systemd debug 行，反而使 `systemd-sysctl` 持续到
90 秒 unit 超时后被 `SIGTERM` 终止（退出状态 15）。由于临时环境变量和
artifact 布局已经改变真实 workload 的时序/终态，这次运行只能证明该诊断
方式有扰动，不能作为 Linux ABI 修复依据。临时 unit 环境已经撤销；正式
rootfs 必须重新发布后才能继续回归。下一轮定位应使用不会改变 unit 日志目标
的窄化证据，例如确认 `exit_group(1)` 前最后一个由 systemd 聚合的负返回
来源，或者构造直接运行同一二进制和单一配置输入的确定性 Starry 回归。

随后尝试在正式镜像的临时副本中直接替换 `/init`，以绕开 systemd manager
和 credential setup，仅执行同一 `systemd-sysctl` 二进制及四个显式配置
文件。axbuild 对 managed rootfs 的路径约束已按
`rootfs-*.img/rootfs-*.img` 形式满足，但两种 debugfs 注入方式都在目标程序
执行前失败：

1. 直接删除并重建根目录 `/init`；
2. 保留原 `/init` symlink，只删除并重建其 Nix store target。

Starry 均在 `entry.rs` 加载用户入口时报告：

```text
Failed to load user app: Entity not found
```

Linux `debugfs stat` 能看到对应 inode 和 0755 mode，但这类事后新建目录项不能
作为当前 rsext4 上可靠的测试镜像构造方式。因此该实验没有得到
`systemd-sysctl` 退出状态，不构成红测或实现依据。QEMU 配置已恢复到正式
rootfs、正式 success/fail regex 和 600 秒超时；临时镜像不进入提交。若继续
做直接执行回归，应在 Nix 构建阶段生成入口文件，不能再用 debugfs 事后替换。

### 7.26 构建期 direct-rootfs 将失败收敛到 proc sysctl 写入（2026-08-03）

为避免 debugfs 事后新建入口导致的 rsext4 加载扰动，在锁定 flake 的构建阶段
临时增加专用 PID 1 和 ext4 输出。该入口直接运行正式闭包中的 systemd 260.2
`systemd-sysctl`，并显式传入与正式 rootfs 逐字节一致的四个配置文件。重新
实现的 toplevel 仍为：

```text
/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2
```

direct image 通过容器内 `e2fsck -fn`，其 `/init` 在 Nix 构建时创建为：

```text
/nix/store/cr34c4cn314b4idjg1k3ayiic2h4rwg4-starry-nixos-sysctl-direct-init/init
```

使用 Podman + `.ci-cache` 和 managed-rootfs manifest 运行：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 \
cargo xtask starry app qemu -t nixos --arch x86_64
```

完整日志：

```text
.ci-cache/tmp/starrynixos-systemd-sysctl-direct-run.log
```

该 direct workload 不经过 systemd manager、unit credential 或 service 启动
环境，仍稳定得到：

```text
Couldn't write '4194304' to 'kernel/pid_max': Bad file descriptor
Couldn't write '1048576' to 'vm/max_map_count': Bad file descriptor
STARRY_NIXOS_SYSCTL_DIRECT_EXIT=1
STARRY_NIXOS_SYSCTL_DIRECT_FAILED
```

其余缺失 proc sysctl 节点继续被 systemd 明确标记为 `ignoring: ENOENT`。因此
T028 的当前阻塞不在 systemd manager/unit 环境，而在
`/proc/sys/kernel/pid_max` 与 `/proc/sys/vm/max_map_count` 的可写文件描述符
语义：路径打开成功，但后续 `write(2)` 返回 `EBADF`。下一步必须先建立同时
覆盖 `open(O_WRONLY)`、`open(O_RDWR)`、`write(2)`、offset 和回读结果的 Linux
oracle/Starry 确定性回归，确认是哪一层错误地丢失写权限或拒绝已打开 fd，再只
修复对应 procfs/VFS 子系统。临时 flake 输出和 QEMU matcher 已撤销，direct
image 仅保留为 `.gitignore` 覆盖的诊断产物，不进入提交。

`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成。

### 7.27 proc sysctl 可写语义红绿验证与正式 workload 越界（2026-08-03）

围绕 direct-rootfs 已定位的两个节点新增确定性回归：

```text
test-suit/starryos/qemu/system/bugfix-proc-sysctl-writable-limits/
```

测试对 `/proc/sys/kernel/pid_max` 和 `/proc/sys/vm/max_map_count` 分别覆盖：

- `openat(O_WRONLY | O_CLOEXEC)`；
- `openat(O_RDWR | O_CLOEXEC)`；
- `lseek(fd, 0, SEEK_SET)`；
- 通过两种可写 fd 写回原值；
- 重新打开节点并核对写后回读。

Linux oracle 对 `kernel.pid_max` 的 7 项断言全部通过。rootless Podman
缺少修改宿主 `vm.max_map_count` 所需的 capability，该节点在 open 阶段返回
`EACCES`，因此不能把该容器环境当作第二个节点的写入 oracle。测试始终写回
读取到的原值，不主动改变宿主 sysctl 配置。

修复前 Starry 结果为：

```text
=== Results: 10 passed, 4 failed ===
```

四个失败均发生在已成功打开 fd 后的 `write(2)`，errno 为
`EBADF`。完整红测日志：

```text
.ci-cache/tmp/bugfix-proc-sysctl-writable-limits-red.log
```

修复将两个只读 `SimpleFile` 改为可写整数 proc sysctl：

- `pid_max` 接受 `301..=4194304`；
- `max_map_count` 接受 `1..=i32::MAX`；
- 非法 UTF-8、空白以外的非整数和越界值返回 `EINVAL`；
- 合法写入更新节点的回读状态。

当前实现只兑现 systemd 所需的 proc 文件写入与回读语义；PID 分配器和 VMA
分配器尚未消费这两个 atomic，因此不能宣称对应资源限制已经具有完整执行
语义。

修复后同一 Starry 回归结果为：

```text
=== Results: 14 passed, 0 failed ===
STARRY_PROC_SYSCTL_WRITABLE_LIMITS_PASSED
```

完整绿测日志：

```text
.ci-cache/tmp/bugfix-proc-sysctl-writable-limits-green.log
```

Podman + `.ci-cache` 中的 `cargo fmt --all` 通过，
`cargo xtask clippy --package starry-kernel` 的 25 项检查全部通过。clippy
日志：

```text
.ci-cache/tmp/starry-kernel-proc-sysctl-clippy.log
```

随后重新运行正式 rootfs：

```text
system=/nix/store/9qmm1ap5zxbsc3qmkrmphpvlwy9f8a88-nixos-system-starrynixos-starry-nixos-stage2
image_sha256=c791e3cc6c0f4c4b4feaf09e2dd3f9212ff62af50c72d6c5c92a456c9b73c18e
systemd_version=260.2
```

完整日志：

```text
.ci-cache/tmp/starrynixos-proc-sysctl-writable-limits-run.log
```

正式 workload 已越过原 `systemd-sysctl` 阻塞并继续启动后续 units。当前最先
出现的可操作失败变为：

```text
systemd-udevd-kernel.socket:
Failed to create listening socket (kobject-uevent 1): Protocol not available
```

随后另有 `/run/wrappers` mount 失败，但在完成 uevent socket 的 Linux oracle
和确定性 Starry 红测前，不能判断它是否为独立根因。由于
`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，T028 保持未完成。

### 7.28 netlink `SO_REUSEADDR` 红绿验证与 udev socket 越界（2026-08-03）

固定 systemd 260.2 源码确认 `ListenNetlink=kobject-uevent 1` 的实际调用链：

- `socket_address_parse_netlink()` 生成 `SOCK_RAW`、
  `NETLINK_KOBJECT_UEVENT` 和 multicast group 1；
- `socket_address_listen()` 使用
  `SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK` 创建 fd；
- bind 前无条件调用
  `setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, 1)`。

Starry 已支持对应 uevent netlink socket 和 group bind，但其 netlink
`sys_setsockopt()` 未处理 `SO_REUSEADDR`，默认返回 `ENOPROTOOPT`。这与正式
workload 的：

```text
Failed to create listening socket (kobject-uevent 1): Protocol not available
```

逐层一致。

新增确定性回归：

```text
test-suit/starryos/qemu/system/bugfix-netlink-uevent-reuseaddr/
```

同一测试分别覆盖无特权可用的 `NETLINK_ROUTE` 对照和 systemd 实际使用的
`NETLINK_KOBJECT_UEVENT`：

- `SOCK_RAW | SOCK_CLOEXEC | SOCK_NONBLOCK`；
- `setsockopt(SOL_SOCKET, SO_REUSEADDR, 1)`；
- route group 0 / uevent group 1 的 `bind(2)`。

Podman Linux oracle 结果为：

```text
=== Results: 6 passed, 0 failed ===
STARRY_NETLINK_UEVENT_REUSEADDR_PASSED
```

直接宿主进程受当前执行沙箱限制，所有 `AF_NETLINK` socket 都先返回
`EPERM`，因此不作为 Linux 语义证据。

修复前 Starry 结果为：

```text
PASS: create route netlink control socket
FAIL: set SO_REUSEADDR on route netlink socket: errno=92
PASS: bind route netlink socket
PASS: create systemd-style uevent socket
FAIL: set SO_REUSEADDR on uevent socket: errno=92
PASS: bind uevent multicast group 1
=== Results: 4 passed, 2 failed ===
```

完整红测日志：

```text
.ci-cache/tmp/bugfix-netlink-uevent-reuseaddr-red.log
```

修复只在 netlink `setsockopt` 边界读取并接受 `SO_REUSEADDR` 的整数参数。
Starry 当前 netlink bind 模型不通过本地地址复用解决端口或 group 冲突，因此
没有加入未被消费的复用状态，也没有扩大 uevent broadcast 能力声明。

修复后同一 Starry 回归 6/6 通过：

```text
STARRY_NETLINK_UEVENT_REUSEADDR_PASSED
STARRY_GROUPED_TESTS_PASSED
```

完整绿测日志：

```text
.ci-cache/tmp/bugfix-netlink-uevent-reuseaddr-green.log
```

Podman 中 `cargo fmt --all` 通过，
`cargo xtask clippy --package starry-kernel` 的 25 项检查全部通过。clippy
日志：

```text
.ci-cache/tmp/starry-kernel-netlink-reuseaddr-clippy.log
```

正式固定 rootfs 复测日志：

```text
.ci-cache/tmp/starrynixos-netlink-reuseaddr-run.log
```

该运行明确出现：

```text
[  OK  ] Listening on udev Kernel Socket.
Task(91, "systemd-sysctl") exit with code: 0
[  OK  ] Finished Apply Kernel Variables.
```

说明 uevent socket 和前一轮 proc sysctl 两个触发边界均已越过。当前新的首个
terminal failure 是：

```text
systemd-journald:
Failed to fstatvfs(.../journal/<machine-id>): Is a directory
.../system.journal: Unexpected error while writing to journal file:
No such file or directory

[FAILED] Failed to start Flush Journal to Persistent Storage.
```

下一步应先直接比较 Linux/Starry 对目录 fd 和普通文件 fd 的
`fstatvfs(2)`，确认 `EISDIR` 是否来自 Starry VFS fd 类型分派，再建立确定性
红测。不能直接屏蔽 `systemd-journal-flush.service`。由于
`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成。

### 7.29 目录 fd `fstatvfs` 红绿验证与 journal flush 越界（2026-08-03）

新增确定性回归：

```text
test-suit/starryos/qemu/system/bugfix-fstatvfs-directory/
```

同一测试覆盖：

- 目录 fd 的 `fstatvfs(3)` 成功路径；
- 目录结果包含非零 `f_bsize`；
- 普通文件 fd 的对照成功路径；
- 无效 fd 返回 `EBADF`。

Podman Linux oracle 结果为：

```text
=== Results: 6 passed, 0 failed ===
STARRY_FSTATVFS_DIRECTORY_PASSED
```

修复前 Starry 结果为：

```text
PASS: open directory fd
FAIL: fstatvfs accepts directory fd: errno=21 (Is a directory)
PASS: open regular file fd
PASS: fstatvfs accepts regular file fd
PASS: fstatvfs rejects invalid fd with EBADF
=== Results: 4 passed, 1 failed ===
```

完整红测日志：

```text
.ci-cache/tmp/bugfix-fstatvfs-directory-red.log
```

根因位于 `os/StarryOS/kernel/src/syscall/fs/stat.rs`：
`sys_fstatfs()` 无条件通过 `File::from_fd()` 取得位置，而该转换为常规文件
I/O 明确把 `Directory` 映射为 `EISDIR`。Linux 的 `fstatfs(2)` 接受指向目录
的合法打开 fd，因此修复仅在 `sys_fstatfs()` 中为 `Directory` 选择其
`Location`，其他 fd 类型继续走既有 regular file/memfd 分派；未扩大到
socket、pipe 或匿名 fd 的未验证语义。

修复后同一 Starry 回归结果为：

```text
=== Results: 6 passed, 0 failed ===
STARRY_FSTATVFS_DIRECTORY_PASSED
STARRY_GROUPED_TESTS_PASSED
```

完整绿测日志：

```text
.ci-cache/tmp/bugfix-fstatvfs-directory-green.log
```

Podman + `.ci-cache` 中 `cargo fmt --all -- --check` 通过，
`cargo xtask clippy --package starry-kernel` 的 25 项检查全部通过。clippy
日志：

```text
.ci-cache/tmp/starry-kernel-fstatvfs-directory-clippy.log
```

正式固定 rootfs 复测日志：

```text
.ci-cache/tmp/starrynixos-fstatvfs-directory-run.log
```

该运行不再出现：

```text
Failed to fstatvfs(.../journal/<machine-id>): Is a directory
```

并明确越过原触发边界：

```text
[  OK  ] Started Journal Service.
[  OK  ] Finished Flush Journal to Persistent Storage.
```

journald 后续仍报告创建或写入 `system.journal` 时的 `ENOENT`，但该消息未再
使 journal flush unit 失败，不能与已修复的目录 fd `EISDIR` 合并处理。当前
新的首个 terminal failure 是：

```text
[FAILED] Failed to start Rule-based Manager for Device Events and Files.
See 'systemctl status systemd-udevd.service' for details.
```

对应 `(systemd-udevd)` 任务退出码记录为 `57856`，acceptance runner 随后由
`Failed to start` fail pattern 正确终止。下一步需要为该 udevd 退出建立独立
诊断和 Linux oracle；不能从当前日志推测为 netlink、keyctl 或文件系统问题。
由于 `STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成。

### 7.30 systemd-udevd `EXIT_NAMESPACE` 源码收敛（2026-08-03）

固定 rootfs 中 systemd derivation：

```text
/nix/store/cmkrg47n2jb3hn1n1p4jc5yzfgg6icbp-systemd-minimal-260.2.drv
```

声明的精确源码输入为：

```text
/nix/store/kban61mm86a1nhq05rzg771n4l7qfjgw-source
```

systemd 260.2 的 `systemd.exec(5)` 和 `src/shared/exit-status.h` 确认退出码
226 是 `EXIT_NAMESPACE`，含义限定为 mount、UTS 或 IPC namespace 设置失败。
正式日志中的 Starry wait status `57856` 等于 `226 << 8`，因此当前失败已从
泛化的 udevd 启动失败收敛到 systemd exec sandbox 的 namespace 阶段。

固定 unit 的相关项是：

```ini
PrivateMounts=yes
ProtectHostname=yes
```

systemd 260.2 的实际调用顺序是：

1. `PrivateMounts=yes` 使 `exec_needs_mount_namespace()` 为真；
2. `setup_namespace()` 先调用 `unshare(CLONE_NEWNS)`；
3. 随后执行 `mount(NULL, "/", NULL, MS_SLAVE | MS_REC, NULL)` 并应用
   unit 所需的 mount 列表；
4. mount namespace 完成后，`ProtectHostname=yes` 再通过
   `unshare(CLONE_NEWUTS)` 建立 UTS namespace；
5. 上述 mount/UTS 路径中的非忽略失败均映射为 `EXIT_NAMESPACE`。

Starry 当前已有测试覆盖基础 `unshare(CLONE_NEWNS)`、
`unshare(CLONE_NEWUTS)`、`MS_SLAVE`/`MS_PRIVATE` propagation 和 mount
namespace 隔离，因此现有证据不足以把失败归因到 namespace 创建本身。日志中
udevd 启动期间出现的 `keyctl` ENOSYS 属于并行 `/run/wrappers` mount 任务，
不能作为 udevd 226 的根因。

下一步先为 `systemd-udevd.service` 临时增加仅用于诊断的 console/debug
输出，取得 systemd exec 子进程报告的具体 namespace 步骤和 errno。该诊断不得
关闭 `PrivateMounts`、`ProtectHostname` 或其他 sandbox 项，也不作为提交内容。
得到精确失败调用后，再用相同输入建立 Podman Linux oracle 和确定性 Starry
红测；红测前不修改内核。`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持
未完成。

### 7.31 初始 no-sandbox 支持边界调整（2026-08-03）

用户确认 StarryOS 的既有 Nix 使用方式允许不启用 sandbox，并批准 Stage 2
先验证 no-sandbox NixOS 基线，后续再恢复 sandbox 支持。这里需要区分：

- guest 内 `nix.enable = false`，本轮不在 StarryOS 内执行 Nix build，因此
  `nix.settings.sandbox = false` 不是当前启动阻塞；
- 当前 226 来自 systemd service 的 namespace sandbox，因此本轮先对实际
  阻塞 unit 的精确 sandbox 指令应用局部 override。

SpecKit 已同步允许以下策略：

1. 只对实际阻塞 unit 关闭已测得失败的 systemd sandbox 指令；
2. 记录 unit、上游值、override、触发 exit status 和恢复条件；
3. 禁止全局关闭所有 systemd sandbox；
4. no-sandbox baseline 可以用于证明 Stage 2 启动，但不得声称对应隔离语义已
   支持；
5. acceptance 的 PID 1、activation、multi-user、marker 和唯一 pass marker
   要求不变。

当前实验对象是 `systemd-udevd.service`：

```text
upstream: PrivateMounts=yes, ProtectHostname=yes
override: PrivateMounts=no,  ProtectHostname=no
trigger:  EXIT_NAMESPACE (226)
restore:  Starry mount/UTS sandbox 调用链具备确定性 Linux 对照并通过后
```

下一步撤销临时全局 `LogLevel=debug` / `LogTarget=console` 配置，改用上述
unit-local no-sandbox override，重建固定 artifact 并运行真实 acceptance。
T028 在 `STARRY_NIXOS_SYSTEM_PASSED` 出现前保持未完成。

### 7.32 udevd no-sandbox 越界与 firewall 新边界（2026-08-03）

NixOS 生成结果保留 systemd 260.2 上游 unit：

```ini
PrivateMounts=yes
ProtectHostname=yes
```

并只生成：

```text
systemd-udevd.service.d/overrides.conf
```

其中包含：

```ini
PrivateMounts=false
ProtectHostname=false
```

未修改其他 unit，也未全局关闭 systemd sandbox。重建后的 managed artifact：

```text
system=/nix/store/ikcjg9nprb98ijxdxigmlsgdz9qnlxg8-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=c4a9c49112cb426da5be56c3e32fdf006208e5c2a7812af12edf5778e0925248
```

构建日志：

```text
.ci-cache/tmp/starrynixos-udevd-no-sandbox-rootfs-rebuild.log
```

Podman + `.ci-cache` 正式运行日志：

```text
.ci-cache/tmp/starrynixos-udevd-no-sandbox-run.log
```

该运行不再出现 `(systemd-udevd)` 的 `EXIT_NAMESPACE`，并明确出现：

```text
[  OK  ] Started Rule-based Manager for Device Events and Files.
```

因此 unit-local no-sandbox override 已越过原触发边界。新的首个 terminal
failure 是：

```text
[FAILED] Failed to start Firewall.
See 'systemctl status firewall.service' for details.
```

firewall 不是 systemd sandbox，而是 NixOS 默认启用的网络过滤工作负载。Stage 2
spec 的 FR-013 已把 networking stack 列为非目标，因此下一步允许声明
`networking.firewall.enable = false`，并把它记录为 profile 级非目标例外，而
不是内核兼容修复。acceptance 的五项证据仍保持不变。
`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成；当前配置和文档不满足
独立提交条件。

### 7.33 关闭 firewall 后的 no-sandbox 验收回归（2026-08-03）

按 FR-013 将初始 profile 的非目标网络过滤服务关闭后，重新生成的 managed
artifact 为：

```text
system=/nix/store/63yrvxyr7x61j84fvdx2i732296j1bd3-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=5ee2b461975021b84bb34bd435a0c0a7964667f581c0c6aefc0d8f3a8ac2346c
```

构建和 Podman + `.ci-cache` 正式运行日志分别为：

```text
.ci-cache/tmp/starrynixos-no-sandbox-firewall-disabled-rootfs-rebuild.log
.ci-cache/tmp/starrynixos-no-sandbox-firewall-disabled-run.log
```

该运行继续证明 `systemd-udevd.service` 的 unit-local override 已越过原
`EXIT_NAMESPACE` 边界：

```text
[  OK  ] Started Rule-based Manager for Device Events and Files.
```

但关闭 firewall 后未取得 Stage-2 pass。当前运行中
`systemd-sysctl.service` 持续 90 秒后被 systemd 发送 `SIGTERM`，最终出现：

```text
Task(90, "systemd-sysctl") exit with code: 15
[FAILED] Failed to start Apply Kernel Variables.
```

并同时出现：

```text
[FAILED] Failed to start Flush Journal to Persistent Storage.
```

这与此前同一内核兼容修复后的正式运行
`.ci-cache/tmp/starrynixos-netlink-reuseaddr-run.log` 中：

```text
Task(91, "systemd-sysctl") exit with code: 0
[  OK  ] Finished Apply Kernel Variables.
```

不一致，因此现阶段不能把该超时直接归类为新的稳定内核语义缺口，也不能通过
屏蔽 `systemd-sysctl` 或 `systemd-journal-flush` 宣称 no-sandbox baseline
通过。下一步必须比较两个 artifact 的实际 unit、drop-in、rootfs 内容和运行
时序，确认这是配置差异、sandbox 差异还是可稳定复现的内核行为。由于
`STARRY_NIXOS_SYSTEM_PASSED` 未出现，本轮改动不提交，T028 保持未完成。

对同一 artifact 的复跑日志为：

```text
.ci-cache/tmp/starrynixos-no-sandbox-firewall-disabled-rerun.log
```

复跑证明 `systemd-sysctl.service` 的 90 秒超时不是稳定的配置或 sandbox
差异；相同 unit 和 rootfs 在本轮约 9 秒内完成：

```text
Task(90, "systemd-sysctl") exit with code: 0
[  OK  ] Finished Apply Kernel Variables.
```

同一运行还成功越过：

```text
[  OK  ] Finished Flush Journal to Persistent Storage.
[  OK  ] Finished Create Static Device Nodes in /dev.
```

其中第二项耗时约 250 秒，属于当前 Starry 文件系统路径的显著性能问题，但
最终退出 0，不能记录为功能失败。随后 `/run/wrappers` 的 mount 进程退出 0，
systemd 仍报告 mount unit 失败并使 `nix-suid-wrappers.service` 依赖失败；该
失败未阻止继续到 `local-fs.target`。

本轮第一个被 acceptance fail pattern 捕获的 terminal failure 是：

```text
Task(99, "systemd-machine-id-setup") exit with code: 256
[FAILED] Failed to start Save Transient machine-id to Disk.
```

对应 unit 为 `systemd-machine-id-commit.service`。它尝试把本次启动生成的
transient machine-id 持久化到只读 system artifact；这不是 namespace
sandbox，不能通过追加 sandbox override 处理。下一步应比较两个更小且可审查
的 profile 方案：在 artifact 中预置合法 machine-id，或在明确不要求
machine-id 持久化的 Stage-2 profile 中仅禁用该 unit。选择前必须核对 NixOS
container profile 和 systemd 对只读 `/etc/machine-id` 的既有约定。
`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现，本轮改动不提交，T028 保持未完成。

固定 systemd 260.2 文档和源码进一步确认：

- 该 unit 只负责把已可用的 transient machine-id 转为持久 machine-id；
- `--commit` 路径会调用 `detach_mount_namespace()`，再卸载临时
  `/etc/machine-id` mount、写入持久文件并切回原 mount namespace；
- transient machine-id 本身在 unit 启动前已经可供 PID 1、journald 和其他
  services 使用。

因此 Stage-2 profile 选择禁用
`systemd-machine-id-commit.service`，不在可复制 artifact 中预置会被所有实例
共享的固定 machine-id。该例外只排除跨启动持久化；不把 mount namespace
替换语义描述为已支持。Spec 的 FR-013 同步把 persistent machine identity
列为初始非目标。下一次重建必须验证生成 unit 被精确禁用，并重新运行完整
acceptance。

### 7.34 machine-id commit 精确禁用的 artifact（2026-08-03）

使用宿主 Nix dev shell 在执行沙箱外完成重建；Podman CI 镜像本身不包含
`nix`，因此只继续用于后续 QEMU 验收。构建日志为：

```text
.ci-cache/tmp/starrynixos-machine-id-commit-disabled-rootfs-rebuild.log
```

新的 managed artifact 为：

```text
system=/nix/store/211p2xi0fbxi7fq6dq3zjryl46wk3dmz-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=0afa089a2950c990842cb5e8a2d66eae4086a8ef519cc19458b870d92ec2e641
```

生成的 system unit 验证为精确 mask：

```text
systemd-machine-id-commit.service -> /dev/null
```

同时 `systemd-udevd.service.d/overrides.conf` 仍只包含已记录的
`PrivateMounts=false` 和 `ProtectHostname=false`，未引入全局 systemd sandbox
关闭。该 artifact 尚未完成 QEMU acceptance，因此本分类仍不提交，T028 保持
未完成。

### 7.35 machine-id 例外后的 600 秒真实验收（2026-08-03）

首先使用宿主绝对路径挂载仓库的尝试在 guest 启动前被 artifact 校验拒绝：

```text
StarryNixOS artifact error: published manifest '/workspace/...manifest' is missing
```

这是已发布 manifest 记录 `/workspace/...` 构建路径与容器挂载路径不一致，
不是 StarryOS guest 回归。随后按 artifact 发布时的 `/workspace` 路径重跑
Podman + `.ci-cache` 验收：

```bash
env -u LD_PRELOAD podman run --rm --userns=keep-id:uid=0,gid=0 \
  -v "$PWD:/workspace" \
  -v "$PWD/.ci-cache/cargo:/tmp/cargo" \
  -v "$PWD/.ci-cache/rustup:/tmp/rustup" \
  -v "$PWD/.ci-cache/tmp:/tmp/ci-tmp" \
  -w /workspace \
  -e CARGO_HOME=/tmp/cargo \
  -e RUSTUP_HOME=/tmp/rustup \
  -e TMPDIR=/tmp/ci-tmp \
  ghcr.io/rcore-os/tgoskits-container:latest \
  bash -lc 'set -o pipefail; STARRY_NIXOS_REUSE_ROOTFS=1 cargo xtask starry app qemu -t nixos --arch x86_64 2>&1 | tee /tmp/ci-tmp/starrynixos-machine-id-commit-disabled-rerun.log'
```

该 run 的 artifact identity 为：

```text
system=/nix/store/211p2xi0fbxi7fq6dq3zjryl46wk3dmz-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=0afa089a2950c990842cb5e8a2d66eae4086a8ef519cc19458b870d92ec2e641
```

它明确越过两个已记录边界：

```text
Task(90, "systemd-sysctl") exit with code: 0
Finished Apply Kernel Variables.
Finished Flush Journal to Persistent Storage.
Finished Create Static Device Nodes in /dev gracefully.
Finished Create Static Device Nodes in /dev.
Started Rule-based Manager for Device Events and Files.
```

因此：

- `systemd-machine-id-commit.service` 的精确 `/dev/null` mask 生效，未再出现
  transient machine-id 持久化失败；
- `systemd-udevd.service` 的局部
  `PrivateMounts=false`、`ProtectHostname=false` override 生效，未再出现
  `EXIT_NAMESPACE (226)`；
- 没有全局关闭 systemd sandbox。

`/run/wrappers` mount 仍报告失败，继而使 `nix-suid-wrappers.service` 依赖失败，
但不是该 run 的 terminal failure，systemd 继续执行后续事务。验收在 600 秒时
由 outer runner 终止：

```text
Error: QEMU timed out after 600s
```

超时前 `Register Nix Store Paths` 与 `Create System Files and Directories`
两个无时限 job 从约 guest 337 秒开始持续运行，marker 未执行。日志反复出现：

```text
Failed to update file times on drop: AxErrorKind::ReadOnlyFilesystem
```

该信息尚未给出触发 syscall、Linux oracle 或可确定复现的 Starry 红测，不能据此
猜测性修改内核或放宽 matcher。下一步先解出 `nixos-register` 和实际
system-files unit 的 `ExecStart`，在 Linux 上建立同一最小操作的行为/时延基线，
再新增红测并只修复其 owning subsystem。`STARRY_NIXOS_SYSTEM_PASSED` 未出现，
T028 仍未完成；本次没有可独立提交的改动。

### 7.36 600 秒停滞 unit 的实际 workload（2026-08-03）

对 7.35 artifact 的 toplevel closure 只读检查表明，job 标题并非抽象
activation 阶段，而是以下两个精确操作：

```text
register-nix-paths.service
  ExecStart=.../register-nix-paths-start
  nix-store --load-db < /nix-path-registration
  rm /nix-path-registration
  nix-env -p /nix/var/nix/profiles/system --set /run/current-system

systemd-tmpfiles-setup.service
  ExecStart=systemd-tmpfiles --create --remove --boot --exclude-prefix=/dev
```

前者的 `ConditionPathExists=/nix-path-registration`，`Type=oneshot` 且
`RemainAfterExit=true`；后者加载生成的 `tmpfiles.d`，其中含有 `/run/keys`、
`/var`、journal 目录、ACL、属性和临时目录规则。因此这两个 job 的共同启动
时间不能单独证明某一个具体文件系统 syscall 错误。

同一 guest 日志在 job 停滞前还记录了 PID 1 上反复出现的：

```text
Unimplemented syscall: kcmp (tid=1)
unsupported fcntl parameters: cmd: 1027
```

以及后续 task 99/100 上的只读 file-time diagnostic 和一次 `keyctl` ENOSYS。
这些记录尚未和上述两个 unit 的具体进程建立因果对应；尤其 `kcmp` 的 `tid=1`
来自 systemd manager，不能在没有 Linux 对照和直接回归的情况下当作
`nix-store` 或 tmpfiles 的根因。

下一步按两个候选 workload 分别建立 Linux 可观察基线：先确认
`nix-store --load-db` 的最小输入、数据库/锁/链接副作用，再将 tmpfiles
输入缩减到实际触发的 rule。只有某一最小程序在当前 Starry 上确定失败，才新增
对应 system QEMU 红测并进入内核修复。T028 保持未完成。

### 7.37 Nix path registration 的隔离 Linux 时延基线（2026-08-03）

从正式 artifact 使用 `debugfs` 只读导出的 `/nix-path-registration` 为
`144084` 字节，SHA-256 为：

```text
349eb69c55fda456ace8b215a2e6eecbe46edeec484bf577a16f7d05ad90af60
```

使用 artifact 中同一 Nix 2.34.8 binary，在 `/tmp` 内独立的
`NIX_STATE_DIR`、`NIX_LOG_DIR` 和 profile 路径运行 7.36 的两步操作。该
实验不写入宿主 `/nix/var/nix` 数据库，也不修改 artifact 或项目文件：

```text
NIX_STORE_LOAD_DB_ELAPSED=0.30
NIX_ENV_SET_ELAPSED=0.10
DB_BYTES=8646658
```

因此完整的真实 registration 输入在 Linux 上约 0.40 秒完成，而 Starry
acceptance 在该 unit 与 tmpfiles unit 并行运行超过数分钟仍无完成记录。这建立了
可量化的 Linux 时延差异，但不能把二者共同的 timeout 直接归为
`nix-store --load-db`：

- `register-nix-paths` 还包含 profile 更新；
- `systemd-tmpfiles-setup` 在同一事务中并行执行；
- 当前日志的 `kcmp`、`fcntl(1027)`、`keyctl` 和只读 file-time 信息尚未绑定到
  一项具体 workload。

下一步对 Linux registration 运行只跟踪 `kcmp`、`fcntl`、`keyctl` 的 syscall
轨迹，并为 tmpfiles 建立相同 root/rule 的隔离基线；只有证据将某一 syscall 或
VFS 操作与 Starry 的停滞关联起来，才添加 T027 所需的确定性红测。T028 保持
未完成。

### 7.38 Nix syscall 轨迹排除与 tmpfiles Linux 基线限制（2026-08-03）

对 7.37 的同一 Nix 2.34.8 path-registration workload 做 Linux `strace`
后，`nix-store --load-db` 的锁行为仅包含 SQLite 使用的 POSIX record lock：

```text
fcntl(..., F_SETLK, ...)
fcntl(..., F_GETLK, ...)
```

该最小 workload 未调用 `kcmp`、`keyctl` 或 `fcntl(1027)`。因此 guest 日志中
PID 1 的 `kcmp` 与 `fcntl(1027)`，以及另一个 task 的 `keyctl` ENOSYS，均不能
再被推测为 Nix path registration 的直接调用；它们仍需先与实际执行 task 建立
身份对应。

此前首次在临时 Linux root 上运行 `systemd-tmpfiles` 的尝试也不能作为 oracle：
该执行环境没有 root `chown` 权限，且未提供 artifact 的完整 passwd/group 数据，
按 Linux 规则以状态 65 退出。这个结果不说明 StarryOS 与 Linux 在 tmpfiles
语义上存在差异。

下一步必须在具有 root 能力且隔离状态的 Podman/namespaced Linux 环境中，使用
artifact 的 passwd/group 与完整 tmpfiles rules 重跑相同 systemd 260.2
workload；同时定位 guest task 99/100 的实际 exec 身份。只有缩减后的一个操作
在 Linux/Starry 对照下给出明确差异后，才编写 T027 要求的确定性红测。T028
保持未完成，`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现。

### 7.39 完整 tmpfiles 规则的 root-capable Linux 基线（2026-08-03）

guest 7.35 日志的 task identity 已由启动顺序与 unit 标题收敛为：

```text
task 99  -> register-nix-paths.service
task 100 -> systemd-tmpfiles-setup.service
```

两者各自都出现过一次 `keyctl` ENOSYS；PID 1 的 `kcmp`/`fcntl(1027)` 日志则
仍来自 systemd manager，不能转嫁为 task 99 或 100 的 workload 调用。

为建立有效的 tmpfiles Linux oracle，使用映射当前用户的 root Podman user
namespace（`--userns=keep-id:uid=0,gid=0`）从只读正式 ext4 image 提取临时根
目录。该根目录保留 artifact 的 `/etc/passwd`、`/etc/group`、完整
`tmpfiles.d` 与 Nix store；容器退出即丢弃，不修改 image、artifact 或项目文件。

systemd 260.2 在无 `/proc` 的 `chroot` 中明确拒绝运行并建议使用 `--root`，
因此最终使用 image 内同一 toplevel 的 `systemd-tmpfiles` 和官方离线模式：

```text
systemd-tmpfiles --root=<temporary-root> \
  --create --remove --boot --exclude-prefix=/dev
```

该操作以状态 0 完成，`TMPFILES_ELAPSED=0.00`（按 0.01 秒精度格式化）。它表明
完整生成规则、账号/组解析、文件创建、权限与属性处理在 root-capable Linux
对照中并不固有停滞。

`--root` 基线不复现 guest 已经建立的 live `/run`、`/proc` 或 systemd manager
状态，所以仍不能单独指定 Starry owning subsystem；但它排除了此前非特权状态
65 退出造成的无效比较。下一步应为 task 99/100 中可观测的一项精确 VFS 操作
建立 Linux/Starry 红测，而不是以 timeout、mask 或全局 sandbox 设置替代诊断。
T028 保持未完成，`STARRY_NIXOS_SYSTEM_PASSED` 仍未出现。

### 7.40 只读挂载读访问的 atime 候选（2026-08-03）

600 秒运行中反复出现的：

```text
Failed to update file times on drop: AxErrorKind::ReadOnlyFilesystem
```

已定位到 `ax-fs-ng` 的 `File::drop`：只要文件句柄曾读取过数据，它就无条件
调用 `Location::update_metadata()` 写入 atime。若该 `Location` 位于只读
挂载，VFS 正确返回 `ReadOnlyFilesystem`，但该调用本身仍是不必要的 VFS 写尝试。

Linux 上游固定提交
`075b74841bd0065a3bda3440873c747938e69b68` 的 `fs/inode.c` 给出本次
语义依据：

1. `atime_needs_update()` 明确说明会处理只读文件系统和介质；
2. `touch_atime()` 在调用文件系统的 `update_time` 前，先取得 superblock 写锁
   与 mount write access；
3. 任一写访问取得失败时，它跳过 atime 更新，且该路径不把错误暴露给读者。

因此 Starry 的最低层回归应验证：从只读挂载读取后释放 `File`，不得调用
`update_metadata()`。这是一个潜在的 Linux VFS 语义修复，而不是当前
`register-nix-paths`/`systemd-tmpfiles` 600 秒停滞的已证实 owning cause。
已加入的最小单元测试
`file::handle::tests::readonly_mount_skips_read_access_metadata_update` 在修复前
以确定性方式失败：

```text
cargo test -p ax-fs-ng readonly_mount_skips_read_access_metadata_update
assertion failed: !needs_metadata_update_on_drop(&location, 1)
```

失败原因是当前 drop-time 判定仅检查“是否发生过 I/O”，没有排除只读
`Location`。下一步仅将该判定限制为可写挂载，然后执行格式化、`ax-fs-ng`
clippy、聚焦回归和真实 NixOS acceptance 复验。

该最小修复现已完成：drop-time 判定要求同时满足“句柄发生过 I/O”与“所在
mount 可写”。同一单元测试已转绿：

```text
cargo test -p ax-fs-ng readonly_mount_skips_read_access_metadata_update
1 passed; 0 failed
```

并已执行：

```text
cargo fmt -p ax-fs-ng
cargo xtask clippy --package ax-fs-ng
```

后者覆盖 base、`axtest`、`ext4`、`fat`、`lockdep`、`vfs` 六个配置，全部以
`-D warnings` 通过。该结果只证明 ax-fs-ng 的最低层 Linux VFS 对齐；尚未证明
它解除 Stage-2 600 秒停滞，也尚未满足真实 QEMU 越过日志边界的独立提交条件。

随后使用同一正式 artifact 与 Podman + `.ci-cache` 重跑：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 \
  cargo xtask starry app qemu -t nixos --arch x86_64
```

该运行重新构建了包含修复的 StarryOS kernel，并进入 NixOS stage 2、activation
与 systemd 事务。`systemd-tmpfiles` 的 `Create Static Device Nodes in /dev
gracefully` 在 guest 约 139 秒时以状态 0 结束；但 `systemd-sysctl.service`
与 `systemd-journal-flush.service` 在更早阶段超时，acceptance 的既有失败
matcher 正确终止：

```text
Failed to start Apply Kernel Variables.
Failed to start Flush Journal to Persistent Storage.
```

这个 terminal failure 早于此前 `register-nix-paths.service` /
`systemd-tmpfiles-setup.service` 在数百秒后停滞的 task 99/100 边界。因此本次
运行没有验证该精确 workload 中的只读 file-time diagnostic 是否已消失，也不能
将局部 VFS 红绿修复标记为完整 Stage-2 进展或提交。后续需先对
`systemd-sysctl`/journal flush 的可重复性和其与任务调度的关系做更小的对照，
不得因本次较早失败放宽 matcher、mask 这些 unit 或声称 pass。
`STARRY_NIXOS_SYSTEM_PASSED` 尚未出现，T028 保持未完成。

### 7.41 readonly-atime 修复后的第二次真实 QEMU 运行检查点（2026-08-03）

使用与 7.40 相同的正式 rootfs、包含 `ax-fs-ng` 修复的 kernel，以及 Podman
+ `.ci-cache` 的 CI-like 命令启动第二次 acceptance 后，先前在约 139 秒前失败
的早期事务这次均已完成：

```text
systemd-sysctl.service                 exit 0
systemd-journal-flush.service          [  OK  ]
systemd-tmpfiles --prefix=/dev         exit 0（guest 132.96 秒）
systemd-udevd.service                  [  OK  ]
```

`run-wrappers.mount` 仍报告 failed，因而其依赖的 SUID/SGID wrapper 创建没有
完成；这属于 Stage-2 明确排除的 wrapper 能力，且当前并未使 systemd 事务终止。
重要的是，本轮已经启动此前数百秒停滞的两个 unit：

```text
task 98  -> register-nix-paths.service
task 100 -> systemd-tmpfiles-setup.service
```

该运行最终由既有严格 runner 在 600 秒终止：

```text
Error: QEMU timed out after 600s
```

完整日志（SHA-256
`d2024ac53078cb2752418d09ca85e090684811efaf7cebd78febfadc4c31ce84`）
确认：task 98 和 100 各启动一次，`systemd-sysctl`/journal flush 没有再次失败，
`STARRY_NIXOS_SYSTEM_PASSED` 为零次；两个 task 一直运行到外层超时。它重现了
7.37–7.39 的精确停滞边界，不能完成 T028。

同一完整日志中 `Failed to update file times on drop` 为零次。这次实际越过了原先
出现该只读 atime 写尝试的 task 98/100 workload，结合已完成的红测、绿测、格式化
和 `ax-fs-ng` targeted clippy，满足了该**独立** Linux VFS 修复的验证门槛；
它已作为不签名的独立提交
`41e8e5dd9 fix(ax-fs-ng): skip metadata updates on read-only mounts`
落地，与仍未解决的 Stage-2 timeout 分开管理。此结论不把 600 秒 timeout、
`run-wrappers.mount` 失败或任何 unit mask 解释为通过，也不改变 T028 未完成状态。

下一步仍是将 task 98 或 100 的一个实际操作缩减为 Linux/Starry 的确定性红测。
在能够证明 owning syscall 或 VFS 操作前，不对 `keyctl`、PID 1 的
`kcmp`/`fcntl(1027)`，或两项 workload 的共同超时做推测性内核修改。

对本次正式 ext4 artifact 的只读 unit 解析进一步确认 task 98 的精确脚本为：

```sh
nix-store --load-db < /nix-path-registration
rm /nix-path-registration
nix-env -p /nix/var/nix/profiles/system --set /run/current-system
```

task 100 保持 systemd 260.2 的
`systemd-tmpfiles --create --remove --boot --exclude-prefix=/dev`。因此后续 task 98
诊断应先区分 `nix-store --load-db`、删除 registration 文件和 profile 更新；不能把
完整脚本或 tmpfiles 的并行等待直接命名为某一个 syscall 缺陷。

### 7.42 临时进程快照单元的依赖与输出修正（2026-08-03）

为直接确认 task 101 的 exec 身份，上一轮在
`apps/starry/nixos/configuration.nix` 增加了仅用于 T028 诊断的
`starry-nixos-process-snapshot.service`。首次 artifact 的 systemd 260.2
日志表明该诊断本身无效：

```text
sysinit.target: Found ordering cycle ...
Job starry-nixos-process-snapshot.service/start deleted
Failed to parse output specifier, ignoring: console
```

原因是普通 service 的默认依赖与 `before = [ "sysinit.target" ]` 形成排序环，
并且 `StandardOutput=console`/`StandardError=console` 不是该 systemd 版本可接受
的 output specifier。因此它没有产生任何 `/proc` 快照，不能据此推断 task 101。

临时诊断配置已改为 `unitConfig.DefaultDependencies = false`，保持
`after = [ "local-fs.target" ]` 和 `before = [ "sysinit.target" ]`，并在脚本开头
显式重定向到 `/dev/console`。同时正式 marker 改为
`StandardOutput=journal+console` 与 `StandardError=journal+console`，避免 marker
日志仅依赖当前仍不稳定的 journal 路径。该改动不 mask unit、不调整任何全局
sandbox 策略，也不改变 acceptance matcher。

下一步必须重建 rootfs 并重跑原有严格 Podman QEMU acceptance，确认快照实际出现，
再以 task 101 的命令行、父 PID 和状态决定是否建立一个最小红测。本文条目只记录
诊断配置的修正；尚未构成 T028 通过或可提交的独立成果。

### 7.43 快照单元已被调度，但最早脚本路径仍未产生输出（2026-08-03）

使用重建后的正式 artifact 运行原有严格 Podman + `.ci-cache` acceptance：

```text
system=/nix/store/y8jznjmlxcgky0phyjq584mfd0chjndg-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=df9e507b60eedb8ee9c69a93f96bdd6998825a51104d9c46ed779be54bef9a50
```

运行已明确越过早期阶段：

```text
Finished Apply Kernel Variables.
Finished Flush Journal to Persistent Storage.
Finished Create Static Device Nodes in /dev gracefully.
Finished Create Static Device Nodes in /dev.
Reached target Local File Systems.
```

随后 systemd 同时调度了三个无时限 job：

```text
task 99   Register Nix Store Paths
task 100  Capture StarryNixOS Stage-2 process state
task 101  Create System Files and Directories
```

其中 task 101 在 task 99 进入 `waitpid(-1)` 后启动，并出现一次 `keyctl`
ENOSYS；该时序不能证明 task 101 是 `nix-store --load-db`，也不能将 `keyctl`
归为停滞根因。到外层严格 runner 的 600 秒上限时，三个 job 都尚未完成：

```text
Error: QEMU timed out after 600s
```

尽管快照 unit 已不再因 ordering cycle 被删除，日志中仍为零次：

```text
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_BEGIN
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_END
```

且没有预期的 `sleep` 子进程痕迹。该 service 脚本在 `sleep` 前唯一的显式操作是
`exec >/dev/console 2>&1`；现有证据只能表明它很可能卡在脚本的最早路径，尚不能
直接证明 console 打开就是原因。为取得更小、可观察的对照，临时诊断现移除直接
打开 `/dev/console` 的重定向，改用 systemd 260.2 已验证可接受的
`StandardOutput=journal+console` 与 `StandardError=journal+console`。保留
`DefaultDependencies=false`、`after=local-fs.target` 和 `before=sysinit.target`；
未 mask 任何 unit、未放宽 acceptance matcher、未改变任何全局或额外 unit 的
sandbox 设置。

下一步重新构建同一 NixOS closure 并复跑严格 Podman QEMU acceptance。仅当快照
给出 task 99/101 的命令行、PPID 与状态后，才为确证的最小 Linux/Starry 差异建立
红测。T028 仍未完成，本轮诊断改动没有独立可提交成果。

### 7.44 移除 console 重定向后的首次严格复验仍早于快照失败（2026-08-03）

使用 7.43 的更新配置重新构建并发布 artifact：

```text
system=/nix/store/apirkvggx2f82lh8nprldszw25mkhypz-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=8d698c9d766197f2856b5d3f0007c2ade2d52b4331fb7e9eb90f331b50126003
```

随后以严格 Podman + `.ci-cache` runner 复验。NixOS activation 完成，且
`systemd-sysctl` 与第一个 `/dev` tmpfiles job 均以状态 0 结束；但
`systemd-journal-flush.service` 在 guest 约 151 秒失败：

```text
[FAILED] Failed to start Flush Journal to Persistent Storage.
=== FAIL PATTERN MATCHED: (?m)^Failed to start .+
```

严格 matcher 随即停止 QEMU，符合 failure contract。该终止早于
`local-fs.target` 与 `starry-nixos-process-snapshot.service`，因而本次**不能**
证明或否定移除 `exec >/dev/console 2>&1` 是否使快照可运行，也不产生 task
99/101 的进程信息。暂不对 journal flush、console 打开、`keyctl`、`kcmp` 或
`fcntl(1027)` 作根因推断；不 mask journal flush、不放宽 matcher、不改变
sandbox 范围。T028 仍未完成，本轮失败不提交。

### 7.45 console-less 快照确认 Nix path-registration 的活跃子进程（2026-08-03）

复用 7.44 的 artifact 进行第二次严格 Podman + `.ci-cache` 验收后，早期
`systemd-journal-flush.service` 以状态 0 完成；随后运行到 `local-fs.target`。
`run-wrappers.mount` 仍失败（其 SUID/SGID wrapper 非目标不作为本轮根因），但
systemd 继续调度后续事务。

移除直接 `/dev/console` 重定向的 snapshot service 不再停在首条脚本语句：

```text
task 100 -> sleep (task 103) -> STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_BEGIN
```

其 `ps` 快照给出了此前缺失的直接进程身份：

```text
PID  PPID  COMMAND              ARGS
 99     1  register-nix-pa      .../register-nix-paths-start
100     1  starry-nixos-pr      .../starry-nixos-process-snapshot-start
102    99  nix                  .../nix-store --load-db
```

因此 `register-nix-paths.service` 内由 task 99 等待的 child 是 task 102 的
`nix-store --load-db`。同时，task 101 的 `systemd-tmpfiles` 已经在快照前完成，
故 task 101 的 `keyctl` ENOSYS 不得再归因于 Nix path registration。上述结论只
确认 process identity；尚未确定 task 102 的阻塞 syscall、SQLite/VFS 子操作或
owning kernel subsystem。

快照的 `ps` 本身仍在输出时，`systemd-journal-catalog-update.service` 失败：

```text
[FAILED] Failed to start Rebuild Journal Catalog.
=== FAIL PATTERN MATCHED: (?m)^Failed to start .+
```

严格 matcher 正确停止 QEMU，因此没有 `STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_END`
或完整逐 PID `/proc` 记录，也没有 `STARRY_NIXOS_SYSTEM_PASSED`。该服务失败不是
已证实的 systemd sandbox requirement；不为继续诊断而 mask 它、不放宽 matcher、
不扩大 no-sandbox 边界。T028 仍未完成，本轮仅形成了后续最小回归所需的
`nix-store --load-db` workload 身份证据，尚不可提交。

### 7.46 定向 Nix `/proc` 快照的采样调整（2026-08-03）

7.45 表明 `ps -eo ...` 需要约十余秒才输出，在其完成前严格 runner 已可因无关
的 journal-catalog failure 终止 QEMU，导致逐 PID `/proc` 循环没有执行。为在同一
严格 failure contract 下取得 task 102 的状态，不增加 service mask、timeout、
sandbox exception 或 matcher 例外，只调整临时 snapshot service 的采样顺序：

1. 等待从 15 秒缩短为 3 秒；7.45 已证明 `nix-store --load-db` 在该 unit 启动后
   数秒内已出现。
2. 以 shell 内建 `read` 扫描 `/proc/*/comm`，只识别 command 为 `nix` 的进程。
3. 对每个匹配项立即输出 `cmdline` 与原始 `/proc/<pid>/status`，再发出 snapshot
   end marker；不再先运行全表 `ps`。

该脚本只用于 T028 诊断且会在定位后移除。它不硬编码 task/PID，不将 `nix` 的
出现视为成功，也不改变 guest 对 Nix、systemd 或 Linux ABI 的生产语义。下一次
必须重建 artifact 并用同一严格 Podman acceptance 验证：至少获得 task 102 的
`State`、`PPid`、cmdline 和 end marker，或如实记录新的更早 terminal failure。

### 7.47 `nix-store --load-db` 运行态与 sleep 延迟（2026-08-03）

按 7.46 的采样顺序重建 artifact 后，第三次严格 Podman + `.ci-cache`
验收到达 `local-fs.target`，并同时启动：

```text
task 99   register-nix-paths-start
task 100  starry-nixos-process-snapshot-start
task 101  systemd-tmpfiles
task 102  nix-store --load-db
task 103  sleep 3
```

task 102 于 guest `159.786s` 进入用户态。task 103 在 guest `141.486s`
进入用户态，但直到 guest `491.429s` 才退出；因此诊断服务请求的三秒等待在
该 Nix workload 运行期间延后约 350 个 guest 秒。随后快照完整输出：

```text
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_BEGIN
STARRY_NIXOS_DIAG_NIX pid=102 \
  cmdline=.../nix-store --load-db
Name:    nix
State:   R (running)
Tgid:    102
Pid:     102
PPid:    99
Threads: 1
voluntary_ctxt_switches:    0
nonvoluntary_ctxt_switches: 0
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_END
```

这进一步排除 task 101 是当前 Nix workload 的可能：systemd 随后报告
`Finished Create System Files and Directories.`。task 102 在采样时不是
文件锁、futex 或 I/O 等待态，而是 runnable/running；其观察到的零上下文切换
与等待中的诊断服务不能按预期时间恢复相一致。

该现象尚不足以单独宣布 scheduler 或 timer 实现存在缺陷：`/proc/status`
计数的更新点、Nix 的精确 userspace 操作、以及内核是否在 syscall 边界以外
提供时间片抢占仍未被同一最小测试证明。下一步先核对 x86_64 Starry build 的
preempt/timer 配置和现有调度测试，再设计能够在 Linux 与当前 StarryOS 上区分
“计算极慢”与“不可抢占”的确定性回归；不对 Nix 或 systemd 添加 timeout、mask、
matcher 放宽或新的 sandbox 例外。

本次严格 runner 在快照结束后按既有 failure contract 因无关 unit 终止：

```text
[FAILED] Failed to start Rebuild Journal Catalog.
=== FAIL PATTERN MATCHED: (?m)^Failed to start .+
```

它没有产生 `STARRY_NIXOS_SYSTEM_PASSED`，因此 T028 仍未完成。本轮仅新增临时
诊断及失败证据，不满足独立提交条件。

### 7.48 x86 单核 timer/preemption 最小回归设计（2026-08-03）

7.47 的现象不能直接归因于 Nix、SQLite、VFS 或 scheduler：它只证明在
`nix-store --load-db` 处于 runnable/running 状态时，另一个请求三秒等待的 task
被延后约 350 个 guest 秒。为避免对尚未确认的 owning subsystem 作推测性修复，
先完成了当前 x86_64 用户态 IRQ 和调度调用链检查：

```text
axruntime::init_percpu_irq
  -> request_percpu_irq(timer_irq_handler)
  -> axcpu x86_64 UserContext::run() 的用户态 IRQ 返回
  -> ax_cpu::trap::dispatch_irq()
  -> ax_hal::irq::handle_irq()
  -> axruntime::timer_irq_handler()
  -> ax_task::on_timer_irq()
  -> timers::check_events() + scheduler_timer_tick()
```

StarryNixOS app 的 `qemu-x86_64.toml` 没有 `-smp`，实际为单 CPU；现有
`qemu/system` wrapper 固定 `-smp 4`，不能替代该运行形态。`starry-kernel`
启用 `ax-task/sched-rr`，而该 feature 依赖 `preempt`。早期 someboot x86
`lapic_timer_handler()` 仅 ACK 的事实不构成直接修复依据：运行时已经安装上述
IRQ framework handler，不能据此修改 someboot。

最小回归将建立为独立的
`test-suit/starryos/timer-preemption-x86/` C QEMU case，并使用
`max_cpu_num = 1`、未指定 `-smp` 的 x86_64 配置。该程序：

1. 在无竞争时校准有限的 TSC 自旋预算；
2. `fork()` 一个只在用户态执行 `pause` 自旋的 child；
3. parent 对同一 CPU 请求短 `nanosleep()`；
4. 要求 parent 在宽松但有限的延迟上界内醒来，随后清理 child。

Linux `nanosleep(2)` 规定调用线程应挂起至请求的间隔经过或被信号打断；正常返回
为零。该用例不把 QEMU 外层 timeout 当作通过，也不依赖 `ITIMER_REAL`：当前
StarryOS 的 interval timer 信号还在 syscall 返回时 poll，单独以它作为 trigger
会混入另一个未隔离的语义。若该回归在当前单核 StarryOS 上红，再从它触达的
timer wakeup、IRQ return 和 RR preemption 边界定位 owning subsystem；在此之前
不修改 kernel、不 mask journal unit、不放宽 matcher，亦不增加 sandbox 例外。

该用例现已完成 Linux/Starry 对照，而非红测：

```text
Linux host:
PASS: nanosleep parent woke after 100017184 ns while the same-CPU child was CPU-bound

Podman + QEMU x86_64:
PASS: nanosleep parent woke after 144027536 ns while the same-CPU child was CPU-bound
STARRY_TIMER_PREEMPTION_X86_PASSED
```

Starry run 使用实际的单核 configuration（`max_cpu_num = 1`，最终 QEMU 命令带
`-smp 1`），并由 `cargo xtask starry test qemu --arch x86_64 -c
timer-preemption-x86` 发现、构建和执行。因此当前证据排除“所有 CPU-bound
用户任务都会阻断定时唤醒和 RR 抢占”的粗粒度假设；它不证明 Nix workload 没有
更窄的 scheduler、signal、VFS、内存或 userspace 算法问题。后续诊断应将
`nix-store --load-db` 的实际执行路径进一步缩减，或获得该 workload 下的
kernel-level 调度/IRQ 证据，而不是修改已被该测试覆盖的通用单核 timer 路径。

### 7.49 首次定向 `strace` 采样未进入附加阶段（2026-08-03）

为取得 `nix-store --load-db` 的直接 syscall 证据，临时诊断 closure 增加
`strace 7.1`，并让快照单元在识别到 command 为 `nix` 的 PID 后尝试附加两秒。
该诊断不替换 registration workload、不 mask unit、不修改严格 matcher，也不
改变 systemd sandbox 范围。

固定开发环境重建时请求了：

```text
NIX_CONFIG='sandbox = false'
```

但宿主 Nix daemon 明确报告当前用户不是 trusted user，因而忽略客户端
`sandbox` 设置。本次 artifact 仍在 daemon 实际配置下成功构建，不能把它记录为
已验证的 no-sandbox 构建。发布结果为：

```text
system=/nix/store/0wvsj09y6hxm1p1b95sxdf60agnhkvf0-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=032056b15999d3daded12c6ce90606212175369b43df634e49fcd6a8cac466e2
```

严格 Podman/QEMU 日志：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-run.log
sha256=b30faee74d268db3c38ca92c6a5914b9829d64242b53203ac164a115e51b1e93
```

该运行再次越过 sysctl、journal flush、静态 `/dev`、udevd 和
`local-fs.target`。task 101 的完整 tmpfiles 于 guest `340.123s` 结束；task
103 的三秒 sleep 于 `371.201s` 结束后，快照识别出：

```text
STARRY_NIXOS_DIAG_NIX pid=102 \
  cmdline=.../nix-store --load-db
```

但脚本仍先读取 `cmdline` 和 `/proc/102/status`，尚未输出
`STARRY_NIXOS_DIAG_STRACE_BEGIN` 时，`systemd-journal-catalog-update.service`
失败并触发既有严格 fail matcher。因此本次结果既不能证明 ptrace/strace 可用，
也不能证明其失败；采样根本没有进入附加阶段。

下一次只调整临时诊断顺序：识别到 Nix PID 后立即执行有界 `strace`，再读取
`cmdline/status`。不增加 mask、timeout、matcher 例外或 sandbox 例外。只有取得
实际 syscall 轨迹或明确 ptrace 错误后，才决定最低层红测和 owning subsystem。
T028 保持未完成，本轮没有可提交成果。

### 7.50 前移采样后的首次复跑被早期 journal failure 终止（2026-08-03）

临时脚本已将 `STARRY_NIXOS_DIAG_STRACE_BEGIN` 和两秒有界附加前移到识别
Nix PID 后的第一项操作，并使用新的 artifact：

```text
system=/nix/store/nrfkv5a9havk6x5anx4zxfh6c1c57agy-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=540f4700321b1cc4c80a34cc8ef934f5c12f3cc3c204f87623fe54440b889ee3
```

首次严格复跑日志为：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-immediate-run.log
sha256=6482e2ff33a232746241a88762b43ca26042783f1fccebee939a144472d86935
```

本轮 `systemd-sysctl` 和第一个 `/dev` tmpfiles 均退出 0，但
`systemd-journal-flush.service` 于 guest `146.132s` 失败，严格 matcher 随即
终止 QEMU。该时点早于完整 `/dev` tmpfiles、`local-fs.target` 和
`register-nix-paths.service`，日志中没有任何 `STARRY_NIXOS_DIAG_` marker。
因此这次运行没有验证前移后的 strace 路径，也不能将 journal flush 的偶发失败
解释为 Nix registration 根因。

下一步复用完全相同的 artifact 和严格 runner 重跑。保持 journal flush
未 mask、matcher 未放宽、Nix registration 未替换；只有运行到
`STARRY_NIXOS_DIAG_STRACE_BEGIN` 才评价 ptrace/strace 结果。T028 保持未完成，
本轮没有可提交成果。

### 7.51 两秒 strace 窗口未发出 ptrace 请求（2026-08-03）

使用 7.50 的相同 artifact 和严格 Podman runner 复跑后，本轮成功进入前移后的
附加窗口。完整日志为：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-immediate-rerun.log
sha256=b6799188843732c101247ae971eb98fd2ebc8670e86c828be90ea5859a3f72ad
```

快照服务识别到 task 102 为：

```text
/nix/store/2ff4pcybn1x6hbh4assw3dazzm29mplx-nix-2.34.8/bin/nix-store --load-db
```

并输出：

```text
STARRY_NIXOS_DIAG_STRACE_BEGIN pid=102
STARRY_NIXOS_DIAG_STRACE_END pid=102
```

BEGIN/END 之间没有 syscall 轨迹，也没有 `strace:`、`ptrace` 或 `PTRACE`
错误。task 107（strace 子进程）在外层两秒 timeout 发出 `SIGTERM` 后以 wait
status `36608`（shell exit 143）退出；task 106（timeout）以 wait status
`31744`（shell exit 124）退出。内核日志中也没有任何 `sys_ptrace <=` 入口，
因此不能把该结果解释为“成功附加后目标两秒内没有 syscall”：strace 尚未发出
首次 ptrace 请求。

附加尝试结束后的 `/proc/102/status` 仍显示：

```text
State: R (running)
TracerPid: 0
Threads: 2
voluntary_ctxt_switches: 0
nonvoluntary_ctxt_switches: 0
```

这次结果隔离出诊断工具启动窗口问题，不构成 Nix、SQLite 或 ptrace 内核语义的
确定性红测。下一次只把临时 strace 上限从两秒扩大到十秒，并显式输出 timeout/
strace 的 shell exit status；不修改 registration workload、不 mask journal
unit、不放宽严格 matcher，也不增加 sandbox 例外。只有观察到首次 ptrace 请求、
明确工具错误或实际 syscall 轨迹后，才决定最低层回归和 owning subsystem。
T028 保持未完成，本轮没有可提交成果。

### 7.52 十秒诊断 artifact 的首次严格运行提前终止（2026-08-03）

7.51 计划中的临时诊断已将 strace 上限扩大到十秒，并增加
`STARRY_NIXOS_DIAG_STRACE_END ... status=<code>`，随后在仓库固定
`nix develop` 环境中重建并发布：

```text
system=/nix/store/mjch11hzbhq33q53raqax2ak3217hpsi-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=d9d2ac94f933a86cb1eb19d0bf573542b3b2f93b7a64dec4acc144b3a4384c0f
```

严格 Podman + `.ci-cache` 首次运行日志为：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-10s.log
sha256=e116ef2d0181136cebc76485cbe9ac60142b22add88bedf6ad4d9ad7ee3c5382
```

本轮 activation 退出 0、systemd 排队 `multi-user.target`，但
`systemd-journal-flush.service` 在其 95 秒 unit timeout 后失败；同一时段
`systemd-sysctl.service` 也因 timeout 退出。严格 runner 随即匹配：

```text
=== FAIL PATTERN MATCHED: (?m)^Failed to start .+
```

该时点早于完整 `/dev` tmpfiles、`local-fs.target`、Nix registration 和 snapshot
unit，日志中没有 `STARRY_NIXOS_DIAG_` marker。因此本次结果没有执行十秒 strace
路径，也不能据此评价 ptrace 或 Nix workload。下一步复用完全相同的 artifact
和严格 runner 重跑；不修改 unit、matcher、sandbox 或 kernel。T028 保持未完成，
本轮没有可提交成果。

### 7.53 同 artifact 复跑证明 sleep 型诊断不可用（2026-08-03）

复用 7.52 完全相同的 artifact 和严格 Podman runner 重跑。完整日志为：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-10s-rerun.log
sha256=4db2efd092260896beaa754fba37768b8ab150ff87e908d6979513b6d0c99c5a
```

该次运行越过 journal flush、完整 `/dev` tmpfiles 和 `local-fs.target`。虽然
`run-wrappers.mount` 报告失败，严格 matcher 只按既有 contract 匹配
`Failed to start`，因此运行继续并启动：

```text
task 99  register-nix-paths-start
task 100/101  registration/snapshot 后续进程
task 102  snapshot 的 sleep 路径
```

可观察顺序为：

```text
Starting Register Nix Store Paths...
Starting Capture StarryNixOS Stage-2 process state...
task 102 enters userspace
task 101 enters userspace
```

此后直到外层 600 秒 QEMU timeout 都没有任何
`STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_BEGIN`。因此 snapshot 脚本尚未越过开头的
`sleep 3`，十秒 strace 本身仍未启动。结合 7.47 中同一个三秒 sleep 被延迟约
350 guest 秒的证据，继续依赖外部 sleep 作为采样门槛已不能稳定取得该 workload
的现场。

下一次删除临时 snapshot 开头的 `sleep`，改为不依赖 timer wakeup 的、次数有界
的 shell 内建 `/proc/*/comm` 轮询；发现 command 为 `nix` 后立即执行十秒
strace 并记录退出状态。该变化只调整诊断触发，不修改 Nix registration、
systemd unit sandbox、严格 matcher 或 kernel。T028 保持未完成，本轮没有可
提交成果。

### 7.54 无 sleep 轮询仍错过 Nix 进程启动窗口（2026-08-03）

删除 snapshot 脚本开头的 `sleep 3` 后，临时诊断改为最多 64 次、仅使用 shell
内建读取 `/proc/*/comm` 的有界轮询。该版本没有修改 registration workload、
systemd sandbox、严格 matcher 或 kernel。构建并发布的 artifact 为：

```text
system=/nix/store/4izsk9c4pic0jdzg74myc8d6vh5vpm5w-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=977e2c114e8c1e55d51757f0ad3353796a22511fea551d88a41329c136ee837d
```

严格 Podman + `.ci-cache` 运行日志为：

```text
.ci-cache/tmp/starrynixos-nix-load-db-strace-nosleep.log
sha256=99c466cf3fc399a83d7d4dd33bb367b40fa80f014eb1664d0852c94e5bbe623b
```

本轮 snapshot 脚本立即运行并完整输出：

```text
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_BEGIN
STARRY_NIXOS_DIAG_SCAN_END attempts=64 found_nix=0
STARRY_NIXOS_DIAG_PROCESS_SNAPSHOT_END
```

64 次扫描约跨越 19 guest 秒，但发生在 `nix-store --load-db` 可观察窗口之前；
随后既有 `systemd-udevd` failure 触发严格 matcher，运行如实失败。该结果说明
轮询次数或 sleep 时长都不能稳定地把独立 snapshot unit 与 registration 进程
对齐，继续扩大轮询只会增加竞态而不会形成确定性证据。

下一步改为从 registration 进程启动边界直接跟踪：临时、unit-local 覆盖
`register-nix-paths.service` 的 `ExecStart`，用 `strace` 从进程创建时运行原始
的 `nix-store --load-db < /nix-path-registration`，仅在其成功后继续执行原始
`rm /nix-path-registration` 和 `nix-env --set /run/current-system`。该诊断将
删除并行 snapshot unit，不替换输入、不 mask 其他 unit、不放宽 matcher，也不
新增 sandbox 例外。它仍是未提交的诊断手段；只有取得直接 syscall 证据后才
决定最低层确定性红测及 owning subsystem。T028 保持未完成。

### 7.55 启动时 strace artifact 首次运行在 registration 前终止（2026-08-03）

7.54 计划中的临时 `register-nix-paths.service` `ExecStart` 覆盖已成功求值、构建并
发布。生成的 unit 保留原始依赖、条件、`Type=oneshot` 和
`RemainAfterExit=true`，只将启动脚本替换为有界 strace 包装；包装脚本仍执行
完全相同的 `nix-store --load-db < /nix-path-registration`，且仅在退出 0 后
删除 registration 文件并设置 system profile。artifact 为：

```text
system=/nix/store/651d4vsvyzhshl5r3iv8chpm8qymjpsn-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=e95bc9e487781535b64ad5a41e5fdfb54bff3cd780e08dba3413300b4f438106
```

首次严格 Podman + `.ci-cache` 运行日志为：

```text
.ci-cache/tmp/starrynixos-registration-startup-strace.log
sha256=2e7d05846e72e13ba4acbaea1224e1c323a0a2e6e7b41573a8e64fa3e8172dc6
```

该运行在完整 `/dev` tmpfiles 结束后，`systemd-sysctl.service` 与
`systemd-journal-flush.service` 均因既有 timeout 失败，严格 matcher 匹配
`Failed to start Apply Kernel Variables` 并终止 QEMU。运行尚未到达
`local-fs.target` 或 `Starting Register Nix Store Paths...`，因此没有
`STARRY_NIXOS_DIAG_REGISTRATION_STRACE_BEGIN`、ptrace 请求或 syscall 轨迹；
本次结果不能评价启动时 strace。

同时，生成 unit 中的 `StandardOutput=console` 与 `StandardError=console` 被
systemd 260 报告为无效 output specifier 并忽略。这是临时诊断配置自身的问题，
不属于 StarryOS kernel 差异。下一版将两项恢复为此前已实际使用的
`journal+console`，随后重建并复跑；不修改 workload、其他 unit、matcher、
sandbox 或 kernel。T028 保持未完成，本轮没有可提交成果。

### 7.56 修正输出后的首次运行仍在 registration 前终止（2026-08-03）

将临时 registration unit 的标准输出和错误恢复为 `journal+console` 后，新
artifact 构建、发布成功：

```text
system=/nix/store/7hkdqi1133drh10lvckwabjp5gw42p6l-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=6e9fdd6c3989458d7c0e808681664eb32abedabd78fc08ac4a3915ad03d82fde
```

首次严格运行日志为：

```text
.ci-cache/tmp/starrynixos-registration-startup-strace-journal-console.log
sha256=91e431cf26641b41829b96417b76cd73f56ec14a6972ecdbe4667d66f1980f67
```

日志不再包含 output specifier 解析错误，证明临时 unit 文件有效。但本轮仍由
`systemd-sysctl.service` 和 `systemd-journal-flush.service` 的既有 timeout 在
guest 约 141 秒处触发严格 fail matcher，尚未到达 `local-fs.target` 或
registration unit，因而没有启动时 strace marker、ptrace 请求或 syscall
轨迹。

下一次不重建、不修改任何配置，复用完全相同的 artifact 运行严格 runner；
此前同 artifact 重跑曾跨过这些具有运行间波动的早期 unit。T028 保持未完成，
本轮没有可提交成果。

### 7.57 启动边界已命中，但额外 shell 链仍未进入 strace（2026-08-03）

复用 7.56 的相同 artifact 再次严格运行。日志为：

```text
.ci-cache/tmp/starrynixos-registration-startup-strace-journal-console-rerun.log
sha256=eed69d12b278f5a7388a61d9a7140245d958b2021bb2acbc64628ebb06dc03c8
```

本轮 `systemd-sysctl.service` 成功，完整 `/dev` tmpfiles 成功，并到达：

```text
Reached target Local File Systems.
Starting Register Nix Store Paths...
```

registration 的 task 99 于 guest `149.105806s` 进入用户态；另一个 task 100 于
`159.485872s` 进入用户态。但在既有 journal flush failure 被严格 matcher 于
guest 约 182 秒处理前，日志没有包装脚本第一条
`STARRY_NIXOS_DIAG_REGISTRATION_STRACE_BEGIN`，内核也没有 `sys_ptrace <=`。
这说明从 unit 启动边界取样消除了 PID 竞态，但当前
`bash -> timeout -> strace -> nix-store` 额外启动链仍过长，尚未实际进入
strace；不能据此判断 ptrace 或 Nix workload。

下一版删除 `writeShellScript` 和 `timeout` 中间层，使用 oneshot unit 的多条
`ExecStart` 直接启动 `strace`、`rm`、`nix-env`，并用 systemd
`StandardInput=file:/nix-path-registration` 向第一条命令提供完全相同的输入。
systemd 只会在前一条 `ExecStart` 成功后执行后一条，因此仍保持“registration
成功后才删除输入并设置 profile”的原语义；外层严格 QEMU 的 600 秒 timeout
作为本次诊断的硬边界。不修改其他 unit、matcher、sandbox 或 kernel。T028
保持未完成，本轮没有可提交成果。

### 7.58 直接启动 strace 隔离出 ptrace syscall-stop 停点（2026-08-03）

删除额外 shell/timeout 后，生成的 registration unit 已核对为三条顺序
`ExecStart`：

```text
strace -f -tt -T -s 96 nix-store --load-db
rm /nix-path-registration
nix-env -p /nix/var/nix/profiles/system --set /run/current-system
```

并由 `StandardInput=file:/nix-path-registration` 向第一条命令提供原始输入。
构建、发布的 artifact 为：

```text
system=/nix/store/lsl1xxqs5ql3prdqv0n64p3zjymzgjz8-nixos-system-starrynixos-starry-nixos-stage2
systemd_version=260.2
image_sha256=7ca6ec9743632c1a9abb72cbd18738ad742c63319b6d215fdef44d377a0224e1
```

严格 Podman + `.ci-cache` 日志为：

```text
.ci-cache/tmp/starrynixos-registration-direct-strace.log
sha256=0ca0d4660dc167abaf9528430b67e51ac8941019a46e0a72fe0c7a574c21cc68
```

本轮 sysctl、journal flush 和两阶段 `/dev` tmpfiles 均成功，随后越过
`local-fs.target` 并启动 registration。strace task 99 首次实际进入 ptrace；
内核记录的关键请求序列为：

```text
PTRACE_SEIZE        request=0x4206 pid=99
PTRACE_SETOPTIONS   request=0x4200 pid=101 data=0x1
PTRACE_GET_SYSCALL_INFO request=0x420e pid=101

PTRACE_SEIZE        request=0x4206 pid=102 data=0x5f
PTRACE_INTERRUPT    request=0x4207 pid=102
PTRACE_GETSIGINFO   request=0x4202 pid=102
PTRACE_SYSCALL      request=24     pid=102 data=SIGCONT
PTRACE_GETSIGINFO   request=0x4202 pid=102
PTRACE_SYSCALL      request=24     pid=102 data=SIGSTOP
```

task 101 是 strace 的能力探测子进程；在 `PTRACE_GET_SYSCALL_INFO` 后由 tracer
终止并退出，不应单独解释为 Nix workload 被 kernel 杀死。实际 tracee task 102
完成 `SEIZE -> INTERRUPT`，并在第二次 `PTRACE_SYSCALL` 后没有再向 tracer
发布可由 `waitpid(__WALL)` 观察的 stop。直到后续
`systemd-journal-catalog-update.service` 于 guest `583.151980s` 失败并触发
严格 matcher，日志没有新的 task 102 ptrace stop 或 syscall trace。

因此当前第一个可复现差异已经从模糊的 Nix registration hang 缩小为：
`PTRACE_SEIZE`/`PTRACE_INTERRUPT` 后带 group-stop 信号恢复的
`PTRACE_SYSCALL` 没有产生 strace 所需的下一次 wait-visible stop。下一步先在
`test-suit/starryos/qemu/system/` 增加只覆盖该请求序列的确定性回归，并在 Linux
上记录 oracle；必须先证明当前 StarryOS 为红，才修改
`os/StarryOS/kernel/src/` 的 ptrace stop/wakeup owning path。临时 registration
覆盖仍不提交，T028 保持未完成。

### 7.59 ptrace syscall-stop 确定性红测与 Linux 对照（2026-08-03）

新增聚焦 grouped QEMU case：

```text
test-suit/starryos/qemu/system/test-ptrace-seize-syscall-stop/
├── CMakeLists.txt
└── src/main.c
```

它以 3 秒的 `waitpid(__WALL | WNOHANG)` 有界轮询覆盖完整序列：

```text
PTRACE_SEIZE(PTRACE_O_TRACESYSGOOD)
PTRACE_INTERRUPT
waitpid -> SIGTRAP + PTRACE_EVENT_STOP
PTRACE_GETSIGINFO -> SIGTRAP
PTRACE_SYSCALL(SIGCONT)
waitpid -> SIGTRAP|0x80 syscall-entry stop
PTRACE_GETSIGINFO -> SIGTRAP
PTRACE_SYSCALL(SIGSTOP)
waitpid -> SIGTRAP|0x80 syscall-exit stop
PTRACE_SYSCALL(0)
waitpid -> SIGSTOP signal-delivery stop
```

直接宿主进程受执行沙箱的 seccomp/`NoNewPrivs` 限制，`PTRACE_SEIZE` 返回
`EPERM`，不能作为 Linux ABI 结论。改用具有 `SYS_PTRACE` 与未收紧 seccomp
的只读 Podman oracle：

```bash
podman run --rm --userns=keep-id \
  --cap-add=SYS_PTRACE --security-opt seccomp=unconfined \
  -v "$PWD:/workspace:ro" -w /workspace \
  ghcr.io/rcore-os/tgoskits-container:latest bash -lc \
  'cc -std=c11 -Wall -Wextra -Werror \
   test-suit/starryos/qemu/system/test-ptrace-seize-syscall-stop/src/main.c \
   -o /tmp/test-ptrace-seize-syscall-stop && /tmp/test-ptrace-seize-syscall-stop'
```

该 oracle 输出：

```text
PTRACE_SEIZE syscall-stop regression
PASS: PTRACE_SEIZE interrupt and syscall signal-stop sequence
```

当前 StarryOS 在相同聚焦 QEMU case 得到确定性红灯：

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/test-ptrace-seize-syscall-stop
```

```text
PTRACE_SEIZE syscall-stop regression
FAIL: PTRACE_INTERRUPT reports PTRACE_EVENT_STOP: status=0x137f signal=19 event=0
```

`0x137f` 是普通 `SIGSTOP`，而 Linux oracle 要求由 `PTRACE_INTERRUPT` 产生
`SIGTRAP` 与 `PTRACE_EVENT_STOP`（128）。当前实现还存在两个同一序列已覆盖的
缺口：`PTRACE_SEIZE` 错误忽略第 4 参数 options，且 `PTRACE_SYSCALL(SIGSTOP)`
从 syscall stop 恢复后的信号没有进入 signal-delivery ptrace stop。下一步仅在
`os/StarryOS/kernel/src/syscall/task/ptrace.rs` 与
`os/StarryOS/kernel/src/task/user.rs` 修正这三个状态转换；必须先跑聚焦 QEMU
绿测、`starry-kernel` clippy 和真实 NixOS registration 越界验证，才考虑独立
提交。临时 registration/strace unit 覆盖仍不提交。

### 7.60 ptrace syscall-stop 红绿回归已通过，待完整 gate（2026-08-03）

修复严格限于先前记录的 ptrace stop owning path：

1. `PTRACE_SEIZE` 现在从第四参数 `data` 读取并校验 options，立即保存
   `PTRACE_O_TRACESYSGOOD`，不再误读被 Linux 要求为零的 `addr`；
2. `PTRACE_INTERRUPT` 在 tracer/tracee 关系和 tracee TID 已验证后设置
   `PTRACE_EVENT_STOP`，并中断目标的用户返回路径；它不会再排队额外的用户可见
   `SIGTRAP`，从而避免该 synthetic signal 抢在 tracer 注入的 `SIGSTOP` 前；
3. `PTRACE_SYSCALL` 从 syscall-entry/exit stop 恢复时保留 tracer 指定的
   signal；该 signal 在 syscall-exit stop 之后按普通 signal-delivery stop
   再次交给 tracer 观察。

当前 head 的 CI-like Podman/QEMU 绿测命令：

```bash
cargo xtask starry test qemu --arch x86_64 \
  -c qemu/system/test-ptrace-seize-syscall-stop
```

输出已到达：

```text
PTRACE_SEIZE syscall-stop regression
PASS: PTRACE_SEIZE interrupt and syscall signal-stop sequence
STARRY_SYSTEM_TEST_PASSED: /usr/bin/starry-test-suit/test-ptrace-seize-syscall-stop
STARRY_GROUPED_TESTS_PASSED
result: 1/1 case(s) passed
```

`cargo fmt --all -- --check` 已通过。CI-like Podman 中
`cargo xtask clippy --package starry-kernel` 也已通过全部 25 个配置检查（包含
两套 aarch64 system 配置；仅有不影响退出状态的上游 `memchr` future-incompat
提示）。因此该修复还不能提交的唯一验证缺口是：真实 StarryNixOS registration
workload 必须越过 7.58 的 `PTRACE_SYSCALL(SIGSTOP)` 停点。

`PTRACE_INTERRUPT` 仅允许
`PTRACE_SEIZE` tracee 的完整 attach-vs-seize 区分是当前 process-level ptrace
状态模型之外的后续语义工作；本次不会把它表述为已完整支持。

### 7.61 ptrace 修复后的首次真实 app 复跑未到达 registration（2026-08-03）

使用与聚焦回归相同的 Podman 用户映射，并以 manifest 校验后的已发布 artifact
复跑：

```bash
STARRY_NIXOS_REUSE_ROOTFS=1 \
  cargo xtask starry app qemu -t nixos --arch x86_64
```

首次误省略 `--userns=keep-id`，使容器内的非 root UID 无法读取主机
`.ci-cache/cargo` 的 `0600` registry 文件；该次在 Cargo 解析前退出，未进入
QEMU。随后使用 keep-id 映射重跑，artifact reuse 已完成 ext4 和 manifest
验证，StarryOS 也已经构建启动。

该次 guest 在 `Apply Kernel Variables`、`Flush Journal to Persistent Storage` 和
首阶段 `/dev` tmpfiles 并发时停留到 unit timeout；约 guest `143s`，
`systemd-sysctl.service` 被 SIGTERM 终止，严格 matcher 因
`Failed to start Apply Kernel Variables.` 返回失败。此时未出现
`Reached target Local File Systems.`、`Starting Register Nix Store Paths...`、
`PTRACE_SEIZE` 或 `PTRACE_SYSCALL`。它因此不能证明也不能反驳 7.60 的 ptrace
修复；只记录为已有早期启动波动的未通过集成尝试，不纳入任何提交。

下一步不修改 kernel、rootfs、临时 registration/strace 覆盖或 matcher，使用
完全相同的 keep-id Podman command 重试一次。仅当运行实际越过 7.58 的
`PTRACE_SYSCALL(SIGSTOP)` 边界时，才能作为本次独立 ptrace 提交的
workload-crossing 证据。

### 7.62 ptrace 修复后的真实 workload 已越过原 syscall-stop 边界（2026-08-03）

按 7.61 所述完全相同的 keep-id Podman 命令复跑，guest 已依次到达：

```text
Reached target Local File Systems.
Starting Register Nix Store Paths...
PTRACE_SEIZE / PTRACE_INTERRUPT / PTRACE_GETSIGINFO
PTRACE_SYSCALL / PTRACE_GETREGSET / PTRACE_SYSCALL
```

在原来等待不到下一次 tracer 可见 stop 的 request 序列之后，strace 实际输出：

```text
execve("/nix/store/2ff4pcybn1x6hbh4assw3dazzm29mplx-nix-2.34.8/bin/nix-store",
       ["/nix/store/2ff4pcybn1x6hbh4assw3dazzm29mplx-nix-2.34.8/bin/nix-store",
        "--load-db"], ...)
    = 0 <142.484411>
```

这证明 tracer 已从此前的 `PTRACE_SYSCALL(SIGSTOP)` 停点继续执行，并观察到
tracee 的后续 `execve` 成功；因此 7.60 内核修复满足“真实 StarryNixOS workload
越过触发边界”的提交 gate。当前 run 仍在执行 `Register Nix Store Paths` 与
`Create System Files and Directories`，尚未产生
`STARRY_NIXOS_SYSTEM_PASSED`，故 T028 仍未完成，也不能把整机 Stage-2
acceptance 表述为通过。继续等待同一次运行的终态，并将其与该独立 ptrace 修复的
提交资格分开判断。

同一次运行最终在外层 QEMU 600 秒上限处结束：

```text
Error: QEMU timed out after 600s
```

超时前 `Create System Files and Directories` 已完成，随后又启动了
`Rebuild Journal Catalog` 和 `Record System Boot/Shutdown in UTMP`。strace 在
tracee `execve` 之后报告过一次：

```text
Stray PTRACE_EVENT_EXEC from pid 102, trying to recover...
```

但它继续报告同一 tracee 在 x32/64-bit mode 间切换，尚不能把这条诊断定为
600 秒超时的根因；须先建立独立的 Linux/Starry 对照和确定性红测。该完整 run
未通过，不能随本次提交混入；它仅作为证明 ptrace 修复已越过原触发边界的集成
证据。

### 7.63 `PTRACE_O_TRACEEXEC` 与 syscall-exit stop 的下一项对照（2026-08-03）

对 7.62 的事件顺序和当前 user-return 流程核对后，`execve` 在
`PTRACE_SYSCALL` 跟踪下的当前 StarryOS 顺序为：

```text
syscall-entry stop
syscall-exit stop
PTRACE_EVENT_EXEC stop
```

strace 已在第二步输出成功的 `execve(...)=0`，然后将第三步报告为
`Stray PTRACE_EVENT_EXEC`。当前实现的直接原因是 user-return loop 在
`handle_syscall()` 后无条件消费 `SyscallTraceState::Exit`，随后才消费 exec
pending flag。

Linux `ptrace(2)` 已明确：对 `PTRACE_SEIZE` tracee，`execve` 不产生额外
SIGTRAP；`PTRACE_O_TRACEEXEC` 使 tracer 观察到
`SIGTRAP | (PTRACE_EVENT_EXEC << 8)`。但手册不足以单独确定 syscall tracing
下 event-stop 与 syscall-exit-stop 的精确相对顺序。因此下一步不直接修改
kernel：先新增一个最小 `PTRACE_SEIZE | PTRACE_O_TRACEEXEC |
PTRACE_O_TRACESYSGOOD`、`PTRACE_SYSCALL`、`execve` 对照用例，在 Linux oracle
记录完整 stop 序列，并让当前 StarryOS 证明红测。只有两边差异稳定后，才修改
exec syscall-stop 的拥有路径。

Linux oracle 已通过：

```text
PTRACE_SEIZE TRACEEXEC syscall-stop regression
PASS: PTRACE_EVENT_EXEC replaces the execve syscall-exit stop
```

首次 CI-like Starry QEMU 调用在 guest 启动前失败：容器内当前非 root 用户映射
没有完整 rootfs ownership 权限，且 image 未提供 `fakeroot`，因此 Axbuild 在
grouped rootfs extraction 阶段明确返回失败。这不是内核红测，也不改变测试或
kernel；下一次使用同一 Podman image 的 root 身份运行完全相同的 `cargo xtask`
case，使 direct `debugfs` extraction 满足所有权前置条件。

容器 root 身份与 `--userns=keep-id` 仍不满足完整映射；在一次性容器内安装
`fakeroot` 后，QEMU 用例已真正进入 guest，但第一版用例在：

```text
PTRACE_SYSCALL resumes the group stop with SIGCONT: errno=ESRCH
```

处稳定红灯。Linux oracle 通过而 StarryOS 返回 `ESRCH`，这是一条真实但尚未与
NixOS strace 触发顺序一致的差异：实际 strace 在 `PTRACE_SEIZE` 后会先调用
`PTRACE_INTERRUPT`，再从 `PTRACE_EVENT_STOP` 用 `PTRACE_SYSCALL(SIGCONT)`
继续。因此本轮不据此扩展 kernel 修复范围；先把红测改为实际
`SEIZE -> INTERRUPT -> SYSCALL -> execve` 顺序，排除 initial group-stop
恢复的干扰，再对 `PTRACE_EVENT_EXEC` 与 syscall-exit 的相对顺序作出判断。

修订后的 Linux oracle 还确认一个必须保留的 group-stop 细节：tracee 在
`SIGSTOP` group-stop 中接受 `PTRACE_INTERRUPT` 时，wait status 为：

```text
SIGSTOP | (PTRACE_EVENT_STOP << 8)
```

而运行态 tracee 才会得到 7.59 已覆盖的
`SIGTRAP | (PTRACE_EVENT_STOP << 8)`。因此用例随即按 Linux oracle 改为期待
前者；这不是 StarryOS 语义差异，也不改变已提交的 running-tracee
`PTRACE_INTERRUPT` 回归。

### 7.64 初始 group-stop 的 `PTRACE_INTERRUPT` 已形成当前 Starry 红测（2026-08-03）

修订后的 `test-ptrace-seize-traceexec` 在 Linux oracle 中通过，并精确要求
initial `SIGSTOP` group-stop 中的：

```text
PTRACE_SEIZE(PTRACE_O_TRACESYSGOOD | PTRACE_O_TRACEEXEC)
PTRACE_INTERRUPT
waitpid(__WALL) -> SIGSTOP | (PTRACE_EVENT_STOP << 8)
```

当前 StarryOS 在 CI-like Podman grouped QEMU case 中没有发布该 stop；测试在
3 秒有界轮询后以 `ETIMEDOUT` 失败。该失败发生在 `PTRACE_INTERRUPT` 之后、
首次 `PTRACE_SYSCALL(SIGCONT)` 之前，因此尚未触及或判断
`PTRACE_O_TRACEEXEC` 与 execve syscall-exit stop 的顺序。

当前实现只把 pending `PTRACE_EVENT_STOP` 交给 tracee 的 user-return 路径，并
以 task interrupt 触发消费；initial group-stop 的 tracee 不会返回用户态，故
event 没有绑定为可被 tracer `waitpid()` 观察的 ptrace stop。下一步只检查并
修复这一 group-stop 情况下的 ptrace event-stop 状态发布与唤醒路径，保留
running-tracee 的 `SIGTRAP | PTRACE_EVENT_STOP` 语义，并先让该最小用例
red-to-green 后再继续 exec event 顺序验证。T028 仍未完成，当前新增用例和诊断
记录均不提交。

### 7.65 group-stop `PTRACE_INTERRUPT` 已越界，TRACEEXEC 用例进入下一处红测（2026-08-03）

对 7.64 的 owning path 修复后，Linux oracle 仍通过：

```text
PTRACE_SEIZE TRACEEXEC syscall-stop regression
PASS: PTRACE_EVENT_EXEC replaces the execve syscall-exit stop
```

CI-like Podman grouped QEMU 不再在 `PTRACE_INTERRUPT` 的 3 秒等待中超时。它已经
成功越过 initial group-stop 的 event-stop 断言，随后在向 execve 前推进 syscall
trace 时得到：

```text
FAIL: expected a syscall or group-stop before execve entry:
status=0x4057f signal=5 event=4
```

其中 `0x4057f` 是 `SIGTRAP | (PTRACE_EVENT_EXEC << 8)`。这证明当前补丁已发布
Linux 要求的 `SIGSTOP | (PTRACE_EVENT_STOP << 8)`，但 Starry 在该 group-stop
恢复路径中在 execve syscall-entry stop 前发布了 exec event。该结果与 7.63
基于真实 strace 的“syscall-entry、syscall-exit、exec event”观察不同，不能
将其中任一顺序泛化为完整语义。

下一步先以 `AX_LOG=info` 复跑同一最小 case，记录从
`PTRACE_SYSCALL(SIGCONT)` 到 event 的实际 Starry state transition，再在 Linux
oracle 中输出同一段 stop 序列。不得仅根据 event 的出现位置移动 user-return
逻辑；必须先分清 group-stop 续跑、syscall-entry 与 exec-event 的所有权。
T028 未完成，当前 kernel/test 改动尚未达到提交条件。

### 7.66 group-stop event-stop 已对齐，syscall trace 在 execve 前仍失真（2026-08-03）

7.65 的 `PTRACE_INTERRUPT` group-stop 发布路径已在 Linux oracle 和 Starry
CI-like Podman/QEMU 中对齐。两边均先观察到：

```text
SIGSTOP group-stop
SIGSTOP | (PTRACE_EVENT_STOP << 8)
SIGSTOP | (PTRACE_EVENT_STOP << 8)
```

Linux 随后仅报告一个 `SIGTRAP|0x80` 的 `execve` syscall-entry stop，紧接着为
`SIGTRAP | (PTRACE_EVENT_EXEC << 8)`。当前 Starry 在相同位置报告四个
`SIGTRAP|0x80` stop，最后才出现 `PTRACE_EVENT_EXEC`：

```text
SIGTRAP|0x80
SIGTRAP|0x80
SIGTRAP|0x80
SIGTRAP|0x80
SIGTRAP | (PTRACE_EVENT_EXEC << 8)
```

这已排除 initial group-stop event 发布问题，但尚不能仅凭 stop 数量判断 entry/
exit 状态机的具体错误。下一步在最小回归中打印每个 syscall stop 的
`orig_rax` 与 `rax`，用 Linux/Starry 对照确认它们各自对应的 syscall-entry 或
syscall-exit，再修正 `ProcessData` 的 per-TID syscall trace 状态消费与重设。
不得猜测性移动 `PTRACE_EVENT_EXEC` 的消费顺序。T028 仍未完成；该诊断和未通过
的回归不得提交。

### 7.67 syscall-stop 寄存器对照确认状态机续跑错误（2026-08-03）

诊断版最小用例在 Linux oracle 中输出：

```text
PTRACE_SYSCALL_REGS: orig_rax=59 rax=-38
PTRACE_STOP: PTRACE_O_TRACEEXEC replaces execve syscall-exit stop
```

即 Linux 只在 `execve` syscall-entry 停止，x86_64 的 `rax=-ENOSYS` 符合
ptrace syscall-entry ABI；恢复后直接报告 `PTRACE_EVENT_EXEC`，而不是
`execve` syscall-exit stop。

同一诊断在 CI-like Podman/QEMU 中稳定红灯，连续报告：

```text
PTRACE_SYSCALL_REGS: orig_rax=14 rax=14
PTRACE_SYSCALL_REGS: orig_rax=0 rax=0
PTRACE_SYSCALL_REGS: orig_rax=59 rax=59
PTRACE_SYSCALL_REGS: orig_rax=0 rax=0
PTRACE_STOP: ... status=0x4057f signal=5 event=4
```

因此 Starry 对每个 syscall 都重复发布了同一类 stop，且没有在 syscall-entry
向 user-visible x86_64 `rax=-ENOSYS` 状态转换。`orig_rax=59` 已证明
`PTRACE_EVENT_EXEC` 本身发生在真实 `execve` 之后；错误的 owning boundary 是
`PTRACE_SYSCALL` 从 ptrace stop 恢复时，per-TID entry/exit trace state 被重置
为新的 `Entry`，而非延续当前 syscall 的 `Exit`。下一步只修正该状态延续与
entry-stop ABI，再用同一 Linux/Starry 用例验证，不提前调整 exec-event 顺序。
T028 未完成，所有改动保持未提交。

### 7.68 `PTRACE_SYSCALL` entry/exit 续跑与 exec event 已修复（2026-08-03）

修复将 per-TID syscall trace state 保留到 tracer resume，而不再由
user-return path 在发布 stop 时消费。`PTRACE_SYSCALL` 仅在它恢复的 stop 本身
是 syscall boundary 时执行 `Entry -> Exit -> Entry` 切换；从 group、event 或
signal-delivery stop 恢复时明确从下一次 syscall-entry 开始。这样不会把
`PTRACE_EVENT_EXEC` 后的下一次 `PTRACE_SYSCALL` 错当成一个 syscall-exit
resume。

同时：

- x86_64 `PTRACE_GETREGSET(NT_PRSTATUS)` 在 syscall-entry stop 对 tracer
  公开 `rax=-ENOSYS`，保持 `orig_rax` 为真实 syscall 号；
- `PTRACE_SETREGSET` 在该 boundary 使用 Linux `orig_rax` 选择待执行 syscall，
  不把合成的 `rax=-ENOSYS` 写回 kernel 执行上下文；
- `execve` 成功后优先发布 `PTRACE_EVENT_EXEC`，不再发布额外 syscall-exit
  stop。

Linux oracle 仍通过，关键记录为：

```text
PTRACE_SYSCALL_REGS: orig_rax=59 rax=-38
PTRACE_STOP: PTRACE_O_TRACEEXEC replaces execve syscall-exit stop
```

同一 CI-like Podman/QEMU grouped regression 现在通过：

```text
PTRACE_SYSCALL_REGS: orig_rax=14 rax=-38
PTRACE_SYSCALL_REGS: orig_rax=0 rax=0
PTRACE_SYSCALL_REGS: orig_rax=59 rax=-38
PTRACE_STOP: PTRACE_O_TRACEEXEC replaces execve syscall-exit stop status=0x4057f
PASS: PTRACE_EVENT_EXEC replaces the execve syscall-exit stop
STARRY_GROUPED_TESTS_PASSED
```

这条修复已满足新增最小回归的 Linux/Starry 红绿验证；尚须运行 workspace
format、`starry-kernel` clippy 和既有 `test-ptrace-seize-syscall-stop` 回归，
再判断是否具备独立提交资格。T028 仍未完成，真实 StarryNixOS workload 也尚未
在本次修复后复跑。

### 7.69 epoll LT 公平性与 rearm 丢唤醒修复完成独立 gate（2026-08-06）

`bugfix-epoll-lt-fairness` 的 Linux oracle 在一个持续可读 pipe 之后返回 Unix
listener；修复前 Starry 连续八次只返回 distractor，修复后当前 Podman/QEMU
复跑输出：

```text
PASS: listener returned after 1 persistent distractor events
STARRY_EPOLL_LT_FAIRNESS_PASSED
STARRY_GROUPED_TESTS_PASSED
```

同一 epoll 路径的 consume-to-register race 使用内核 axtest 建立确定性模型：
旧 wake 已消费后，replacement waker 注册期间 readiness 变为可见。修复前
`AXTEST_SUMMARY pass=396 fail=1`，注册后按订阅 mask recheck 并经正常 waker
入队后变为 `pass=398 fail=0`。

Podman + `.ci-cache` 质量 gate：

```text
cargo fmt --all -- --check
cargo xtask clippy --package starry-kernel
```

格式检查通过，Clippy 25/25 配置通过。聚焦公平性 QEMU 1/1 通过。

随后以 manifest 受检 rootfs 运行：

```text
STARRY_NIXOS_REUSE_ROOTFS=1 \
  cargo xtask starry app qemu -t nixos --arch x86_64
```

QEMU 命令明确包含 `-smp 4`，CPU 1/2/3 初始化成功，NVMe 建立 4 个 hctx。该
运行完成 journal flush、两阶段 `/dev` tmpfiles、udevd、journal catalog、
`systemd-update-utmp` 和 Nix store path registration；其中
`systemd-update-utmp` 退出码为 0。新的严格终态为：

```text
Failed to start Initialize NixOS Channel.
```

该失败约出现在 guest 233 秒，未产生 `STARRY_NIXOS_SYSTEM_PASSED`，所以 T028
仍未完成；但真实 workload 已跨过 epoll 修复的触发边界，epoll 修复本身满足
独立提交 gate。日志：

```text
.ci-cache/tmp/bugfix-epoll-lt-fairness-verified.log
.ci-cache/tmp/starrynixos-epoll-fairness-verified-smp4.log
```

## 8. 关联文档

- `silicalet/TODO.md`：总体路线与长期门槛；
- `silicalet/003-starryos-nixos-optionB-research.md`：早期方案 B 调研；
- `silicalet/NIXOS-COMPAT-RESEARCH.md`：Linux ABI/NixOS 对照研究；
- `specs/004-add-starry-nixos/`：本轮规范、计划、任务和契约；
- `apps/starry/nixos/README.md`：用户侧构建和运行边界；
- `apps/starry/nixos/compatibility.md`：精确运行证据和兼容性账本。
