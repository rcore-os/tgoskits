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

`scripts/test/data/rsext4-linux-7.1-map.json` 将该表扩展为固定 commit 的源码区间
清单。每个区间必须记录符号、状态机、不变量、Rust owner、差异理由和测试 ID。
`scripts/test/check_rsext4_linux_map.py` 的普通 CI 模式检查冻结 inventory、blob、
区间连续性和字段完整性；`--linux-src <path>` 进一步对照真实 Linux checkout 的
HEAD、完整文件集合、blob 和行数。最终门禁必须增加 `--require-reviewed`，拒绝
任何仍用 `coarse` 整文件占位的条目。当前清单覆盖 61 个 tracked 文件、77,895
行，其中 8 个 Linux build/KUnit 文件已完成排除审阅，其余文件仍必须按顶层
符号和预处理分支拆分，因此 `linux-map-complete` 继续保持红项。

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
- journal commit 或 replay 一旦失败，abort 必须在 mount object 生命周期内保持
  粘性。本次操作返回原始 domain error；之后的 write、handle、flush 与 unmount
  commit 均返回 `JournalAborted`，不得重试已经部分持久化的 transaction state。

## 6. Draft 红测台账

| ID | 基线失败 | 最终要求 | Owner phase | 当前状态 |
| --- | --- | --- | --- | --- |
| `boundary-no-os-deps` | `ax-kspin`、`log` direct dependencies | boundary script passes | portable core skeleton | 绿：`RSEXT4_BOUNDARY_PASSED` |
| `domain-error-no-errno` | core 公开并按 Linux `Errno` 分支 | typed domain error，errno 仅由 adapter 映射 | portable core skeleton | 绿：core 已无 `Errno`；`ax-fs-ng` 集中映射 |
| `owned-mount-boundary` | caller 分别持有公开字段的 `Ext4FileSystem` 和公开 `Jbd2Dev`，且 block device 必须同时实现 `Clock` | 私有 `Ext4<D, S>` 独占 device/cache/journal/services；`BlockIo` 与 `Clock` 分离；只公开 typed operations/DTO | portable core skeleton | 进行中：`Ext4<D, MountedServices<...>>` 已消费 device 与 `MountServices`，独立 clock callback 驱动 metadata 链路；ax-fs-ng 现由一个 sleepable mutex 独占 `MountedExt4`，mount、inode I/O、readdir、namespace mutation、sync/unmount 不再 split 或访问 core cache/superblock/JBD2，手写 `unsafe Send/Sync` 已删除。host harness 也已改用 typed `format` 与 owned inode I/O，不再依赖 legacy path/JBD2 proxy。旧 path/fd re-export、公开 `Ext4FileSystem`/`Jbd2Dev` 与 `initial_jbd2dev` 仍待 crate tests/axtest 迁移后删除；RW 失败后同一 cache owner 的 RO fallback 也待恢复阶段证明，尚不能转绿 |
| `directory-name-no-truncate` | `insert_dir_entry` 对超过 255 byte 的名称静默截断并仍返回成功 | raw name 在任何 inode/dirent mutation 前校验；非 UTF-8 合法，空串、NUL、`/`、超过 255 byte 明确拒绝 | namespace boundary | 绿：256-byte 名称确定性红测证明旧实现创建截断 dentry；`FileName` 与 strict insert 现在在分配/插入前返回 `InvalidInput`，同一测试验证没有遗留 255-byte truncated entry |
| `typed-namespace-create` | create/mkdir 接收 absolute UTF-8 path，core 自动创建父目录并在创建后由 adapter 二次修改 mode | `parent inode + FileName + FilePermissions + MutationContext`，path/permission policy 留在 VFS | namespace boundary | 进行中：owned API 已提供 raw-byte regular-file/directory/special-inode/symlink create，并在首次 metadata publish 时应用 uid/gid/umask；ax-fs-ng create 已迁移到 resolved parent inode 与 typed DTO，不再路径查找或二次直改 inode。VFS 现有 symlink create/set 两阶段合约、project inheritance/quota 与 caller context 贯通仍为红项 |
| `special-inode-rdev` | extents-enabled filesystem 对 CHR/BLK/FIFO/SOCK 无条件写 extent header，且 core 没有 `i_rdev` codec，special inode 还能错误携带普通文件 payload | 仅 DIR/REG/normal symlink 初始化 extent tree；CHR/BLK 使用 Linux old/new device codec，FIFO/SOCK 保持空 `i_block`；typed create 拒绝类型/payload 不匹配 | inode codec/namespace boundary | 绿（typed primitive）：确定性红测证明旧 char inode 带 `EXT4_EXTENTS_FL`；同一测试现要求零 size/block 且拒绝 payload。`DeviceNumber` checked major/minor 与 old/new codec 单测通过，owned `create_special_inode` 持久化 259:511 后由 Linux `debugfs` 解码一致且 `e2fsck -fn` clean。rename whiteout 的同 transaction 创建/回滚仍归属 `typed-rename-flags` 与 `rename-mutation-rollback` 红项 |
| `typed-hard-link` | hard link 重新解析两个 UTF-8 absolute path，并先发布 dentry、后增加 target nlink | `target inode + parent inode + FileName`；先更新 target nlink，再发布 raw dentry，失败时仅在确认 dentry 未出现后恢复计数 | namespace boundary | 绿（typed primitive）：raw 非 UTF-8 hard-link test 验证同 inode/nlink=2；完整 crash atomicity 仍归属统一 JBD2 handle，link nlink=0 的 tmpfile/orphan resurrection 尚未启用 |
| `typed-unlink-open-lifecycle` | core final unlink 立即释放 inode/data block，可能破坏仍由 VFS 引用的 open inode；adapter 另写一套 zero-link 逻辑 | `parent inode + FileName -> UnlinkOutcome`；最后 dentry 消失后 inode 保持 allocated/readable，VFS 最后引用释放后显式 reap | namespace/lifecycle boundary | 绿（运行时生命周期）：确定性红测证明旧实现 final unlink 立即清 inode bitmap；typed raw-name unlink 现在返回剩余 nlink，zero-link inode 持续按 inode number 可读写，显式 reap 才释放。ax-fs-ng 现通过 owned `Ext4::unlink` 消费同一 outcome，并以唯一、失败可重试的 reap claim 串行化 unlink fast path 与 `Inode::drop`。完整 crash atomicity 继续由 JBD2 transaction 阶段负责 |
| `typed-rmdir-open-lifecycle` | path-based `delete_dir` 递归回收目录，无法保留 VFS 已打开目录的 inode 生命周期 | 仅删除空目录名称，target 进 orphan/zero-link，最后 VFS ref 释放后才降 `used_dirs` 并回收 | namespace/lifecycle/JBD2 boundary | 进行中：owned `remove_empty_directory` 与 ax-fs-ng rmdir 已共用 `UnlinkOutcome`/reap tracker，空目录持有时 inode 保持 allocated，非空目录确定性无变更，`used_dirs` 仅在 reap 降低。坏 `.`/`..`/dirent 的 strict empty-dir codec，dentry+nlink+orphan+parent time 的单一 journal transaction 与后置 I/O 失败回滚仍为红项 |
| `symlink-target-transaction` | adapter 手工释放旧块后再分配新 target，60-byte 边界错误且失败可泄漏/留下部分状态 | Linux 59-byte fast/60-byte long 边界，long disk payload 含 NUL，新 target 完整构造后原子发布并在同 transaction 释放旧块 | inode/namespace/JBD2 boundary | 进行中：core typed create/replace 与 ax-fs-ng 已统一，59/60、fast→long→fast 确定性测试通过，long payload 为 `target + NUL`。新 inode 发布后释放旧块遇 I/O 失败仍可返错但已生效并泄漏；VFS create/set 两阶段也尚非 Linux 原子 symlink create，必须随 filesystem-owned journal transaction 整体重构 |
| `typed-rename-flags` | path-based rename 先删除目标再移动，same-path 会删除自身；Starry 在 syscall 层预查 `NOREPLACE` 后丢弃 flags，存在 TOCTOU；无 `EXCHANGE` | `old/new parent inode + raw FileName + RenameOptions -> RenameOutcome`；same-inode no-op，`NOREPLACE` 与 mutation 同锁判定，`EXCHANGE` 原位交换，替换目标沿 orphan/reap 生命周期处理 | namespace/VFS boundary | 进行中：same-path 确定性红测证明旧实现返回 `NotFound` 且删除源项；owned core 已支持 raw-name `REPLACE`/`NOREPLACE`/`EXCHANGE`，覆盖非 UTF-8 跨目录交换、目录环、跨父目录 `..`/nlink 和替换目标延迟 reap。VFS、ax-fs-ng 与 Starry 已贯通不可构造非法组合的 typed options，ext4 adapter 按真实 `RenameOutcome` 发布 zero-link；`WHITEOUT`、tmpfs/overlay 的完整 exchange/whiteout、filesystem-owned JBD2 原子 transaction 及 legacy path/split-state 删除仍为红项 |
| `classic-orphan-recovery` | `s_last_orphan`/`i_dtime` 只有 codec，无 add/del、mount recovery 或链损坏防护 | replay 后校验经典 orphan 链；zero-link inode 可重启回收；范围、未分配 inode 与环明确拒绝 | inode/JBD2 lifecycle | 进行中：zero-link unlink 头插经典链，显式 reap 支持头/中间节点摘除并在最终 bitmap free 前保留 orphan-next；不干净提交后的两节点链在 JBD2 replay 后、root/`lost+found` 修复前完成回收，自环确定性镜像拒绝挂载。linked truncate orphan recovery、orphan-file feature、block-by-block restartable truncate/free，以及 dentry+nlink+orphan+bitmap 的单一 filesystem-owned journal transaction 仍为红项，不能宣称 Linux crash parity |
| `mkdir-publish-rollback` | child inode/block finalize 后 parent dentry 扩块失败会泄漏分配，并提前增加 parent nlink/used-dirs | 失败时恢复 child allocation、parent link count 与 group directory accounting；最终由统一 journal handle 保证原子性 | namespace/JBD2 boundary | 绿（局部回滚）：确定性 ENOSPC 红测中旧实现把 root nlink 从 3 留成 4 且消耗最后 block/inode；resolved-parent primitive 现在使同一测试转绿。父目录 ctime 与任意持久化边界的完整原子回滚仍归属 JBD2 transaction 阶段 |
| `feature-gate-strict` | unknown incompat、ENCRYPT、RW QUOTA 均被接受 | incompat 拒绝；未实现 RO_COMPAT 只允许 RO | codec/feature negotiation | 绿：四项确定性单测完成红绿验证 |
| `device-sector-map` | filesystem block number 被直接作为 device sector，512-byte 设备只读一个 sector | typed `SectorId` + private filesystem-block mapper | portable I/O core | 绿：512-byte sector 聚合与 byte-offset superblock 红绿回归通过 |
| `filesystem-block-dynamic` | core 算法仍大量引用 4 KiB 常量 | 1/2/4 KiB geometry、cache、JBD2 与 codec 全部按 mount 派生 | codec/geometry | 绿：Linux 与 rsext4 各自创建的 1/2/4 KiB 镜像均在 512-byte sector 上完成跨块写入、rename、remount 与 `e2fsck -fn`；cache、extent 与 JBD2 buffer 均按 mount geometry 分配 |
| `linux-default-rocompat-rw` | Linux mkfs 默认设置 `HUGE_FILE`、`DIR_NLINK` | 完整读写语义后纳入 writable mask | inode/namespace lifecycle | 绿：`HUGE_FILE` 统一按 Linux 的 32-bit sector、48-bit sector、filesystem-block 三级 codec 读写，所有 block accounting mutation 使用 checked 状态转换；`DIR_NLINK` 覆盖 65000 到 sentinel 1、连续 mutation 保持 sentinel、无 feature 时分配前返回 `EMLINK`；Linux 默认 feature 的 1/2/4 KiB round-trip、extent/JBD2 replay 与 `e2fsck -fn` 全部通过 |
| `linux-map-complete` | 仅有 subsystem mapping，缺少逐区间清单 | every source line classified | design/traceability | 进行中：冻结 inventory 已覆盖 Linux v7.1 的 61 个 tracked 文件、77,895 行，并由普通 CI 与本地源码双模式检查 gap、overlap、blob、行数和文件集合；8 个 build/KUnit 文件已审阅，其余 53 个 `coarse` 条目使 `--require-reviewed` 确定性失败，必须按符号/预处理区间完成语义审阅后才能转绿 |
| `journal-no-direct-fallback` | uninitialized JBD2 performs home write | typed journal-aborted error | JBD2 rewrite | 绿：确定性红绿回归已覆盖 write/umount |
| `jbd2-handle-credits` | metadata queue 满时会在一个 bulk mutation 中间自动提交，失败后替换过的 pending image 无法恢复 | operation handle 预留 credits，禁止 operation 内切 transaction，并在 operation error 时恢复 running queue | JBD2 rewrite | 进行中：私有 in-memory handle 已按 distinct metadata block 计 credit；bulk write 在任何预提交前校验输入，预留不足时先提交旧 queue，handle 内禁止 auto-commit，credit overrun/operation error 恢复 queue snapshot，nested handle 与 active-handle umount 明确返回 busy。固定且公开的 10-buffer 上限已删除，当前 single-descriptor writer 从 block size、tag/UUID/checksum tail 与 journal ring geometry 推导安全容量；连续与 mapped journal 安装都在发布 state 前验证容量，exact-capacity 与 capacity+1 的两次 commit 均逐 home block 验证。该层尚未拥有 filesystem cache/bitmap/inode undo、Linux running/committing/checkpoint transaction 或 revoke，因此不能解除 recursive indirect truncate 红项 |
| `jbd2-abort-sticky` | descriptor/payload flush 失败只返回一次 I/O error，随后仍可从已推进的 ring cursor 重试、继续 metadata write 或关闭 journal 绕过错误 | 首次提交/恢复错误保留原始 cause；同一 mount 的后续 mutation、handle、flush、unmount 全部稳定拒绝，并持久化 journal errno | JBD2 rewrite | 进行中：所有 auto-commit、handle precommit 与 unmount commit 已收口到单一 transaction owner；任一 commit/cache-coherence failure 锁存首个 cause，本次返回原始 typed error，后续 write/handle/flush/unmount/reinstall 返回 `JournalAborted`。journal mode 切换改为 fallible state transition：abort 时拒绝，pending queue 或 active handle 时返回 busy，不能再关闭 journal 后绕过未提交 metadata。replay 现在以 typed `JournalReplayPhase` 区分 initialize/scan/revoke/replay/persist/cache，保留 I/O、checksum 与 corruption 原始 domain cause、事务 restart 位置和 progress 持久化次错；mount 返回首错并通过 `Observer` 发送完整 typed failure，不再统一伪装为 corruption，越界 `s_start` 也不再清日志报成功。descriptor read 确定性红测已在旧实现证明 `Corrupted != Io`，payload read、home write、checksum+flush 首错优先、final flush 与 replay superblock write fault 均有定点测试。首次 abort 同时以私有 JBD2 wire code 持久化 `s_errno`，重新计算 checksum，并通过原生 FUA 或明确的 write-then-flush fallback 等待 durability；两种能力都缺失时返回 unsupported，record 失败单独保存且不覆盖首次 cause。当前 single-payload transaction 的 open-superblock、descriptor、payload、commit、checkpoint、close-superblock 六次 write 与四个 flush barrier 已逐项注入并验证 sticky first-error。精细 on-disk error mapping、Linux 独立 scan/revoke/replay 三遍及 pass-end 一致性、`ACK_ERR`/shutdown、ext4 `continue`/`remount-ro` policy，以及 multi-payload checkpoint/revoke 的完整 fault matrix 仍为红项 |
| `jbd2-csum-v3-write-replay` | writer emits legacy tags while accepted CSUM_V3/64BIT journals require tag3/high block numbers | Linux-compatible descriptor tags and checksum followed by self/Linux replay | JBD2 rewrite | 绿：writer 生成 tag3/64-bit block number、escaped payload CRC32C、descriptor/commit checksum；replay 在任何 home write 前校验 commit 与全部非 revoke payload，并校验 descriptor/revoke tail；Linux `debugfs` 多块事务与逐边界损坏测试通过；mkfs 将 ext4 `metadata_csum`/`64bit` 映射为对应 JBD2 feature |
| `extent-checked-codec` | raw extent nodes are sorted after parsing and malformed roots/children can be treated as holes | checked structural validation preserves on-disk order and propagates corruption | mapping rewrite | 绿：root/child codec 检查 magic、depth、capacity、非空 index、logical/physical overflow 与 leaf/index ordering；`EXT4_EXTENTS_FL` 是唯一格式判据，坏 magic 不再降级为 legacy/hole；读取、查找、插入、删除、HTree 和 block resolver 均传播 typed error，不再排序或吞错；hard-link parent corruption 完成确定性红绿验证 |
| `extent-empty-index` | crafted empty or malformed internal child can panic | corruption error, no mutation | mapping rewrite | 绿：root 与 external child 在 mutation 前统一 checked decode；空 index、坏 child 与超过 inline root 容量均返回 corruption，确定性测试验证 inode 不被截断或修改 |
| `extent-block-checksum` | extent block lookup lacks the inode number required by metadata checksum and assumes the checksum tail is always at the end of the block | every resolver carries typed inode identity and verifies the Linux `eh_max`-derived checksum tail | mapping rewrite | 绿：resolver/HTree/mount/adapter 调用链显式传递 `InodeNumber`；external node 读写按 inode generation/number 校验 CRC32C，2 KiB `eh_max` tail offset 与损坏测试通过 |
| `extent-system-zone-validity` | physical extents are checked only against filesystem/device bounds | reject overlap with ext4 system metadata zones, with Linux's owning-inode exception | mapping rewrite | 绿：mount/replay 后完整构建并一次发布 immutable zone index，覆盖 per-group super/GDT/reserved GDT、bitmap、inode table 与 internal journal blocks；普通 inode 指向 block bitmap 的确定性红绿测试完成，journal inode owner exception 单测通过；first-data、溢出和 filesystem/device 上界继续共同生效 |
| `mount-option-block-validity` | core did not protect system metadata blocks | default `block_validity` plus Linux-compatible `noblock_validity` mount/remount lifecycle | mount/remount options | 绿：RW/RO mount 默认建立 layout + internal-journal owner system-zone index，`with_block_validity(false)` 在 initial mount 与 replay reload 保持空索引；owned `remount` 禁用时释放，重新启用时先完整构建后一次发布。crafted block-bitmap extent 确定性红测证明仅修改 option 但不释放 index 仍拒绝；同一测试现要求 disable 允许、reenable 再拒绝，extent 与 legacy indirect 共用同一 index |
| `mount-remount-full` | mount options 仅有 readonly/replay，remount 无统一 state transition | 完整对齐 Linux ext4 mount/remount options，ro↔rw、journal/data mode、barrier/discard/error policy 与失败回滚 | mount/remount/JBD2 options | 红：当前 owned `remount` 只允许在 readonly/replay mode 不变时切换 `block_validity`，明确拒绝 mode change；不能用该子集声称 Linux remount 完整性 |
| `extent-mutation-rollback` | split/remove 的 metadata write 或 bitmap I/O 失败可留下泄漏、部分释放或不可达节点 | plan/validate/journal persist，任一失败保持旧树与 bitmap/i_blocks 一致 | mapping rewrite | 红：HUGE_FILE checked accounting 已在分配/释放前预检；跨多个 metadata/bitmap I/O 的完整事务回滚仍待 extent 整体重写 |
| `mkdir-mutation-rollback` | child inode 初始化后，父 link/group accounting 或目录项插入失败可留下孤儿 inode、泄漏块或部分发布的计数 | mkdir 的 inode、block、父目录项、父 link count 与 group stats 属于同一可回滚 transaction | namespace/JBD2 rewrite | 红：link 上限已在分配前预检；目录项插入及其后续 I/O 失败的整体回滚仍待 namespace transaction 重写 |
| `rename-mutation-rollback` | 跨父目录 rename 在新项、旧项、父 link count 或 `..` 更新任一步失败时可留下部分状态 | rename 的全部目录项、link count、`..` 与被替换 inode 更新崩溃原子且可回滚 | namespace/JBD2 rewrite | 红：typed rename 已把目标 dentry 替换改为精确 block+offset 原位更新，所有目录 nlink、目标 free preflight 和 ancestry 均在 namespace publish 前校验；当前 best-effort rollback 仍不能恢复目录扩块、inode/cache/bitmap 和任意持久化边界，多对象 mutation 必须迁入 filesystem-owned journal transaction 后才能转绿 |
| `io-failure-no-panic` | mount/commit paths contain `expect` | all errors propagated | codec/JBD2 rewrite | 进行中：mount/JBD2 与 extent root/child traversal 已移除 panic/静默失败，剩余生产路径待审计 |
| `legacy-indirect-13-blocks` | non-extent path is unsupported | Linux-compatible mapping | mapping rewrite | 进行中：checked read 与 allocate-before-publish write 已覆盖 direct/single/double/triple、hole、整块 pointer validity、system zone、cycle、data+metadata `i_blocks` 与运行时失败反向 rollback；跨 direct/single 的 Linux image 已通过 umount、e2fsck、remount/read、再次 e2fsck。full ownership preflight 现在不受 `i_size` 裁剪，完整收集 data 与 child-first metadata，拒绝跨树重复物理块和隐藏损坏；legacy direct-only final free 已改用该结果，修复 size 外 pointer 泄漏。truncate grow 对 extent/legacy 都只发布 sparse `i_size`，不再预分配；旧 partial EOF 在 grow 前清零，whole-file read 保留 extent hole 的逻辑位置，1/2/4 KiB Linux image 已通过 umount、e2fsck、remount/read、再次 e2fsck。recursive indirect truncate/free、punch 与 crash-atomic journal transaction 仍为红项；含 indirect root 的 shrink 继续在任何 inode/bitmap mutation 前完成只读 preflight 后返回 typed unsupported |

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

