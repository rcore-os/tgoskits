# Axvisor 自带资源与根切换验收记录

## 1. 实现边界

初次本地验收基于 `dev` `72a5528bf6a2f0a4e2998f420ef34f8339a8f652`，日期为 2026-10-08；后续变基到 `8c2160843220803f101c131a6a33ebb10d94e2e3`，增量验证见第 6 节。共享根切换与归档回收对照本地 Linux v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`；主要依据是 `init/initramfs.c`、`init/main.c` 和 `fs/namespace.c`。启动契约、资源生产者与板卡迁移要求保存在 [宿主 initramfs 启动契约](host-initramfs.md)。

### 1.1 无盘启动

`axvisor::builtin::prepare_root()` 在未请求磁盘根、没有块设备驱动或没有注册宿主块设备时保留内存根，直接读取 initramfs 的 `/guest/builtin`。即使命令行继承了 `root=`，无块设备也不会尝试切根。`axbuild` 对未接宿主盘且未传 `--rootfs` 的显式 QEMU 配置不下载、不插入宿主磁盘。

有块设备且显式请求磁盘根时，`prepare_block_root()` 必须成功准备目标；只读根、无效根选择、空间不足或自带资源安装失败均停止 VM 启动。`PreparedRoot::commit()` 只在安装成功后发布新根。ArceOS 在应用前处理显式根；Starry 有早期 init 时保留内存根，由 init 切换，没有早期 init 时内核切换，早期 init 执行失败不重新挂载磁盘。

### 1.2 自带资源发布

`axbuild::axvisor::bundle` 将 kernel、DTB、BIOS、UEFI firmware 和客户机 initrd 放入 `/guest/builtin/images`，生成的配置始终引用最终路径。客户机可写磁盘路径维持原配置。`vm_configs` 和 `--vmconfig` 仅作为归档输入，Axvisor 删除 `fs` feature、`image_location`、静态配置回退及客户机二进制嵌入。

`ax_fs_ng::bundle::install_directory()` 完成整包暂存、完整性校验、刷盘和发布。Ext4 使用目录 `EXCHANGE`；FAT 等后端使用备份、发布及失败回滚。目录替换删除旧包独有文件；缺失源目录保留旧包，空包清空旧资源。FAT 路径不保证断电原子性。`selected_configs()` 优先有效非空用户配置集，用户配置无效时报告错误。

## 2. 项目检查

验证使用仓库 `cargo xtask` 入口和固定 `nightly-2026-09-04` 工具链。本节和第 3 节保存初次本地验收日志，不能代替后续提交的远程 CI 或未运行目标的结果。

### 2.1 静态与标准库检查

`cargo xtask test --since` 选择已提交差异中的受影响软件包，最后一轮覆盖全部语义改动。标准库检查之后的代码调整只补齐 UEFI 模板的 `root=`、调整 Starry 源码格式并保留构建脚本原有版权注释，没有更改 Rust 行为。

| 检查 | 结果 | 本地证据 |
| --- | --- | --- |
| `cargo fmt --all`、`git diff --check` | 通过 | 工作区最终差异 |
| `cargo xtask test --since 72a5528bf6a2f0a4e2998f420ef34f8339a8f652` | 17 个软件包全部通过；axbuild 427 项测试 | `/tmp/axvisor-dev2-std-final.log` |
| 变基后定向 Clippy：ax-fs-ng、axbuild、axvm、arceos-helloworld | 4 个软件包、37 个检查通过 | `/tmp/axvisor-dev2-clippy.log` |
| CI 规划器测试 | 85 项通过 | `/tmp/axvisor-dev2-ci-plan.log` |

Clippy 命令同时请求过 `--package axvisor`，现有通用入口明确跳过该软件包，因为需要专用目标及构建配置；没有把这项跳过写成通过。Axvisor 本身由实际目标构建、AArch64 内核测试及 SVM/HTTP 系统测试验证。

CI 规划器检查使用 `uv run --python 3.13 --no-project python3 -m unittest discover -s scripts/test -p 'test_ci_*.py'`。`.github/ci/checks/axvisor.toml` 中仅保留一个 SVM `kvm-amd` 作业，`cache_key = ""`、`timeout_minutes = 120`，用一条命令选择六个用例；规划出的 self-hosted AMD/KVM 作业与该登记一致。

### 2.2 确定性回归

回归采用同一测试恢复旧实现后失败、修复后通过的方式。`MemoryFs` 生命周期测试检查实际页分配/回收计数，未仅凭引用计数或日志宣称内存释放。

| 保护的行为 | 旧实现失败证据 | 修复后的证据 |
| --- | --- | --- |
| 未发布 MemoryFs 不形成自引用循环 | `/tmp/axvisor-memory-red.log` | `/tmp/axvisor-root-green-2.log`、最终标准库测试 |
| 同点 pivot 后旧根不可遍历，旧文件及页 pin 继续可用 | `/tmp/axvisor-pivot-red.log` | `pivot_detaches_old_root_and_defers_pages_until_last_open_reference` 的最终标准库结果 |
| 切根后的命名空间复制保留实际根 | `/tmp/axvisor-namespace-red-2.log` | 同一生命周期测试，最终标准库结果 |
| 未接宿主盘时继承 root 参数不补盘 | `/tmp/axvisor-diskless-std-red.log` | `explicit_diskless_qemu_keeps_host_rootfs_unattached`，最终标准库结果 |
| 没有块驱动时继承 root 参数不切根 | `/tmp/axvisor-no-driver-red.log` | `/tmp/axvisor-dev2-no-driver.log`：85 通过、0 失败，`AXTEST_SUITE_OK` |
| 不存在的显式根返回错误，保持当前根 | `/tmp/axvisor-root-prepare-red.log` | `missing_requested_root_returns_an_error_without_publishing`，最终标准库结果 |

`pivot_detaches_old_root_and_defers_pages_until_last_open_reference` 在关闭文件后确认 MemoryFs owner 已释放，只有映射页 pin 继续持有一页；关闭最后 pin 后分配与释放计数完全相等。测试名称及实现位于 `fs/ax-fs-ng/src/fs/memory.rs`。

资源替换通过 `directory_install_replaces_recovers_and_preserves_on_validation_failure` 和 `failed_publication_flush_restores_the_installed_directory` 验证旧独有文件删除、缺失源保留、空包清理、遗留备份恢复、容量不足、只读拒绝及发布刷盘失败回滚。`fs/ax-fs-ng/src/fs/ext4/rsext4/fs.rs` 使用真实 Ext4 后端和 64 MiB 内存块设备验证 `EXCHANGE`，并确认已打开旧文件仍可读取。备份算法故障注入使用真实 MemoryFs 后端；没有模拟物理 FAT 断电。

## 3. 运行时验收

三套 OS 的测试均从真实 QEMU 启动入口运行。内存根继续使用时，解包文件系统按正常生命周期保留；原始归档完整页可以在解包借用结束后单独回收。

### 3.1 内存根与磁盘根

`PreparedRoot::commit()` 通过不可变命名空间底座上的实际根完成 pivot，更新同命名空间仍指向旧根的 root/cwd，脱离旧根。Starry 的早期 init 直接发起系统调用，覆盖物理 Ext4 挂载、同点 pivot、旧文件及映射继续读取和磁盘上的 exec。

| 系统与入口 | 已验证行为 | 本地证据 |
| --- | --- | --- |
| ArceOS `FEATURES=initramfs-smoke`，helloworld `qemu-host-initramfs-aarch64.toml` | 应用从外部归档内存根读取文件 | `/tmp/arceos-dev2-memory-root.log` |
| ArceOS `FEATURES=block-root-smoke`，helloworld `qemu-host-initramfs-root-aarch64.toml` | 应用启动前切到 NVMe 根，读取磁盘独有 `/etc/alpine-release` | `/tmp/arceos-dev2-disk-root.log` |
| Starry `qemu/host-initramfs` | 内存根上的早期 init | `/tmp/starry-dev2-ramfs-root.log` |
| Starry `qemu/host-initramfs-disk-fallback` | 无早期 init 时内核切到磁盘根 | `/tmp/starry-dev2-kernel-root.log` |
| Starry `qemu/host-initramfs-switch-root` | init 自行切根、detach；旧 FD 读取；成功 close 后 mmap 读取；munmap；磁盘 exec | `/tmp/starry-dev2-user-root.log` |
| Starry `qemu/system/test-pivot-root` | 普通 pivot、非挂载目录 EINVAL、脱离后旧根不可遍历 | `/tmp/starry-dev2-pivot-root.log` |
| Starry `qemu/system/test-pivot-root-namespace` | 子进程私有命名空间切根不改变父进程根 | 未重新运行（本轮仅运行 `test-pivot-root`） |
| Starry `qemu/system/syscall-test-mountinfo` | 实际根、source、bind、传播字段与 detach 可见性 | 未重新运行 |
| Axvisor `ktest qemu -p axvisor --test axtest --arch aarch64`，`build-memory-root-aarch64.toml`、`qemu-memory-root-aarch64.toml` | 无块驱动、继承 root 参数保留内存根；用户配置优先、无效用户配置拒绝 | `/tmp/axvisor-dev2-no-driver.log`：85/85 |
| Axvisor `axvisor test qemu --arch aarch64 --test-case http-control-plane` | 安装自带资源、切根后删除并重新创建和启动 VM | `/tmp/axvisor-dev2-http-root-rerun.log`：1/1 |

无驱动内核测试的完整选择命令显式使用不启用块驱动的构建配置，避免默认测试配置把无驱动分支变成已注册磁盘的分支。

```bash
cargo xtask ktest qemu -p axvisor --test axtest --arch aarch64 \
  --config os/axvisor/tests/build-memory-root-aarch64.toml \
  --qemu-config os/axvisor/tests/qemu-memory-root-aarch64.toml
