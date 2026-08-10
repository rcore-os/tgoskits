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
| `device-sector-map` | filesystem block number 被直接作为 device sector，512-byte 设备只读一个 sector | typed `SectorId` + private filesystem-block mapper | portable I/O core | 绿：512-byte sector 聚合与 byte-offset superblock 红绿回归通过 |
| `filesystem-block-dynamic` | core 算法仍大量引用 4 KiB 常量 | 1/2/4 KiB geometry、cache、JBD2 与 codec 全部按 mount 派生 | codec/geometry | 绿：Linux 与 rsext4 各自创建的 1/2/4 KiB 镜像均在 512-byte sector 上完成跨块写入、rename、remount 与 `e2fsck -fn`；cache、extent 与 JBD2 buffer 均按 mount geometry 分配 |
| `linux-default-rocompat-rw` | Linux mkfs 默认设置 `HUGE_FILE`、`DIR_NLINK` | 完整读写语义后纳入 writable mask | inode/namespace lifecycle | 绿：`HUGE_FILE` 统一按 Linux 的 32-bit sector、48-bit sector、filesystem-block 三级 codec 读写，所有 block accounting mutation 使用 checked 状态转换；`DIR_NLINK` 覆盖 65000 到 sentinel 1、连续 mutation 保持 sentinel、无 feature 时分配前返回 `EMLINK`；Linux 默认 feature 的 1/2/4 KiB round-trip、extent/JBD2 replay 与 `e2fsck -fn` 全部通过 |
| `linux-map-complete` | 仅有 subsystem mapping，缺少逐区间清单 | every source line classified | design/traceability | 红：尚未加入逐区间 manifest |
| `journal-no-direct-fallback` | uninitialized JBD2 performs home write | typed journal-aborted error | JBD2 rewrite | 绿：确定性红绿回归已覆盖 write/umount |
| `jbd2-csum-v3-write-replay` | writer emits legacy tags while accepted CSUM_V3/64BIT journals require tag3/high block numbers | Linux-compatible descriptor tags and checksum followed by self/Linux replay | JBD2 rewrite | 绿：writer 生成 tag3/64-bit block number、escaped payload CRC32C、descriptor/commit checksum；replay 在任何 home write 前校验 commit 与全部非 revoke payload，并校验 descriptor/revoke tail；Linux `debugfs` 多块事务与逐边界损坏测试通过；mkfs 将 ext4 `metadata_csum`/`64bit` 映射为对应 JBD2 feature |
| `extent-checked-codec` | raw extent nodes are sorted after parsing and malformed roots/children can be treated as holes | checked structural validation preserves on-disk order and propagates corruption | mapping rewrite | 绿：root/child codec 检查 magic、depth、capacity、非空 index、logical/physical overflow 与 leaf/index ordering；`EXT4_EXTENTS_FL` 是唯一格式判据，坏 magic 不再降级为 legacy/hole；读取、查找、插入、删除、HTree 和 block resolver 均传播 typed error，不再排序或吞错；hard-link parent corruption 完成确定性红绿验证 |
| `extent-empty-index` | crafted empty or malformed internal child can panic | corruption error, no mutation | mapping rewrite | 绿：root 与 external child 在 mutation 前统一 checked decode；空 index、坏 child 与超过 inline root 容量均返回 corruption，确定性测试验证 inode 不被截断或修改 |
| `extent-block-checksum` | extent block lookup lacks the inode number required by metadata checksum and assumes the checksum tail is always at the end of the block | every resolver carries typed inode identity and verifies the Linux `eh_max`-derived checksum tail | mapping rewrite | 绿：resolver/HTree/mount/adapter 调用链显式传递 `InodeNumber`；external node 读写按 inode generation/number 校验 CRC32C，2 KiB `eh_max` tail offset 与损坏测试通过 |
| `extent-system-zone-validity` | physical extents are checked only against filesystem/device bounds | reject overlap with ext4 system metadata zones, with Linux's owning-inode exception | mapping rewrite | 绿：mount/replay 后完整构建并一次发布 immutable zone index，覆盖 per-group super/GDT/reserved GDT、bitmap、inode table 与 internal journal blocks；普通 inode 指向 block bitmap 的确定性红绿测试完成，journal inode owner exception 单测通过；first-data、溢出和 filesystem/device 上界继续共同生效 |
| `mount-option-block-validity` | core did not protect system metadata blocks | default `block_validity` plus Linux-compatible `noblock_validity` mount/remount lifecycle | mount/remount options | 红：默认保护已由 `extent-system-zone-validity` 完成；显式 `noblock_validity` 与 remount 时建立/释放索引尚未实现 |
| `extent-mutation-rollback` | split/remove 的 metadata write 或 bitmap I/O 失败可留下泄漏、部分释放或不可达节点 | plan/validate/journal persist，任一失败保持旧树与 bitmap/i_blocks 一致 | mapping rewrite | 红：HUGE_FILE checked accounting 已在分配/释放前预检；跨多个 metadata/bitmap I/O 的完整事务回滚仍待 extent 整体重写 |
| `mkdir-mutation-rollback` | child inode 初始化后，父 link/group accounting 或目录项插入失败可留下孤儿 inode、泄漏块或部分发布的计数 | mkdir 的 inode、block、父目录项、父 link count 与 group stats 属于同一可回滚 transaction | namespace/JBD2 rewrite | 红：link 上限已在分配前预检；目录项插入及其后续 I/O 失败的整体回滚仍待 namespace transaction 重写 |
| `rename-mutation-rollback` | 跨父目录 rename 在新项、旧项、父 link count 或 `..` 更新任一步失败时可留下部分状态 | rename 的全部目录项、link count、`..` 与被替换 inode 更新崩溃原子且可回滚 | namespace/JBD2 rewrite | 红：link count 算术已在发布前预检；多对象 mutation 仍待 journal transaction 整体重写 |
| `io-failure-no-panic` | mount/commit paths contain `expect` | all errors propagated | codec/JBD2 rewrite | 进行中：mount/JBD2 与 extent root/child traversal 已移除 panic/静默失败，剩余生产路径待审计 |
| `legacy-indirect-13-blocks` | non-extent path is unsupported | Linux-compatible mapping | mapping rewrite | 进行中：checked read 与 allocate-before-publish write 已覆盖 direct/single/double/triple、hole、整块 pointer validity、system zone、cycle、data+metadata `i_blocks` 与运行时失败反向 rollback；跨 direct/single 的 Linux image 已通过 umount、e2fsck、remount/read、再次 e2fsck。recursive truncate/free、punch 与 crash-atomic journal transaction 仍为红项；free 继续在任何 inode/link mutation 前返回 typed unsupported |