### 7.10 sparse truncate growth 检查点

采集时间：2026-08-11；被测实现 commit 为
`5bcde9da75af753dee5cd496a021f320465a74fd`，环境与 7.8 相同。该检查点将
truncate grow 对齐 Linux sparse 语义，不再为新逻辑长度预分配 extent 或 legacy
block；grow 前清零旧 partial EOF，并让 extent whole-file read 按逻辑块位置重建
hole。dense extent 热路径直接顺序遍历映射值，避免为无 hole 的既有 workload
逐块执行 sparse 判定。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-sparse-truncate.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=5bcde9da75af753dee5cd496a021f320465a74fd arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6395756 write_p95_ns=7162953 read_median_ns=6328195 read_p95_ns=7062791 sync_median_ns=30754 sync_p95_ns=41326
```

相对 dev 基线，write/read median 分别改善约 6.3% 和 12.3%，对应 p95 分别
改善约 2.4% 和 16.7%；sync p95 回退约 6.9%，仍在 10% latency 上限以内。
sync median 增加约 19.1%，但该指标不是吞吐/IOPS workload，sync latency 的
硬门槛按 p95 判定；因此本检查点满足冻结的 host 性能门槛。7.9 的 legacy
allocator sync p95 红项仍独立保留，未被本检查点覆盖或改判。

### 7.11 JBD2 handle credits 检查点

采集时间：2026-08-11；被测实现 commit 为
`5e143a8302dd441188c7cfe35e603414a00bc0ee`，环境与 7.8 相同。该检查点在
当前 in-memory running queue 上为 bulk metadata write 建立私有 handle：按不同
home block 计 credit，开始 operation 前预留 queue 空间，handle 内禁止 auto
commit，并在 operation error 时恢复 queue snapshot。它尚不包含 filesystem
cache/bitmap/inode undo、revoke、abort state 或 Linux 的多 transaction 状态机。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-jbd2-handle-credits.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=5e143a8302dd441188c7cfe35e603414a00bc0ee arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=7035652 write_p95_ns=7129911 read_median_ns=6662757 read_p95_ns=6768021 sync_median_ns=39193 sync_p95_ns=41800
```