```

Axvisor HTTP 日志分别记录切根后 `releasing decoded ramfs filesystem (53532221 file bytes)`；客户机重建使用已经安装的配置与镜像。SVM Smoke 同时检查宿主 NVMe 读写和双 VM 可写磁盘隔离。内核测试工具会自动接测试盘，因此无驱动用例证明驱动不可用的分支；SVM direct ACPI 使用编入 NVMe 驱动但未接宿主盘、继承 `root=/dev/nvme0n1 rw` 的配置，证明无宿主设备分支。

### 3.2 SVM 单次构建

六个用例共享 `test-suit/axvisor/normal/qemu-svm/build-x86_64-unknown-none.toml`，配置包含 `vpci-test-device`；每个用例独立准备宿主归档。日志 `/tmp/axvisor-dev2-svm.log` 中只有一次 Axvisor 构建开始和一次 `Compiling axvisor`，六次启动的内核 SHA-256 全部为 `c30ab4772de7db35a10cd09811ef22ede88391331a08f635c0e7cf3688e46fb8`。

```bash
cargo xtask axvisor test qemu --arch x86_64 \
  --test-case smoke-svm,direct-acpi-svm,ovmf-acpi-svm \
  --test-case pci-enumeration-svm,pci-block-rw-svm,pci-block-ro-svm