Draft 期间这些测试可以保持失败，但测试本身不得 `ignore`、弱化断言或伪造成功。
PR 转 Ready 前本表必须为空。

当前 core 尚未实现 HTree，因此会在插入时清除 `EXT4_INDEX_FL`。在进入
`DIR_NLINK` sentinel 1 后，core 暂时把这个持久值作为“目录已经进入不精确链接
计数模式”的事实来源，即使 flag 已被降级也继续保持 1；否则下一次 mkdir 会把
计数错误恢复为 2。HTree 阶段必须移除这一过渡差异，并按 Linux `is_dx()` 与
`ext4_inc_count()` 的边界完成同镜像 differential。

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

### 7.3 dynamic filesystem block geometry 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点将
filesystem block 与 device sector 分离，并让 mount、cache、extent、mkfs 与
JBD2 buffer 从 superblock 派生 1/2/4 KiB block geometry：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6292630 write_p95_ns=6763865 read_median_ns=6329181 read_p95_ns=7140698 sync_median_ns=20449 sync_p95_ns=25356
```

相对 dev 基线，write median 改善约 7.8%，read median 改善约 12.3%，sync
median 改善约 20.8%；write/read/sync p95 分别改善约 7.8%、15.8%、34.4%，
满足当前 host 硬门槛。

### 7.4 JBD2 CSUM_V3 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点让默认
`metadata_csum`/`64bit` 文件系统创建对应 JBD2 feature，并在 commit/replay
路径计算与验证 tag、payload、descriptor、revoke 和 commit CRC32C。软件
CRC32C fallback 同时改为经过逐长度对照验证的 slicing-by-8：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6289508 write_p95_ns=6401087 read_median_ns=6389148 read_p95_ns=6676132 sync_median_ns=28617 sync_p95_ns=33200
```

相对 dev 基线，write/read median 分别改善约 7.9% 和 11.5%，对应 p95 分别
改善约 12.7% 和 21.3%；sync p95 改善约 14.1%。sync median 因新增完整 journal
checksum 从 25.8 µs 增至 28.6 µs，但 latency 硬门槛按 p95 判定，未发生回退。

### 7.5 `HUGE_FILE` 与 `DIR_NLINK` 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点将 Linux
默认 RO_COMPAT `0x28` 纳入可写 feature mask，所有 inode block accounting
改为 checked codec，并将目录链接数更新统一为 Linux sentinel 状态转换：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6215992 write_p95_ns=6353502 read_median_ns=6441608 read_p95_ns=6894968 sync_median_ns=33375 sync_p95_ns=35413
```

相对 dev 基线，write/read median 分别改善约 9.0% 和 10.8%，对应 p95 分别
改善约 13.4% 和 18.7%；sync p95 改善约 8.4%，满足当前 host 硬门槛。

### 7.6 extent checked codec 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点在每次
extent root/child traversal 中校验结构、depth、parent key、物理范围与 external
node checksum，并让所有 resolver/HTree/adapter caller 保留 typed error：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6269685 write_p95_ns=7043691 read_median_ns=6206970 read_p95_ns=7098309 sync_median_ns=28674 sync_p95_ns=38321
```