相对 dev 基线，write median 回退约 3.1%，仍在 5% 上限内；read median 改善
约 7.7%，write/read p95 分别改善约 2.8% 和 20.2%。sync p95 回退约 8.2%，
仍在 10% latency 上限内。sync median 增加约 51.8%，但 sync latency 硬门槛按
p95 判定；因此本检查点满足冻结的 host 门槛。7.9 的 legacy allocator sync p95
红项仍独立保留。

### 7.12 JBD2 dynamic transaction capacity 检查点

采集时间：2026-08-11；被测实现 commit 为
`4bba3dc12d0c05b5deec3a8558cdaa02d91d944f`，环境与 7.8 相同。该检查点删除
固定且公开的 10-buffer 上限；当前 single-descriptor writer 从 filesystem block
size、descriptor header、tag/UUID/checksum tail 和 journal ring geometry 推导每个
transaction 的安全 update 上限。连续与 mapped journal 都在安装 state 前完成
feature、checksum、mapping 与最小容量验证。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-jbd2-dynamic-capacity.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=4bba3dc12d0c05b5deec3a8558cdaa02d91d944f arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6467905 write_p95_ns=7227933 read_median_ns=6358891 read_p95_ns=7332003 sync_median_ns=31402 sync_p95_ns=37894
```

相对 dev 基线，write/read median 分别改善约 5.3% 和 11.9%，对应 p95 分别
改善约 1.5% 和 13.6%；sync p95 改善约 1.9%。sync median 增加约 21.6%，但
sync latency 门槛按 p95 判定；因此本检查点满足冻结的 host 门槛。7.9 的 legacy
allocator sync p95 红项仍独立保留。

### 7.13 JBD2 sticky abort 检查点

采集时间：2026-08-11；被测实现 commit 为
`b55692634f4b888ef83f9bb3f666dd230108883b`，环境与 7.8 相同。该检查点将所有
auto-commit、handle precommit 与 unmount commit 收口到单一 owner：首次失败
锁存原始 cause，之后的 write、handle、flush、unmount、mode change 与 state
reinstall 均返回 typed abort。`set_journal_use` 同时改为 fallible state transition，
禁止 active handle 或 pending queue 被关闭 journal 后绕过。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-jbd2-sticky-abort.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=b55692634f4b888ef83f9bb3f666dd230108883b arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6312878 write_p95_ns=6917725 read_median_ns=6221041 read_p95_ns=6873813 sync_median_ns=29124 sync_p95_ns=37816
```

