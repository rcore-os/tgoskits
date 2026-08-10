# rsext4 Linux 7.1 语义对齐与 OS 边界设计

## 1. 基线与目标

- TGOSKits 基线：`6e27704c41528d2e700d2993915c6dc22b9cca34`。
- Linux 语义基线：`v7.1`，commit
  `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。
- `rsext4` 保持 `#![no_std] + alloc`，独立实现 ext4 磁盘语义，不复制
  GPL-2.0 Linux 源码。
- 完成声明必须同时满足本文件的语义映射、测试矩阵和性能门槛。

## 2. 所有权与依赖方向

```text
Starry syscall / namespace / credential / errno
                       |
                       v
ax-fs-ng VFS / page cache / sleepable serialization
                       |
                       v
rsext4 core: format / mapping / allocation / JBD2 / recovery
                       |
                       v
rsext4::io::BlockIo capability
                       |
                       v
ax-fs-ng block adapter / rdif-block / IRQ completion
```

`rsext4` 只拥有 ext4 的磁盘事实和一致性状态。它不得读取当前 task、
credential、namespace 或全局时钟，也不得选择 spin、IRQ-safe 或 sleepable
lock。调用方通过独占 `&mut Ext4` 保证同一挂载实例的串行化。

OS 或 runtime 能力通过以下小接口提供：

- `BlockIo`：sector geometry、读写、只读状态和 durability capabilities；
- `Clock`：创建或修改 inode 时取得时间；
- `EntropySource`：mkfs UUID、salt 及需要随机性的格式字段；
- `CryptoProvider` / `KeyProvider`：fscrypt 与 fsverity 算法和密钥；
- `Observer`：接收 typed lifecycle/integrity/journal events。

路径解析、file descriptor offset、page cache、权限/capability 检查、Linux
errno 和动态设备发现均属于上层 glue。Core 只接收 inode、原始目录项名称、
offset、长度和已经校验过的 mutation context。

## 3. Linux 7.1 语义映射

映射状态只有四类：

- `core`：必须由 `rsext4` 独立实现；
- `capability`：core 编排语义，但外部动作由 capability trait 完成；
- `glue`：属于 VFS、syscall、credential、page cache 或设备 runtime；
- `not-applicable`：Linux 依赖当前项目不存在的硬件能力，必须明确拒绝。

| Linux 范围 | 关键入口/不变量 | Rust owner | 状态 | 验证 |
| --- | --- | --- | --- | --- |
| `fs/ext4/super.c`, `ext4.h` | feature negotiation、mount/remount、错误策略、geometry | `rsext4::ext4` / `superblock` | core | `feature_gate`, Linux image matrix |
| `fs/ext4/inode.c`, `indirect.c`, `inline.c` | inode lifecycle、map blocks、writeback modes、truncate | inode/mapping services | core | map/truncate/crash differential |
| `fs/ext4/extents.c`, `extents_status.c` | checked tree、unwritten extent、split/merge、status cache | extent service | core | codec/property/differential tests |
| `fs/ext4/mballoc.c`, `balloc.c`, `ialloc.c` | multiblock allocation、preallocation、rollback、quota charge | allocator service | core | ENOSPC/fault injection |
| `fs/ext4/namei.c`, `dir.c`, `hash.c` | linear/HTree、link count、atomic rename、casefold | directory service | core | Linux syscall trace + e2fsck |
| `fs/ext4/orphan.c`, `mmp.c` | persistent orphan cleanup、multi-mount exclusion | lifecycle service | core | power-cut recovery |
| `fs/ext4/xattr.c`, `acl.c`, `quota.c` | EA inode/block、ACL encoding、quota persistence | metadata services | core | xattr/ACL/quota differential |
| `fs/ext4/crypto.c`, `verity.c` | on-disk policy、Merkle metadata、file data transformation | core + crypto/key traits | capability | Linux image and negative-key tests |
| `fs/ext4/resize.c`, `ioctl.c`, `fsmap.c`, `move_extent.c` | resize and administrative operations | core typed operations | core | ioctl/fsmap/resize differential |
| `fs/ext4/file.c`, VFS operation tables | permission/open/fd/page-cache/direct-I/O dispatch | ax-fs-ng / Starry | glue | common syscall tests |
| ext4 DAX paths | persistent-memory direct mapping | none | not-applicable | mount option returns unsupported |
| `fs/jbd2/transaction.c`, `commit.c` | handles/credits、ordered data、commit record ordering | journal transaction owner | core | phase fault injection |
| `fs/jbd2/recovery.c`, `revoke.c`, `checkpoint.c` | scan/revoke/replay、tail/checkpoint reclamation | journal recovery owner | core | Linux-created journal replay |

`scripts/test/check_rsext4_linux_map.py` 将把该表扩展为固定 commit 的源码区间
清单，并验证 `fs/ext4` 与 `fs/jbd2` 没有未分类行。该检查在映射清单完整前保持
Draft 红项，不得通过跳过源码文件来转绿。

## 4. 公共 API 迁移

v0.8 删除 `Ext4FileSystem` 公共字段、公开 `Jbd2Dev`、Linux `Errno` core
类型、错误拼写和 path/fd helpers。替代接口为拥有私有状态的 `Ext4<D, S>`、
typed IDs、domain errors，以及 `io`/`runtime` capability traits。

同一集成 PR 内迁移 `ax-fs-ng` 与所有测试，不维护长期双 API。短期编译迁移
helper 只能存在于未提交的本地步骤，不得进入最终 diff。

## 5. 同步与持久化