相对 dev 基线，write/read median 分别改善约 8.2% 和 14.0%，对应 p95 分别
改善约 4.0% 和 16.3%；sync p95 改善约 0.8%。checked traversal 没有越过
现有 workload 的 host 性能门槛。

### 7.7 system metadata zone 检查点

采集时间：2026-08-10；固定 CPU 2，其他参数与 7.1 相同。该检查点按 Linux
`s_system_blks` 语义建立 immutable metadata-zone index，保护 super/GDT、
bitmap、inode table 与 internal journal blocks，并在 replay 后重新校验 journal
inode mapping；mkfs 同时保证 partial final group 的 inode table 与 free-block
accounting 保持一致：

```text
RSEXT4_BENCH_SUMMARY workload=sequential write_median_ns=6300969 write_p95_ns=6432179 read_median_ns=6148506 read_p95_ns=6252651 sync_median_ns=28288 sync_p95_ns=30018
```

相对 dev 基线，write/read median 分别改善约 7.7% 和 14.8%，对应 p95 分别
改善约 12.3% 和 26.3%；sync p95 改善约 22.3%。sync median 增加约 9.5%，
但 p95 latency 未回退，全部现有 workload 仍满足 host 性能门槛。

### 7.8 legacy indirect checked read 检查点

采集时间：2026-08-10；被测实现 commit 为
`2ebb481ceefa1d9669d5cd890b81e948ff977f4b`，固定 CPU 2、x86_64 memory
backend、4 KiB block、`metadata_csum+64bit+journal`，workload 和计时边界与
7.1 相同。该检查点加入 direct/single/double/triple checked decoder，并按 Linux
`ext4_check_indirect_blockref()` 校验读入块的全部非零 pointer；extent 热路径不
经过新增 decoder。

10-run 探测曾出现一个 81.253 us sync 离群值；同配置立即复测的 sync p95 为
37.816 us。为避免 10 个样本时 p95 退化为最大值，正式检查点扩展为 3 次预热加
20 次测量，并保留全部原始样本于
`book/design/data/rsext4-perf/2026-08-10-legacy-indirect.csv`。harness marker 同时补齐
commit、arch、backend 与 feature 字段，但没有改变 workload 或计时区间：

```text
RSEXT4_BENCH_SUMMARY commit=2ebb481ceefa1d9669d5cd890b81e948ff977f4b arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6258062 write_p95_ns=6887763 read_median_ns=6088048 read_p95_ns=7145268 sync_median_ns=28479 sync_p95_ns=34506
```

相对 dev 基线，write/read median 分别改善约 8.3% 和 15.7%，对应 p95 分别
改善约 6.1% 和 15.8%；sync p95 改善约 10.7%。sync median 增加约 10.3%，
但 p95 latency 未回退，全部现有 workload 满足 host 性能门槛。

### 7.9 legacy indirect allocation 检查点

采集时间：2026-08-10；被测实现 commit 为
`dad8b5da29364b0a5d1b85c9aaec912645c72b42`，环境与 7.8 相同。该检查点为
legacy inode 增加 direct/single/double/triple sparse branch 分配、data 与 metadata
block 的 checked `i_blocks` accounting，以及 publish/finalize 失败的反向恢复。
extent 顺序 workload 只经过 inode-format 分支，不进入 legacy allocator。

两次 20-run 探测都原样判为未通过：第一次 write/read p95 分别为 9.130 ms 和
9.978 ms，超过 dev p95 10% 上限；第二次 write/read 均通过，但 sync p95 为
45.740 us，超过 42.508 us 上限。随后扩大为 50 次测量以降低单个离群值对 p95
的支配，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-10-legacy-indirect-allocation.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=dad8b5da29364b0a5d1b85c9aaec912645c72b42 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6395663 write_p95_ns=7102484 read_median_ns=6319891 read_p95_ns=7474386 sync_median_ns=31184 sync_p95_ns=47367
```

相对 dev 基线，write/read median 分别改善约 6.3% 和 12.5%，write p95 改善约
3.2%，read p95 改善约 11.9%，因此 data workload 通过。sync median/p95 分别
回退约 20.8% 和 22.6%，该检查点整体仍登记为性能红项，不放宽断言或选择性
丢弃样本。同机对上一实现点 `bbecc4873` 的 50-run 对照得到 sync median/p95
31.568/41.513 us，说明新增 allocator 没有造成 median 固定开销，但当前 harness
的微秒级 sync 尾延迟仍未满足冻结 dev 基线；在门槛恢复前不得把本项标记为绿。