相对 dev 基线，write/read median 分别改善约 7.5% 和 13.8%，对应 p95 分别
改善约 5.7% 和 19.0%；sync p95 改善约 2.1%。sync median 增加约 12.8%，但
sync latency 门槛按 p95 判定；因此本检查点满足冻结的 host 门槛。7.9 的 legacy
allocator sync p95 红项仍独立保留，未被本检查点覆盖或改判。

### 7.14 JBD2 abort errno durability 检查点

采集时间：2026-08-11；被测实现 commit 为
`367160c3b1f6da4c3299a94b814150dd8db42be2`，环境与 7.8 相同。该检查点将首次
journal abort 记录到 JBD2 superblock `s_errno`，重新计算 checksum，并以设备
原生 FUA 或 write-then-flush fallback 等待 durability；记录过程的第二错误单独
保存，不覆盖提交首错。没有 FUA/flush 能力时明确返回 unsupported，不能伪造
持久化成功。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-jbd2-abort-errno.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=367160c3b1f6da4c3299a94b814150dd8db42be2 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6273551 write_p95_ns=6709066 read_median_ns=6146874 read_p95_ns=6833113 sync_median_ns=28082 sync_p95_ns=47959
```

相对 dev 基线，write/read median 分别改善约 8.1% 和 14.8%，对应 p95 分别
改善约 8.5% 和 19.4%；sync median 回退约 8.7%。sync p95 受 47.959 us 与
61.344 us 两个尾延迟样本影响为 47.959 us，相对 38.644 us 基线回退约
24.1%，超过 10% latency 上限。因此本检查点原样登记为性能红项，不丢弃
离群样本，也不选择性复测覆盖；与 7.9 的 legacy allocator 红项一并留待整体
性能收敛阶段定位、优化并用相同 harness 重新验证。

### 7.15 JBD2 typed replay failure 检查点

采集时间：2026-08-11；被测实现 commit 为
`d7eaa40eb3950506744e62b277dd36069873d182`，环境与 7.8 相同。该检查点让
replay failure 保留 OS 无关的 initialize/scan/revoke/replay/persist/cache phase、
原始 domain cause、restart 位置和 progress persistence 次错；mount 返回同一首错，
并通过 typed `Observer` event 提供完整诊断。越界 `s_start` 不再清日志报成功，
空 descriptor 未提交尾部仍按 Linux clean-end 语义丢弃。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-jbd2-typed-replay.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=d7eaa40eb3950506744e62b277dd36069873d182 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6285312 write_p95_ns=6774413 read_median_ns=6107418 read_p95_ns=7027885 sync_median_ns=29408 sync_p95_ns=35354
```

