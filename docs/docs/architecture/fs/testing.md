---
sidebar_position: 11
sidebar_label: "测试与验证"
---

# 文件系统测试与验证

文件系统验证按“纯算法 → VFS 状态机 → 格式镜像 → OS API → StarryOS Linux ABI → 真实块 IRQ”分层。低层测试给出确定性失败定位，高层测试确认 namespace、syscall、rootfs 和驱动组合；任何一层都不能替代其他层。

## 1. Host crate 测试

### 1.1 虚拟文件系统

`fs/axfs-ng-vfs` 的单元测试覆盖路径、dentry、mount topology 和 propagation；`tests/std.rs` 在 `host-test` feature 下从 crate 外验证 public API。

重点契约包括：

| 测试域 | 必须保持的行为 |
| --- | --- |
| path | 双向 component、file name、非法/超长 entry name |
| dentry cache | mutation generation、rename 后 parent 引用、hard-link user data |
| mount callback | filesystem callback 在 topology guard 外执行 |
| namespace clone | source/flags 保留、mount identity 独立、relation 重建 |
| bind/pivot | subtree 范围、absolute path 重定位、旧根 reparent |
| propagation | peer 对称、slave transitive delivery、visited 去环 |
| unmount | normal busy、lazy child-first、传播多目标原子性、stale plan revalidation |

定向运行：

```bash
cargo test -p axfs-ng-vfs --features host-test
```

### 1.2 文件访问层

`ax-fs-ng` 的 host tests 使用 test page provider 和 mock `rdif-block` controller/queue，不需要 QEMU 即可验证：

- page allocation/deallocation 和 virtual-to-physical capability；
- readahead 4→8→…→32 页及随机读重置；
- EOF 尾页清零、cached read/write、dirty generation；
- writeback callback 锁外调用；
- resize 成功与 backing rollback 失败；
- reclaim 在 registry spin lock 外取得 file lock；
- GPT、MBR primary、EBR logical、raw fallback 和越界拒绝；
- root selector、只读 transient mountpoint、逆序 shutdown；
- block runtime publication、batching、flush barrier、teardown 和资源 rollback。

常用命令：

```bash
cargo test -p ax-fs-ng --features host-test,vfs,ext4,fat
cargo test -p ax-fs-ng --test root_selector --features host-test,vfs,ext4,fat
```

具体 feature 组合应以 workspace/CI 当前定义为准；若完整组合因外部格式库 host feature 不兼容，应分别运行最小 ext4、FAT 和 VFS 组合，不能直接跳过对应域。

## 2. 系统行为

格式测试、ArceOS 系统测试和 StarryOS ABI 测试逐步扩大观察范围。`rsext4` 验证磁盘格式，ArceOS 验证公共 API 与真实 rootfs，StarryOS 则验证 Linux fd、namespace 和 syscall 语义。

### 2.1 Ext4 格式

`fs/rsext4/tests/` 包含 public API、文件/目录操作、metadata 生命周期、checksum、cache coherence、Linux image repro 和错误处理。它们直接对确定性 ext4 image 操作，适合定位 VFS adapter 之下的格式错误。

| 测试文件 | 覆盖点 |
| --- | --- |
| `file_operations.rs` | create/read/write/truncate/unlink |
| `directory_operations.rs` | lookup、mkdir、rename、目录项 |
| `metadata_lifecycle.rs` | inode/link/时间和释放 |
| `crc_integrity.rs` | superblock、bitmap、inode、journal checksum |
| `cache_coherence_repro.rs` | metadata/data cache 一致性回归 |
| `linux_image_repro.rs` | Linux 工具生成 image 的兼容场景 |
| `error_handling.rs` | I/O 和损坏 metadata 的 typed error |

这些测试需要观察 image 操作后的 metadata 或重新 mount 结果，不能只断言内存 API 返回成功。持久化行为还应保持与 Linux `e2fsck` 和读取侧的格式兼容。

```bash
cargo test -p rsext4
```

### 2.2 ArceOS 系统

ArceOS 文件系统用例位于 `test-suit/arceos/rust/src/fs/`，通过真实 rootfs 和 `ax-std`/API 入口覆盖 create、read/write、directory 和 metadata。使用 xtask 保持 feature、架构、磁盘参数和 success regex 一致：

```bash
cargo xtask arceos test qemu --arch x86_64 --test-group rust
```

如果当前 xtask 的具体子命令或配置名变化，应先查询 `cargo xtask --help` 和 `test-suit/arceos` 配置，不要用 raw Cargo/QEMU 猜等价参数。

### 2.3 StarryOS ABI

StarryOS system 分组从用户态调用 Linux syscall，覆盖公共 VFS 无法观察的 fd、errno、credentials、namespace 和结构体布局。文件系统相关场景至少包括：