- Core cache 使用显式可变借用，不包含任何 mutex。
- ax-fs-ng 的 sleepable mutex 拥有整个 `Ext4`，不得持 spin/no-preempt guard
  进入 block I/O。
- page cache 归 ax-fs-ng；ext4 block/metadata cache 归 rsext4。
- 所有 metadata mutation 必须属于一个 journal transaction；journal enabled
  但未初始化时必须失败，禁止直接 home write。
- clean superblock、journal tail 和 transaction completion 只能在相应 flush 或
  barrier 成功后发布。

## 6. Draft 红测台账

| ID | 基线失败 | 最终要求 | Owner phase | 当前状态 |
| --- | --- | --- | --- | --- |
| `boundary-no-os-deps` | `ax-kspin`、`log` direct dependencies | boundary script passes | portable core skeleton | 绿：`RSEXT4_BOUNDARY_PASSED` |
| `domain-error-no-errno` | core 公开并按 Linux `Errno` 分支 | typed domain error，errno 仅由 adapter 映射 | portable core skeleton | 绿：core 已无 `Errno`；`ax-fs-ng` 集中映射 |
| `feature-gate-strict` | unknown incompat、ENCRYPT、RW QUOTA 均被接受 | incompat 拒绝；未实现 RO_COMPAT 只允许 RO | codec/feature negotiation | 绿：四项确定性单测完成红绿验证 |
| `linux-default-rocompat-rw` | Linux mkfs 默认设置 `HUGE_FILE`、`DIR_NLINK` | 完整读写语义后纳入 writable mask | inode/namespace lifecycle | 红：4 个 `linux_image_repro` 测试由严格 gate 拒绝 `0x28` |
| `linux-map-complete` | 仅有 subsystem mapping，缺少逐区间清单 | every source line classified | design/traceability | 红：尚未加入逐区间 manifest |
| `journal-no-direct-fallback` | uninitialized JBD2 performs home write | typed journal-aborted error | JBD2 rewrite | 绿：确定性红绿回归已覆盖 write/umount |
| `extent-empty-index` | crafted empty internal node can panic | corruption error, no mutation | mapping rewrite | 红：待 mapping rewrite |
| `io-failure-no-panic` | mount/commit paths contain `expect` | all errors propagated | codec/JBD2 rewrite | 进行中：mount/JBD2 关键路径已移除，剩余生产路径待审计 |
| `legacy-indirect-13-blocks` | non-extent path is unsupported | Linux-compatible mapping | mapping rewrite | 红：仅完成 12 个 direct block 编码 |

Draft 期间这些测试可以保持失败，但测试本身不得 `ignore`、弱化断言或伪造成功。
PR 转 Ready 前本表必须为空。

## 7. 性能门槛

第一阶段 host harness 的工作负载和输出格式冻结。相同机器、toolchain、CPU
affinity、镜像与 workload 下，预热 3 次、测量至少 10 次；现有语义的 median
吞吐/IOPS 回退不得超过 5%，p95 latency 回退不得超过 10%。新增语义报告相对
Linux 7.1 的代价，不套用不存在的 dev 对照。

### 7.1 dev 顺序 I/O 基线

采集时间：2026-08-10；代码基线为本文第 1 节的 TGOSKits commit，加上只包含
本 benchmark harness 的工作树。环境为：

- `rustc 1.99.0-nightly (da80ed070 2026-07-14)`；
- Linux `6.8.0-124-generic`；
- Intel Core i7-10700，8 cores / 16 threads；
- 128 MiB memory-backed image、4 KiB filesystem block、journal enabled；
- 20 MiB deterministic sequential payload、3 warmups、10 measured runs。

```text
RSEXT4_BENCH_RESULT run=0 write_ns=6572727 read_ns=6911325 sync_ns=26922
RSEXT4_BENCH_RESULT run=1 write_ns=6371145 read_ns=6920173 sync_ns=22973
RSEXT4_BENCH_RESULT run=2 write_ns=7004468 read_ns=8344475 sync_ns=36468
RSEXT4_BENCH_RESULT run=3 write_ns=7078821 read_ns=8481300 sync_ns=38644
RSEXT4_BENCH_RESULT run=4 write_ns=7335398 read_ns=8159319 sync_ns=29738
RSEXT4_BENCH_RESULT run=5 write_ns=6952939 read_ns=7251429 sync_ns=25647
RSEXT4_BENCH_RESULT run=6 write_ns=6558463 read_ns=8046839 sync_ns=28536
RSEXT4_BENCH_RESULT run=7 write_ns=7285547 read_ns=7218673 sync_ns=25823
RSEXT4_BENCH_RESULT run=8 write_ns=6786754 read_ns=6994647 sync_ns=23710
RSEXT4_BENCH_RESULT run=9 write_ns=6826987 read_ns=7100408 sync_ns=24973
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6826987 write_p95_ns=7335398 read_median_ns=7218673 read_p95_ns=8481300 sync_median_ns=25823 sync_p95_ns=38644
```

该 harness 测的是 rsext4 core 与 memory device，不包含 QEMU、真实控制器或
Starry page cache；因此只用于 core 重构硬门槛。QEMU 与三架构数据单独报告。

### 7.2 portable capability boundary 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点已经移除
core 内部 lock 与全局 logger，并为 mount/recovery/integrity/repair/unmount
接入 typed `Observer` event：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6220509 write_p95_ns=6255444 read_median_ns=6015336 read_p95_ns=6050556 sync_median_ns=20660 sync_p95_ns=23716
```

相对 dev 基线，write median 改善约 8.9%，read median 改善约 16.7%，sync
median 改善约 20.0%；三个 p95 均未回退，满足当前 host 硬门槛。