相对 dev 基线，write/read median 分别改善约 7.9% 和 15.4%，对应 p95 分别
改善约 7.6% 和 17.1%；sync p95 改善约 8.5%。sync median 增加约 13.9%，
但 sync latency 门槛按 p95 判定；因此本检查点满足冻结的 host 门槛。7.9 的
legacy allocator 与 7.14 的一次 sync p95 红项仍原样保留，未被本次通过结果
覆盖或改判。

### 7.16 owned mount boundary 探索性检查点

采集时间：2026-08-11；被测实现 commit 为 `d2871bd7d`，固定 CPU 2，环境与
7.8 相同。该检查点让 `Jbd2Dev` 的内部 timestamp 调用使用 mount 注入的独立
`Clock` callback，并建立消费 device、cache、journal 和 services 的私有
`Ext4<D, S>` owner。当前冻结 harness 仍调用 legacy path API，因此这里只测量
内部 callback 对既有 workload 的影响；待 harness 随公共 API 一并迁移后必须
重新做最终 A/B，不能把本结果视为新 API 性能证明。

本次探索使用 3 次预热与 10 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-owned-mount-boundary.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=d2871bd7d arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=7272243 write_p95_ns=11335665 read_median_ns=6987463 read_p95_ns=7625277 sync_median_ns=37096 sync_p95_ns=46288
```

相对 dev 基线，read median/p95 分别改善约 3.2%/10.1%；write median 回退约
6.5%，write p95 回退约 54.5%，sync p95 回退约 19.8%，均超过相应硬门槛。
11.336 ms write 尾延迟样本未丢弃，也未用选择性复测覆盖；本检查点登记为性能
红项。该 callback 每次 metadata timestamp 才执行一次，尚不能仅凭此样本把尾
延迟归因于动态分派；整体性能收敛阶段必须用最终 owned API harness、同机 dev
对照和至少 20 次正式测量重新定位。

### 7.17 typed rename 检查点

采集时间：2026-08-11；被测实现 commit 为
`e3181fb4dac47922ee68db7267182e141d92f763`，固定 CPU 2，环境与 7.8 相同。
该检查点建立 core、VFS 与 Starry 的 typed rename flags/outcome，覆盖 same-path
no-op、`NOREPLACE`、`EXCHANGE`、跨父目录 `..` 与 link count、替换目标 orphan
生命周期和目录环检测。当前冻结 harness 仍调用 legacy path API，因此结果不构成
typed rename 或最终 owned API 的因果性能证明。

正式检查点使用 3 次预热与 20 次测量，全部原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-typed-rename.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=e3181fb4dac47922ee68db7267182e141d92f763 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=7238489 write_p95_ns=7733222 read_median_ns=7235475 read_p95_ns=9102480 sync_median_ns=41238 sync_p95_ns=53439
```