```

本次命令同时使用重复参数与逗号列表，结果为 6/6 通过；运行器继续执行单例失败后的其他用例并最终返回非零的行为由 axbuild 标准库测试覆盖。各启动归档的独立内容哈希如下。

| 用例 | 结果与用例耗时 | 宿主归档 SHA-256 |
| --- | --- | --- |
| Smoke | 通过，39.85s | `224c515bd2bbdec936d031cfc26808620874c2535157b68a114c4640c19d21b6` |
| direct ACPI | 通过，11.67s | `2ffa68262719306a661388535c601fcde4753d6ce88264b15b7b2f61f0f19f54` |
| OVMF ACPI | 通过，26.88s | `4abf46f2a7ed357722bac6e6b7f9f891c667246112e257760fdd9e2398e3d6cb` |
| PCI 枚举 | 通过，14.57s | `2db66b31491948e75dc30ae58f8c70bb238c3683e8f024a08cf206fba91e152d` |
| PCI block RW | 通过，19.74s | `164bb03c15ef85733de63cb563f05b5334747f0681aabe9aa8127be62b7795d6` |
| PCI block RO | 通过，14.86s | `70ad20583cbfad392a4176489d267eacb817a9c7001da8e410e1fb393c3fa326` |

## 4. 未运行目标

`ensure_guest_image_bundles()` 的宿主测试覆盖全部五类启动资源的获取、文件检查及缺失拒绝。OrangePi 与 Phytium 的发布资源实际经过 `cargo xtask image pull` 下载、校验和解包；定制 BSP、initrd、AXIVC Linux 与机器人 Zephyr 仍需板卡 runner 提供 `AXVISOR_GUEST_ASSETS`，不能由通用镜像替代。

初次本地验收未运行实体板卡 FIT、U-Boot、HTTP Boot 客户机启动、VMX、LoongArch/RISC-V 客户机及三套 OS 的全部架构矩阵。后续 CI 和定向执行记录见第 6 节；迁移、旧提交通过和运行中的任务都不等于当前提交通过。实际 FAT 媒体故障、断电恢复未验证。通用 Clippy 没有 Axvisor 的专用 lint 入口。

## 5. 系统调用兼容性

对照只评价本次根切换、命名空间及旧文件生命周期涉及的行为。编号顺序为 AArch64 / RISC-V 64 / LoongArch 64 / x86_64；实际 Starry 系统回归在 AArch64 执行，其他架构运行兼容性无法确认。表中“正确”限定于测试列明确的场景，未宣称完整参数、权限与所有错误顺序兼容。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| mount(物理 Ext4) / 40/40/40/165 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 管理员挂载块设备后，目标路径访问该文件系统 | sys_mount → mount_ext4 → new_filesystem_from_handle → Location::mount_with_source；任务 namespace | 正确 | host-initramfs-switch-root：NVMe Ext4 实际挂载并读取磁盘文件 |
| mount(物理 Ext4, MS_RDONLY) / 40/40/40/165 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 只读设备或挂载限制禁止写入 | sys_mount → mount_ext4 → Mountpoint::set_readonly；native filesystem | 无法确认 | 未运行 Starry 物理只读挂载回归 |
| mount(MS_MOVE) / 40/40/40/165 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 移动现有挂载，保持设备内容可访问 | sys_mount → Location::move_mount → Mountpoint；任务 namespace | 正确 | host-initramfs-switch-root：移动 /dev 后 exec 磁盘 init |
| mount(MS_BIND) / 40/40/40/165 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4363) | 允许目录绑定自身，新挂载保留源根 | sys_mount → Location::bind_mount → Mountpoint；任务 namespace | 正确 | test-pivot-root、syscall-test-mountinfo：自身绑定与 mountinfo |
| pivot_root(普通) / 41/41/41/155 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4759) | 挂载根移动到 put_old；普通目录返回 EINVAL；调整同 namespace 根/CWD | sys_pivot_root → FsContext::pivot_root → pivot_mount → propagate_pivot_root | 正确 | test-pivot-root：拒绝非挂载目录、切根、detach 后旧根不可遍历 |
| pivot_root(同点) / 41/41/41/155 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4759) | pivot_root(".",".") 堆叠旧根，随后可 detach | sys_pivot_root → FsContext::pivot_root → Mountpoint::pivot_mount；同 namespace FsContext | 正确 | host-initramfs-switch-root：原始 syscall 同点 pivot 与 detach |
| umount2(MNT_DETACH) / 39/39/39/166 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2068) | 从路径树脱离；已有文件与映射继续可用 | sys_umount2 → detach_mount → UnmountPlan；mount lease、inode/page cache | 正确 | 切根测试保留旧 FD/mmap；mountinfo 检查 detach 后行消失 |
| umount2(普通) / 39/39/39/166 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L2068) | 忙挂载拒绝；成功前同步，提交后路径失去挂载 | sys_umount2 → plan_unmount → busy/sync → commit_unmount | 无法确认 | 未执行当前改动的普通卸载忙检查系统回归 |
| unshare(CLONE_NEWNS) / 97/97/97/272 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L3314) | 复制当前挂载树，子 namespace 操作不改变原 namespace | sys_unshare → PreparedUnshare → FsContext::unshare_mount_namespace → clone_namespace | 正确 | test-pivot-root-namespace：子切根，父 /bin/sh 仍可访问 |
| clone(CLONE_NEWNS) / 220/220/220/56 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L2831) | 创建子 namespace 时复制当前有效根；CLONE_FS 有独立约束 | sys_clone → CloneArgs::do_clone → FsContext::unshare_mount_namespace；子任务 scope | 无法确认 | fork+unshare 系统测试不证明 clone 的 NEWNS 路径 |
| clone3(CLONE_NEWNS) / 435/435/435/435 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L3003) | 校验 clone_args 后建立子 namespace | sys_clone3 → CloneArgs::do_clone → 子任务 FsContext 与 NsProxy | 无法确认 | 未执行 clone3 NEWNS 系统回归 |
| setns / 268/268/268/308 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/nsproxy.c#L569) | 按 nsfd/pidfd 与权限切换调用者的 namespace | sys_setns → set_mount_namespace → root/cwd 重定位；任务 FsContext/NsProxy | 无法确认 | 未执行 setns 的两类 FD 系统回归 |
| chdir / 49/49/49/80 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L550) | 从当前根解析目录，更新 CWD，随后相对路径使用新目录 | sys_chdir → FsContext::set_current_dir → current_dir；任务 scope | 正确 | host-initramfs-switch-root：进入新根、pivot 后回到 / |
| fchdir / 50/50/50/81 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L571) | 由已打开目录 FD 更新 CWD | sys_fchdir → Directory::current_dir → FsContext::set_current_dir | 无法确认 | 未运行跨 detach 的目录 FD fchdir |
| chroot / 51/51/51/161 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L588) | 改变进程路径解析根，CWD 有独立语义 | sys_chroot → resolve → FsContext::new；任务 root/cwd | 无法确认 | 未运行 chroot 与 pivot 交互、权限回归 |
| getcwd / 17/17/17/79 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/d_path.c#L413) | 返回相对进程根的 CWD，并处理脱离/不可达目录 | sys_getcwd → current_dir::absolute_path → 用户缓冲区 | 无法确认 | 未运行 chroot 或脱离目录的 getcwd |
| openat / 56/56/56/257 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1381) | 切根后绝对路径从新根查找，原打开文件仍持有旧对象 | sys_openat → with_fs → OpenOptions::open_with_credentials；FsContext/FD File | 正确 | host-initramfs-switch-root：前后分别打开内存文件、磁盘文件 |
| openat2 / 437/437/437/437 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1389) | 按 open_how/resolve 限制路径解析 | sys_openat2 → resolve 检查或 sys_openat → OpenOptions；任务 FsContext | 无法确认 | 未运行 openat2 resolve 与切根交互 |
| mkdirat / 34/34/34/258 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namei.c#L5322) | 在当前根/CWD 下建立目录 | sys_mkdirat → with_fs → create_dir → inode；任务 FsContext | 正确 | 早期 init 原始 syscall 建立 /newroot，再挂载 Ext4 |
| read(旧文件) / 63/63/63/0 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L724) | detach 不改变已打开文件内容与读偏移 | sys_read → fd File::read → CachedFile；inode/page cache | 正确 | host-initramfs-switch-root：detach 后旧 FD 内容与原始数据一致 |
| read(mountinfo) / 63/63/63/0 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/proc_namespace.c#L136) | 展示调用进程可见挂载，挂载点相对进程根 | sys_read → MountTableFile/File → render_mountinfo → FsContext::walk_tree/path_from | 正确 | syscall-test-mountinfo 与 test-pivot-root：实际根、自身 bind、detach |
| read(mounts) / 63/63/63/0 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/proc_namespace.c#L101) | 展示可见挂载的 source、挂载点与选项 | sys_read → File → render_mounts → 目标任务 FsContext | 正确 | syscall-test-mountinfo：根 source、独立 tmpfs source、传播与 detach |
| pread64 / 67/67/67/17 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L770) | 按指定偏移读取文件，不改变打开文件偏移 | sys_pread64 → File::read_at → CachedFile::read_at；inode/page cache | 正确 | 早期 init 先 pread64，再在切根后 read 同一 FD 并比较完整内容 |
| write / 64/64/64/1 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/read_write.c#L748) | 写入已打开 inode，detach 不应替换该对象 | sys_write → fd File::write → CachedFile；inode/page cache | 无法确认 | 未执行 detach 后旧文件写入系统回归 |
| close / 57/57/57/3 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/open.c#L1492) | 移除 FD，独立映射继续有效，最后所有者释放内容 | sys_close → close_file_like → FD 移除/锁释放 → File/lease drop | 正确 | 早期 init 检查 close 成功，随后 mmap 仍可读取；组件计数证明最后引用回收 |
| mmap(文件私有映射) / 222/222/222/9 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/mmap.c#L613) | 文件映射在 detach 与 close 后继续有效 | sys_mmap → MappingOperation::new_cow/new_file → AddrSpace::map_with_permissions_publication | 正确 | host-initramfs-switch-root：MAP_PRIVATE 旧文件，close 后读取原内容 |
| munmap / 215/215/215/11 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/mm/mmap.c#L1076) | 删除指定映射并释放映射持有的页面 | sys_munmap → AddrSpace::unmap → VMA/mapping_slots/PageTable | 正确 | 早期 init 检查 munmap 成功；MemoryFs 页 pin 测试确认最后引用回收 |
| execve / 221/221/221/59 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L1924) | 从当前进程根解析程序，提交新地址空间 | sys_execve → FsContext::resolve → do_execve → AddrSpace 提交/CLOEXEC | 正确 | 切根后 exec 磁盘 /bin/sh，再 exec /sbin/init，进入 Starry root shell |
| execveat / 281/281/281/322 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/exec.c#L1934) | 按 dirfd、路径与 flags 选择执行对象 | sys_execveat → resolve_at/memfd Location → do_execve | 无法确认 | 未执行 execveat 与旧根脱离交互 |
| fsopen / 430/430/430/430 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fsopen.c#L120) | 创建文件系统上下文 FD，生命周期随持有者 | sys_fsopen → MountContext → FD；MountContextState | 无法确认 | 未执行新挂载 API 的生命周期系统回归 |
| fsconfig / 431/431/431/431 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/fsopen.c#L350) | 配置上下文并创建待挂载文件系统 | sys_fsconfig → MountContextState → MemoryFs/devpts filesystem | 无法确认 | 未执行新 API 创建 MemoryFs 的生命周期回归 |
| fsmount / 432/432/432/432 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4431) | 创建未连接挂载 FD，保留 filesystem 与挂载对象 | sys_fsmount → Mountpoint::new_root → Directory::new_detached_mount | 无法确认 | 未执行 detached mount FD 最后引用关闭回归 |
| move_mount / 429/429/429/429 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L4569) | 连接未挂载对象到目标 namespace 并更新可见性 | sys_move_mount → attach_detached → namespace 拓扑/通知 | 无法确认 | MS_MOVE 已运行，move_mount 系统调用未运行 |
| mount_setattr / 442/442/442/442 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/namespace.c#L5137) | 按目标挂载与标志修改挂载属性 | sys_mount_setattr → Mountpoint 属性 → namespace 通知 | 无法确认 | 未运行 AT_EMPTY_PATH 与属性变更系统回归 |

普通卸载、新挂载 API、chroot/getcwd、setns、clone3 与 openat2 等未执行入口采用“无法确认”，不把其他入口的成功作为其运行证据。Starry 的物理块节点原始 read/write 仍未实现；本次 Ext4 挂载通过共享 native handle 完成文件系统 I/O。

## 6. CI 回归修复

后续提交基于 `dev` `8c2160843220803f101c131a6a33ebb10d94e2e3`。PR 为 [#2567](https://github.com/rcore-os/tgoskits/pull/2567)；下面分别记录根因与已取得的证据，当前提交的完整 CI 状态仍需单独核对。

### 6.1 启动与资源交接

`pseudofs::mount_all()` 已持有 `FsContext`，devfs 构建再次锁定同一上下文导致 Starry 启动恐慌。修复通过参数传递已有根设备号；同一 x86_64 PID1 QEMU 用例修复前触发 `PI mutex waiter already owns the lock`，修复后通过，变基后再次通过。

`boot_payload::publish()` 在 MMU 开启前使用原子交换，AArch64 实体板卡停在独占指令重试循环。改用启动 CPU 独占的 load/store 发布；运行时一次性领取保留原子交换。现有 AArch64 机器码测试增强后修复前失败、修复后通过，Axvisor 的 `qemu-host-initramfs` 通过。OrangePi 和 Phytium 的诊断 CI 均停在 `initramfs properties decoded` 后，证明故障位置；实体板卡修复结果待当前 CI 复核。

ROC 固件的 `CONFIG_FIT_IMAGE_POST_PROCESS` 要求 ramdisk 带 `load` 属性。[ostool #207](https://github.com/drivercraft/ostool/pull/207) 已合并，依赖采用包含修复的 crates.io `ostool 0.30.3`，生成 `load = 0`，满足检查且保留 FIT 内归档地址。真实 FIT 编码回归修复前失败、修复后通过，10 个 FIT 测试和上游两条 CI 均通过；已核对发布版 FIT 源码与通过回归的实现相同。

最新 `dev` 的 virtual/real 机器人配置同步删除 `fs` 和 `image_location`，保留各自客户机根盘与板卡镜像路径；38 个 CI 规划器测试通过。Axvisor LoongArch 本地 Smoke 通过，包含宿主 NVMe 读写和客户机 Ext4 挂载；CI 曾在挂载步骤超时，仍需新提交复核。

板卡 rootfs 的外部镜像也必须在提交切根前完整可用。`validate_builtin()` 原先只检查外部路径为绝对路径，未检查文件存在或为空；现在直接在准备好的目标上下文解析并验证。`package_install_validates_external_assets_before_replacing_defaults` 经真实 MemoryFs 和 `install_directory()` 验证缺失、空镜像时保留旧包，补齐后发布新包并删除旧独有文件。同一 AArch64 axtest 修复前 85 通过、1 失败，修复后 86/86 通过；补充资源路径诊断后再次 86/86 通过。HTTP control plane 同轮 1/1 通过，包含切根后的 VM 删除、重建和启动。通用 Clippy 入口仍明确跳过 Axvisor，未将该跳过作为通过；代码由专用目标的实际编译与 QEMU 测试验证。

### 6.2 根设备状态

磁盘根使用 Linux 设备号后，`From<Kstat> for statx` 转换先正确解码设备号，末尾又覆盖主次号。LoongArch musl 用 `statx` 实现 `stat()`，因此根目录 `st_dev` 与块节点 `st_rdev` 不一致。删除重复赋值，并增强现有 `syscall-test-rdev-nvme`，直接调用系统调用比较主次号。LoongArch 同一用例修复前失败、修复后 1/1 通过；x86_64 同一用例也 1/1 通过。原始设备 read 的 `EIO` 判定继续通过。变基后的 `cargo xtask clippy --package starry-kernel --jobs 4` 共 80 项检查全部通过。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| statx(设备号) / LoongArch 291、x86_64 332 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L729) | 根文件系统设备号拆成主次号，与对应块节点的 rdev 一致 | sys_statx → resolve_at → ResolveAtResult::stat → Location::metadata → metadata_to_kstat → From<Kstat> for statx → write_statx；任务 FsContext 与挂载元数据 | 正确 | 两架构 `qemu/system/syscall-test-rdev-nvme` 均 1/1，通过原始 statx 与 libc stat 验证根设备关系；LoongArch 同一用例修复前失败 |
| statx(设备号) / AArch64 291、RISC-V 291 | [v7.1 `8cd9520d35a6`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/stat.c#L729) | 根文件系统设备号拆成主次号，与对应块节点的 rdev 一致 | sys_statx → resolve_at → ResolveAtResult::stat → Location::metadata → metadata_to_kstat → From<Kstat> for statx → write_statx；任务 FsContext 与挂载元数据 | 正确 | `e24f520188` CI 的两架构 `test-rdev-nvme` 均实际执行，原始 statx 与 libc stat 的设备号断言通过；各 10 通过、0 失败 |

### 6.3 板卡 CI 失败与修复

`e24f520188` 的 [CI 运行 37757626026](https://github.com/rcore-os/tgoskits/actions/runs/37757626026) 已终止：38 个作业成功，3 个作业失败。全部六个 SVM 用例通过，日志仅出现一次 `Compiling axvisor`，六次启动的内核 SHA-256 均为 `3b47d1e96d2eabed325464be6582e5fe1839d128a21bd7044e5333a33d693303`；每次启动使用独立宿主归档。这些结果只对应上述提交。

OrangePi 普通 Linux CI 没有设置 `AXVISOR_GUEST_ASSETS`，变量展开为空，生成 `/linux/orangepi-5-plus`，但板卡镜像实际位于 `/guest/linux/orangepi-5-plus`。CI 现在显式使用 `/guest`。相同 Smoke 用例本地通过：先切到 `/dev/mmcblk0p2` 并脱离旧根，再启动 Linux 客户机。日志为 `/tmp/pr2567-orange-local.log`。失败匹配补齐宿主 panic，防止启动失败退化为 shell 等待超时。

ROC 镜像位于 `/userdata/rootfs_overlay/guest`。旧实现直到提交切根后才挂载附加分区，资源安装的预先校验因此无法读取该路径。准备阶段现在挂载附加分区；提交阶段递归绑定已校验的完整挂载树，保持准备时的设备号、source 和只读属性。相同板卡用例在修复前明确报出缺失 `/userdata/rootfs_overlay/guest/linux/roc-rk3568-pc`，日志为 `/tmp/pr2567-roc-prepared-partitions-before.log`；修复后 1/1 通过（`/tmp/pr2567-roc-prepared-partitions-after.log`），日志确认 `/userdata` 在切根前挂载、旧根脱离、解包 ramfs 释放，Linux 客户机进入登录界面。

ROC 固件还会在交接时追加控制 DTB 中的旧 `ro`，覆盖测试命令行里的 `rw`。测试通过 U-Boot 命令只修改本次启动内存中的控制 DTB，使用发布版 ostool `0.30.3`；未写入固件或保存环境。具体命令与资源路径约定见 [启动调试参考](../../.agents/skills/arch-platform-porting/references/boot-debugging.md)。

资源安装错误现在保留缺失或空文件的具体路径。已有整包替换 axtest 修复前 85 通过、1 失败（`/tmp/pr2567-install-error-before.log`），修复后 86/86（`/tmp/pr2567-install-error-after.log`）；仍检查安装失败保留旧包、补齐资源后替换并删除旧独有文件。

ASUS 原作业在加载器串口尚未完成身份绑定时由服务端于 60 秒截止关闭，未进入 Axvisor。后续使用正常发布内核的板卡入口复测时，服务端状态已变为 `bound`，识别 `/dev/ttyUSB6`、启动代次和绑定 ID；未改动服务器或固件。用例 1/1 通过（`/tmp/pr2567-asus-published-boot.log`），宿主无盘直接使用 initramfs，Linux 客户机输出 `test pass!`；外部归档回收 31490048 字节，仍在使用的解包内存根按正常生命周期保留。

本轮 `cargo xtask clippy --package ax-fs-ng` 六项检查全部通过，CI 规划器 38 项测试通过；标准库增量检查使用已提交差异 `cargo xtask test --since e24f520188401f9351a13752c5ba80f3cb420eea`，选中的 16 个软件包全部通过（`/tmp/pr2567-std-prepared-partitions.log`），包括真实 VFS、根切换、MemoryFs 生命周期和目录替换回归，axbuild 为 428 项测试。

### 6.4 性能目录迁移变基

`e05d31a7cd213440a9b33b810a497e7d7c899a29` 的 [完整 CI 37768045833](https://github.com/rcore-os/tgoskits/actions/runs/37768045833) 最终 41 个作业全部通过。首次失败是 ROC 2 号板缺少 `/userdata/rootfs_overlay/guest/linux/roc-rk3568-pc`，原生 Linux 确认该目录为空；补齐同型号 1 号板使用的镜像并刷盘，重启后读回 36692480 字节、SHA-256 `383159149d60e4381a55f0d81501daef5ab9e6c9c2c0cd72c76a32d67a3a62b3` 一致。同一 Smoke 用例本地与 CI 均明确分配到 2 号板并通过，确认旧根脱离、748 字节解包 ramfs 释放及 Linux 客户机登录。临时文件已清理。

该提交的 SVM 六用例 6/6，日志只有一次 `Compiling axvisor`，六次内核 SHA-256 均为 `403e08351837f90683e98e148258f5880037be369bb512ef36974ee19a4c1008`，各归档哈希不同。Starry 四架构的 NVMe 设备号、普通 pivot_root 与命名空间隔离均实际执行；AArch64 的纯内存根、内核磁盘回退和用户态切根均通过。HTTP 控制面在切根及旧 ramfs 释放后删除、重建并启动 VM。

最终核对时 `dev` 合并了性能目录迁移 #2504，本分支再次变基到 `134cb196e25603097d373659062d9449d81f70a2`。原 `ivc-benchmark`、`task-switch-overhead` 与 `vcpu-perf` 改动迁移到 `benchmarks/axvisor`，保持归档输入、文件加载和文件系统 shell 语义；性能资源说明移到对应基准文档。变基后 CI 规划、路由与报告测试共 139 项全部通过，axbuild 定向 Clippy 1/1。`cargo xtask test --since aac162847b` 选中的 16 个软件包全部通过，axbuild 437 项测试；日志分别为 `/tmp/pr2567-ci-tests-rebase-134c-before.log`、`/tmp/pr2567-axbuild-clippy-rebase-134c.log` 和 `/tmp/pr2567-std-rebase-134c.log`。

同一 SVM 命令再次通过六个用例，同时覆盖重复参数与逗号列表。日志 `/tmp/pr2567-svm-rebase-134c.log` 只有一次 `Compiling axvisor`，六次启动的内核 SHA-256 均为 `61e011981a89c4f961badc4ff3f19b37c0b3dad79da93f172f407d94ca254325`，宿主归档分别对应各用例。以上本地结果覆盖性能目录迁移后的源码；随后纳入 `dev` 文档更新 `67268da829fa2c5b36eb5ef7795f84103d24b55e`，没有改变构建或运行代码。上述完整 CI 结果属于 `e05d31a7cd`，后续提交的 CI 需分别核对。

迁移后的板卡入口 `cargo xtask axvisor test board --board orangepi-5-plus-vcpu-perf --server 10.3.10.194 --port 2999` 在 `OrangePi-5-Plus-2` 通过，日志为 `/tmp/pr2567-vcpu-perf-rebase-134c.log`。构建配置未启用块驱动，客户机随宿主归档打包，归档 SHA-256 为 `1e63f456a6bb6843559081bf440d577a128cd8d655c69bb96e9ab9606707c2ff`；实际执行负载就绪、文件系统 shell 控制台连接和客户机性能判定，输出 `VCPU_PERF_PASS`。该运行验证迁移后的资源准备与无盘启动入口；其他两个性能板卡场景本轮未运行。

### 6.5 NVMe 更新变基

`da2f4afa7b371e770a94d94236b97d52bf444fd4` 的 [完整 CI 37781177893](https://github.com/rcore-os/tgoskits/actions/runs/37781177893) 最终 41/41 作业成功，增量 Clippy 为 204 个软件包、778 项检查。SVM 六用例只有一次内核编译，内核 SHA-256 均为 `403e08351837f90683e98e148258f5880037be369bb512ef36974ee19a4c1008`，六个归档哈希不同。四架构 Starry 的 NVMe 设备号、普通 pivot_root 与命名空间隔离均通过；AArch64 三种根策略、HTTP 切根后重建 VM，以及所有 Axvisor 功能板卡实际执行并通过。

首轮 ArceOS 的 RISC-V `task-ipi` 在普通批次通过，但绑到宿主 CPU 0 的单独入口 15 秒超时，导致另外六个作业取消。同一绑核命令本地 4/4 通过；补跑失败和取消项后，原始 SMP4、single-thread TCG 及绑核条件下的 IPI 用例通过，耗时 644 毫秒。首次超时缺少阶段与 PC 观测，根因尚未确认；未将重跑通过写成内核缺陷已修复。证据为 `/tmp/pr2567-arceos-riscv-da2-ci.log`、`/tmp/pr2567-riscv-task-ipi-local-before.log`、`/tmp/pr2567-riscv-task-ipi-local-repeat-{1,2,3}.log` 和 `/tmp/pr2567-riscv-ipi-da2-retry.log`。

随后 `dev` 合入 NVMe 格式与传输限制修复，分支变基到 `35c653e55ec6a42a982d9cf5ac0817a0bbd16426`；42 个提交经 `range-diff` 核对内容一致。格式化和差异检查通过，CI 规划测试 140/140；`cargo xtask clippy --package axbuild --package ax-fs-ng` 两个软件包共 7/7 检查通过；`cargo xtask test --since origin/dev` 在该基线选择的 17 个软件包全部通过，axbuild 为 437 项测试。日志分别为 `/tmp/pr2567-ci-tests-rebase-35c.log`、`/tmp/pr2567-clippy-rebase-35c.log` 和 `/tmp/pr2567-std-rebase-35c.log`。

最新基线的 SVM 同一六用例命令 6/6 通过，日志 `/tmp/pr2567-svm-rebase-35c.log` 只有一次 `Compiling axvisor`，六次内核 SHA-256 均为 `a323070329426ee1b6a4bd4bb95584d8fa8249fc85dff91e7b79ab3b69adb6e4`；Smoke 确认切到 NVMe 根、脱离旧根并继续验证宿主读写及客户机磁盘隔离。上述远程 CI 对应 `da2f4afa7b`，最新基线提交的 CI 需单独核对；性能板卡入口未在本次 NVMe 变基后重复运行。


### 6.6 审查问题修复

本轮从 `e14e185b87d7396ea573b452dfefdedb0d5a31df` 开始，恢复 `root.rs` 中两个独立磁盘保护测试及瞬时元数据读取的保护区域断言。它们分别经过真实 `scan_volumes()`、`collect_partitions()` 和 `axtest_protected_regions()`，证明已识别的整盘文件系统不能进入破坏性测试区域，以及探测失败后即使设备恢复也不能撤销未知布局保护；未修改生产探测逻辑。`cargo xtask test --since e14e185b87d7396ea573b452dfefdedb0d5a31df` 在测试恢复提交后选择 15 个软件包，全部通过，两项恢复测试均实际执行。`cargo xtask clippy --package ax-fs-ng --package axvisor` 中 ax-fs-ng 六项检查通过；工具明确跳过需要专用构建配置的 Axvisor，不把跳过视为通过。

`builtin::validate_builtin()` 只解析自带包内镜像，外部资源必须是绝对路径，但其存在性延迟到相应 VM 加载。`install_builtin()` 通过私有 `validate_builtin_assets()` 显式传入准备好的目标根，继续在发布和切根前拒绝缺失、空或非普通文件的外部资源。增强现有无盘启动回归，提供完整自带内核及不存在的外部设备树：原实现因 `/board/missing.dtb` 返回 NotFound 并触发恐慌，`cargo xtask ktest qemu` 返回非零；修复后同一入口 86/86 通过，并继续断言空自带内核被拒绝。已有外部资源安装回归仍验证缺失、空资源保留旧包以及补齐后的整包替换。

`.github/ci/checks/arceos.toml` 的 AArch64 应用与套件作业现在先通过 `cargo xtask image pack-initramfs` 生成归档、通过 `cargo xtask image pull --arch aarch64` 准备并校验 Alpine 根盘，再执行 `FEATURES=block-root-smoke cargo xtask arceos qemu -p arceos-helloworld --arch aarch64 --qemu-config apps/arceos/helloworld/qemu-host-initramfs-root-aarch64.toml`。同一命令本地成功，输出 `HOST_CMDLINE: root=/dev/nvme0n1 rw` 和 `HOST_DISK_ROOT_PASSED`。持续集成规划、路由和报告测试 140/140 通过。

### 6.7 挂载可见性依据

审查建议将不可达挂载点回退为 `/`，但 [Linux v6.12 `show_vfsmnt()` 和 `show_mountinfo()`](https://github.com/torvalds/linux/blob/v6.12/fs/proc_namespace.c#L94-L179) 都通过 [`seq_path_root()`](https://github.com/torvalds/linux/blob/v6.12/fs/seq_file.c#L480-L504) 对进程根之外的挂载返回 `SEQ_SKIP`。因此保留 `render_mounts()` 和 `render_mountinfo()` 跳过不可达条目的实现，只补充源码注释与依据。

现有 `qemu/system/test-mount-bind` 增强了真实系统边界的证明：子进程直接调用 `SYS_chroot` 改变根目录，再经 `SYS_openat`、`SYS_read` 读取两个 procfs 文件，要求恰有一个可达根和一个 procfs 挂载。临时恢复错误的 `/` 回退后，相同用例明确报出 14 个根挂载条目，分组运行器与最外层任务均失败；临时错误实现已恢复。

恢复正确实现后，`cargo xtask starry test qemu --arch aarch64 -c qemu/system/test-mount-bind` 为 1/1，通过 `TEST_MOUNT_BIND_PASSED` 和最外层 `all starry qemu tests passed`。其他体系结构的新挂载可见性判定本轮未本地执行；现有四架构 system 持续集成会运行该增强用例。此处仅证明根外挂载过滤，不将结果扩大为全部 procfs 或文件系统调用兼容性。

`cargo xtask clippy --package starry-kernel` 四架构共 80 项检查全部通过。格式化及差异检查通过。

本轮没有修改 StarryOS 系统调用或 procfs 的生产行为，下表记录新增系统回归直接证明的读取语义；其他读取入口不能由该测试推定兼容。

| 系统调用/编号 | Linux 基准与稳定链接 | Linux 可观察语义 | StarryOS 入口与调用链 | 实现结论 | 测试与证据 |
| --- | --- | --- | --- | --- | --- |
| read/AArch64:63 | [Linux v6.12](https://github.com/torvalds/linux/blob/v6.12/fs/proc_namespace.c#L94-L179)、[`seq_path_root`](https://github.com/torvalds/linux/blob/v6.12/fs/seq_file.c#L480-L504) | 读取 mounts、mountinfo 时隐藏进程根之外的挂载；可达挂载只按根内路径显示 | sys_read → get_file_like → MountTableFile::read → File::read → VFS File::read → SimpleFile::read_at → procfs 生成器 → render_mountinfo/render_mounts → FsContext::root_dir；根和挂载命名空间由任务的 FS_CONTEXT 引用 | 正确 | 增强 test-mount-bind；错误回退时 14 个根条目并失败，恢复后 AArch64 同一 QEMU 入口 1/1 通过 |
| read/x86_64:0、RISC-V:63、LoongArch:63 | 同上固定 Linux 源码 | 同上根内可见性要求 | 同一共享实现与 FS_CONTEXT 所有权 | 无法确认 | 本轮未在这些架构执行新增判定，需当前提交的对应 system 持续集成结果 |