| 测试类别 | 代表位置/名称 | 主要契约 |
| --- | --- | --- |
| mount namespace | `test-unshare-fs` | clone/unshare 后 topology visibility |
| sync | `test-syncfs`、`syscall-test-syncfs` | page cache + filesystem flush 和错误传播 |
| overlay | `syscall-test-overlayfs` | 多层 lookup/copy-up/mount 行为 |
| mount/umount | system mount cases | busy、flags、bind/move/lazy detach、mountinfo |
| path/open | openat2、symlink、chroot/pivot cases | no-follow/beneath/root boundary |
| file I/O | read/write/truncate/mmap/fsync cases | cache、EOF、MAP_SHARED 一致性 |
| metadata | stat/xattr/link/rename cases | Linux-visible inode、mode、link count、errno |

基础运行入口：

```bash
cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
```

mount、page cache 和 block runtime 都涉及 SMP，相关变更不能只跑单核。至少补充一个多 vCPU 配置；涉及架构/设备差异时按 CI 支持覆盖 x86_64、riscv64、aarch64 或 loongarch64 对应目标。

## 3. 运行时验证

块设备运行时和文档构建分别验证设备状态机与架构说明的可执行性。前者使用 deterministic mock 固定 request/IRQ owner，后者由 Docusaurus 检查 Markdown、链接和 Mermaid。

### 3.1 块设备运行时

块 runtime 的 deterministic mock tests 位于：

```text
fs/ax-fs-ng/src/block/runtime/hctx/tests/
fs/ax-fs-ng/src/block/runtime/lifecycle/tests/
fs/ax-fs-ng/src/block/runtime/irq.rs tests
```

应按变更类型选择：

| 变更 | 必测事实 |
| --- | --- |
| admission/channel | full/closed/NOWAIT 返回完整 request owner |
| batching | queue limit、commit 次数、完成顺序 |
| flush | 所有 data drain 后提交，后续 data 被阻塞，错误后 gate 恢复 |
| publication | READY 前 caller 不可见，queue/IRQ 失败不部分发布 |
| IRQ | spurious、empty ack、shared fan-out、deferred control/rearm |
| teardown | disable+synchronize、worker join、DMA/request 恰好回收一次 |
| SMP online | CPU channel mapping 和 queue epoch 更新 |

真实硬件或 QEMU 测试负责确认 driver endpoint 的 mask/ack/rearm 和 DMA coherency，mock 不能证明这些硬件事实。板端写测前后应遵守项目的 rootfs fsck/boot 检查流程。

### 3.2 文档构建

本目录使用 Docusaurus/MDX 和 Mermaid。修改后至少执行：

```bash
cd docs
npm run build
```

构建会发现 frontmatter、相对链接、Mermaid 语法和 MDX 特殊字符错误。还应检查：

```bash
rg -n '\]\([^)]*\.md\)' docs/docs/architecture/fs
rg -n 'TODO|FIXME|旧路径|ax-fs/' docs/docs/architecture/fs
```

文档中的源码路径应对应当前文件，不固定易漂移的行号。接口名、常量和 feature 必须从代码核对；设计文档中已经删除的历史模型不能写成当前实现。

## 4. 验证范围

不同文件系统改动需要覆盖不同最低层测试和系统场景。验证矩阵用于保持行为层级完整，质量检查则保证同一变更中的 Rust 与 Markdown 都符合仓库工具链要求。

### 4.1 分层矩阵

下表把改动域映射到最低 host 验证和系统验证。host test 用于确定性定位，系统测试用于观察 namespace、rootfs、ABI 和真实设备组合。

| 改动域 | 最低 host 验证 | 系统验证 |
| --- | --- | --- |
| path/dentry | `axfs-ng-vfs` tests | Starry path/open/rename cases |
| mount/namespace | VFS mount tests | Starry unshare/mount/umount/mountinfo |
| file handle/cache | `ax-fs-ng` cache tests | ArceOS fs + Starry mmap/fsync/truncate |
| ext4 | `rsext4` + ax-fs-ng ext4 adapter | ext4 rootfs boot/write/sync；必要时 e2fsck |
| FAT | ax-fs-ng FAT/disk tests | FAT root或附加卷读写/flush |
| volume/root | volume + `root_selector` | 多盘/分区配置启动 |
| block runtime | hctx/lifecycle/IRQ mock tests | 对应 QEMU/board block driver，多核 I/O + sync |
| OS capability glue | targeted clippy/build | ArceOS 启动 + 一个上层系统 |

### 4.2 质量检查

本目录本身仅是 Markdown；纯文档修改不要求 Rust clippy 或 rustfmt。若同一变更修改 Rust 逻辑，按项目要求执行：

```bash
cargo fmt
cargo xtask clippy --package axfs-ng-vfs
cargo xtask clippy --package ax-fs-ng
```

bug 修复必须先加入能在旧实现上确定失败的回归测试，记录失败，再实现修复并验证同一测试通过。放宽 success regex、跳过同步或降低 image 检查不能替代根因测试。