相对 dev 基线，write median/p95 分别回退约 6.0%/5.4%，read median/p95 分别
回退约 0.2%/7.3%，sync median/p95 分别回退约 59.7%/38.3%。write median
超过 5% throughput 门槛，sync p95 超过 10% latency 门槛，因此本检查点原样登记
为性能红项；未丢弃任何样本，也未用选择性复测覆盖。rename 不在该 sequential
workload 的热路径，不能据此把回退归因于 rename 逻辑；整体性能收敛阶段必须迁移
最终 owned API harness，并在同机 dev A/B 中定位波动与真实回退来源。

### 7.18 owned API harness 检查点

采集时间：2026-08-11；被测实现 commit 为
`c1da15b5ebfdee0dfc30d1e1d48932e7a0b58b91`，固定 CPU 2，环境与 7.8
相同。该检查点将冻结 harness 从 legacy `Jbd2Dev + absolute path`
调用迁移到 `format(device, clock, options)` 和私有 `Ext4<D, S>`
owner。format、mount 与 create 仍在计时外；write 计时为已打开
inode 的 `write_inode`，read 计时保留了 read buffer 分配、`read_inode`
和内容校验，sync 计时仍为一次 clean `unmount`，因此没有把
`sync + unmount` 双重提交塞入旧 workload。数据量、随机内容、镜像、
warmup/run 与 marker 字段未变。这是公共 API 边界迁移后的新
frozen harness；dev 基线由旧 API 完成同一 sequential 语义，最终 PR
必须同时报告这一 API 差异，不能宣称为指令级完全相同的 harness。

正式检查使用 3 次预热与 20 次测量，全部有效原始样本保存在
`book/design/data/rsext4-perf/2026-08-11-owned-api-harness.csv`：

```text
RSEXT4_BENCH_SUMMARY commit=c1da15b5ebfdee0dfc30d1e1d48932e7a0b58b91 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6599015 write_p95_ns=7737093 read_median_ns=6700307 read_p95_ns=8030772 sync_median_ns=32339 sync_p95_ns=57306
```

首次采集因手工填写的完整 commit marker 错误而整组作废，之后以
`git rev-parse HEAD` 的真实 SHA 重做完整 3+20；作废原因是元数据
无效，不是选择性剔除性能样本。相对 dev 基线，write median/p95
分别回退约 5.4%/12.3%，read median/p95 分别回退约
10.1%/12.4%，sync median/p95 分别回退约 13.6%/66.1%。因此
median 与 p95 硬门槛均未通过，本检查点保持性能红项；20 个
有效样本一个未丢弃，也不用继续复测覆盖。
