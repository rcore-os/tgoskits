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
- `Delay`：MMP 挂载期所有权检查需要的阻塞等待；
- `Observer`：接收 typed lifecycle/integrity/journal events。

当前 writable feature negotiation 会拒绝 fscrypt/fsverity，因此不预先保存或公开
未被调用的 crypto/key provider；实现对应磁盘语义时再按真实调用链定义最小能力接口。

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
| `fs/ext4/super.c`, `ext4.h` | feature negotiation、mount/remount、错误策略、geometry；普通 mount 只接受存在、已链接、regular 且未加密的 internal journal inode，不修复或合成缺失 inode | `rsext4::ext4` / `superblock` | core | `feature_gate`, journal inode admission, Linux image matrix |
| `fs/ext4/inode.c`, `indirect.c`, `inline.c` | inode lifecycle、map blocks、writeback modes、truncate | inode/mapping services | core | map/truncate、orphan intent 与 restart/crash differential |
| `fs/ext4/extents.c`, `extents_status.c` | checked tree、unwritten extent、split/merge、status cache | extent service | core | codec/property、range-mutation restart tests |
| `fs/ext4/mballoc.c`, `balloc.c`, `ialloc.c` | multiblock allocation、preallocation、rollback、quota charge | allocator service | core | ENOSPC/fault injection |
| `fs/ext4/namei.c`, `dir.c`, `hash.c` | linear/HTree、link count、atomic rename、casefold | directory service | core | Linux syscall trace + e2fsck |
| `fs/ext4/orphan.c`, `mmp.c` | persistent orphan cleanup、multi-mount exclusion | lifecycle service | core | truncate orphan intent + power-cut recovery |
| `fs/ext4/xattr.c`, `acl.c`, `quota.c` | EA inode/block、ACL encoding、quota persistence | metadata services | core | xattr/ACL/quota differential |
| `fs/ext4/crypto.c`, `verity.c` | on-disk policy、Merkle metadata、file data transformation | core + 待实现时定义的最小 runtime capability | capability | Linux image and negative-key tests |
| `fs/ext4/resize.c`, `ioctl.c`, `fsmap.c`, `move_extent.c` | resize and administrative operations | core typed operations | core | ioctl/fsmap/resize differential |
| `fs/ext4/file.c`, VFS operation tables | permission/open/fd/page-cache/direct-I/O dispatch | ax-fs-ng / Starry | glue | common syscall tests |
| ext4 DAX paths | persistent-memory direct mapping | none | not-applicable | mount option returns unsupported |
| `fs/jbd2/transaction.c`, `commit.c` | handles/credits、ordered data、commit record ordering | journal transaction owner | core | multi-transaction restart + phase fault injection |
| `fs/jbd2/recovery.c`, `revoke.c`, `checkpoint.c` | scan/revoke/replay、tail/checkpoint reclamation | journal recovery owner | core | Linux-created journal 与 restart power-cut replay |

Linux v7.1 `fs/ext4/super.c:5504-5510,5886-5960` 在 root inode 之前装载
internal journal，并拒绝不存在、`i_nlink == 0`、非 regular 或加密的 journal
inode。`rsext4` 的普通 mount 同样只校验并装载既有 inode；创建默认 inode 8
被限制在 `mkfs` 的 crate-private bootstrap 路径，不能由损坏镜像触发修复写入。
`super.c:6080-6116` 还要求 internal inode 与 external device 二选一。core 在任何
mount mutation 前拒绝二者同时声明或均缺失；external-only 镜像则返回 typed
`UnsupportedCapability(block_io:external_journal)`，直到双设备 journal/home I/O
ownership、UUID 与 durability 边界完整实现，而不是静默退回主设备上的 inode。
内部 journal 的 JBD2 superblock UUID 是 journal 自己的 checksum seed，Linux
`ext4_open_inode_journal()` 不要求它等于 filesystem `s_uuid`；只有 external journal
设备才比较主 filesystem 的 `s_journal_uuid` 与 journal-device ext4 superblock 的
`s_uuid`。对应确定性回归会改写内部 JBD2 UUID 并重算 superblock checksum，要求
mount 继续成功，避免把 Linux 可挂载的内部 journal 错报为 `EUCLEAN`。

Linux v7.1 对 legacy `uninit_bg`（`RO_COMPAT_GDT_CSUM`）使用 UUID、little-endian
group number 和 group descriptor 字节计算 CRC16；它不复用 `metadata_csum` 的
CRC32C，也不校验 legacy bitmap checksum 字段。core 现在按 feature 选择两种
descriptor checksum 算法，并在首次使用 `BLOCK_UNINIT` bitmap 时忽略陈旧磁盘内容，
根据 superblock/GDT/bitmap/inode-table/journal 的实际 system-zone layout 合成 Linux
等价 bitmap，成功分配后才清 flag 并写回 descriptor checksum。`INODE_UNINIT` 首次
分配同样不读取陈旧 bitmap；若 group 尚未标记 `INODE_ZEROED`，目标 inode record
从全零 raw bytes 初始化，避免解析或保留未初始化 inode-table 尾部。group 0 出现任一
UNINIT bitmap flag 视为损坏。固定 `mkfs.ext4 -O ^metadata_csum,uninit_bg` 镜像回归
覆盖 RW mount、create/write、两次 `e2fsck -fn` 和 remount/read；另有磁盘 bitmap
全 `0xff` 及未清零 inode record 的低层测试约束首次分配语义。

## 4. 公共 API 迁移

v0.8 的生产边界使用拥有私有状态的 `Ext4<D, S>`、typed IDs、domain errors，
以及 `io`/`runtime` capability traits。Linux `Errno` core 类型、错误拼写和
descriptor-style `OpenFile/open/read_at/write_at/lseek` 已删除；低层 path helper、
`Ext4FileSystem` 与 `Jbd2Dev` 暂只服务 crate differential/fault tests，待这些测试迁移后删除。

同一集成 PR 内迁移 `ax-fs-ng` 与所有测试，不维护长期双 API。短期编译迁移
helper 只能存在于未提交的本地步骤，不得进入最终 diff。

当前已删除 `cantflush`、`file_entry_exisr`、
`split_paren_child_and_translatevalid`、`cloc_group_layout`、
`resolve_inode_block_allextend` 与 `remove_extend`；mkfs layout 只保留
`BlockGroupLayout` 和单数的 `*_start_block` 字段。原兼容测试已改为
对正确 API 的独立行为断言，不再用新旧 wrapper 互比。

`Jbd2Dev::buffer_mut()` 与 `Jbd2Dev::write_block()` 也不再构成公开三段式
API。core 内部只能通过 closure-owned `update_block()` 获得短生命周期
mutable image；显式完整 block image 使用 `write_blocks()`。所有生产调用点、
host integration fixture 与 axtest 已迁移，外部调用方无法把未发布 dirty
cache image 带过 transaction boundary。

符号链接迁移不再允许 `create(NodeType::Symlink)` 后调用
`FileNodeOps::set_symlink`。VFS backend 必须实现
`DirNodeOps::create_symlink(name, target, permission, uid, gid)`，并在首次发布
目录项时提供完整 target；已有符号链接不提供原地改 target 的 API。需要改变目标的
调用方应创建新 inode 后通过 rename replacement 发布，或先 unlink 再 create。
rsext4 caller 直接使用 `Ext4::create_symlink`，不再调用已删除的
`Ext4::set_symlink_target`。

目录枚举不再接收或返回裸 `u64` byte offset。core caller 必须保存
`DirectoryCursor` 并把每个 `DirectoryEntry::next_cursor` 原样用于下一次
`Ext4::read_directory`；`Linear` 与 `HTree` cursor 不可混用。OS adapter 若导出
Linux `d_off`，必须把 HTree 的 `(major, minor)` 编成 ABI cookie，并在打开目录的
file description 内另外保存 collision continuation；用户 `lseek` 只恢复 visible
cookie 并清空 continuation。VFS directory sink 接收 raw `&[u8]` 名称，只有明确
要求 Unicode 的高层 API 才能执行 UTF-8 转换。

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
| `boundary-no-os-deps` | `ax-kspin`、`log` direct dependencies | `no_std` core only uses portable capability traits | portable core skeleton | 绿：OS runtime、errno、block adapter 和日志依赖均位于 ax-fs-ng 边界 |
| `domain-error-no-errno` | core 公开并按 Linux `Errno` 分支 | typed domain error，errno 仅由 adapter 映射 | portable core skeleton | 绿：core 已无 `Errno`；`ax-fs-ng` 集中映射 |
| `blockio-adapter-capabilities` | ax-fs-ng 丢弃 read-only、flush/barrier、FUA 与 physical block geometry | adapter 只传递底层真实能力；只读写入在设备 I/O 前返回 typed read-only；缺少 durability 返回 unsupported capability；native FUA 标记每个拆分请求且不增加 post-write flush；physical 与 logical geometry 不混淆 | OS glue/capability boundary | 进行中：只读/flush/FUA 红测现验证零底层只读写、unsupported、真实 flush-as-barrier、一次 native FUA/零普通 write/零 flush，以及 4 个拆分 request 全部带 FUA。512 logical/4096 physical 红测证明旧 adapter 把 physical 错报成 512；同一测试现从 rdif `DeviceInfo` 经 native/region adapter 保留 4096，core 拒绝无效 physical geometry 但允许 filesystem block 小于 physical block。discard 仍为红项；alignment offset/io_min/io_opt 将作为独立 block geometry 能力继续追踪 |
| `readonly-adapter-lifecycle` | ax-fs-ng 在取得 mount owner 前对 readonly sync/shutdown 直接返回成功，丢失 core 的 device flush boundary，且 mounted 状态永不结束 | RO/RW 一律在同一 sleepable mutex 下调用 owned core typed sync/unmount；adapter 不复制 journal/cache/lifecycle/read-only 状态 | OS glue/lock/lifecycle boundary | 绿：共享只读镜像 fixture 先固定旧 sync 的 flush 计数不变，以及 shutdown 后仍可原地 remount；同一测试现验证真实 device flush 与 `Busy(op=remount:unmounted)`。adapter 已删除 mount-time readonly 副本，`FilesystemOps::is_readonly()` 直接查询 owned core，确定性 remount 回归证明状态不会漂移。完整 ax-fs-ng ext4 92+3 tests 和 6/6 clippy 通过 |
| `owned-mount-boundary` | caller 分别持有公开字段的 `Ext4FileSystem` 和公开 `Jbd2Dev`，且 block device 必须同时实现 `Clock` | 私有 `Ext4<D, S>` 独占 device/cache/journal/services；`BlockIo` 与 `Clock` 分离；只公开 typed operations/DTO | portable core skeleton | 进行中：`Ext4<D, MountedServices<...>>` 已消费 device 与 `MountServices`，独立 clock callback 驱动 metadata 链路；ax-fs-ng 现由一个 sleepable mutex 独占 `MountedExt4`，mount、inode I/O、readdir、namespace mutation、sync/unmount 不再 split 或访问 core cache/superblock/JBD2，手写 `unsafe Send/Sync` 已删除。host harness 也已改用 typed `format` 与 owned inode I/O，不再依赖 legacy path/JBD2 proxy。`InodeInfo::file_type`/`is_directory` 和根级 `InodeNumber` re-export 已移除 adapter 对 `disknode::Ext4Inode` 与 `bmalloc` 模块路径的生产依赖。mount fallback 只在 mount 前明确读到 `EXT4_ERROR_FS` 时选择只读 replay，RW/replay failure 不再复用已污染或 abort 的 owner；invalid revoke 红测证明旧实现把首个 `Corrupted` 覆盖成 `JournalAborted`，同一测试现保留首错。显式 forensic no-replay 继续由 typed mount options 提供。descriptor-style fd API 与未消费的 crypto/key provider 已直接删除，不保留兼容 wrapper；低层 path helper、公开 `Ext4FileSystem`/`Jbd2Dev` 与 `initial_jbd2dev` 仍待 crate differential/fault tests 迁移后删除，因此尚不能转绿 |
| `typed-inode-metadata` | owned DTO 无 project ID 和 inode flags，adapter 只能访问磁盘 inode 或调用 path helper | `InodeInfo` 仅公开 typed project ID/用户可见 flags；`InodeMetadataUpdate` 仅能改 Linux user-modifiable bits；未启用 project feature 时 0 为 no-op，非 0 返回 unsupported | inode/capability boundary | 进行中：旧公共 API 编译红测缺少 `InodeFlags`/project 字段；同一 owned 测试现验证内部 `EXTENTS` 保留、`NO_DUMP|NO_ATIME` typed 更新和未启用 feature 时的 project 0 no-op/非 0 unsupported。固定 `mkfs.ext4 -I 256 -O project` 镜像现由 owned core 设置 project 1234/`PROJINHERIT`，子 inode 继承后 Linux `debugfs stat` 解码一致且 `e2fsck -fn` clean。quota transfer 和同一 filesystem-owned transaction 仍为红项 |
| `directory-name-no-truncate` | `insert_dir_entry` 对超过 255 byte 的名称静默截断并仍返回成功 | raw name 在任何 inode/dirent mutation 前校验；非 UTF-8 合法，空串、NUL、`/`、超过 255 byte 明确拒绝 | namespace boundary | 绿：256-byte 名称确定性红测证明旧实现创建截断 dentry；`FileName` 与 strict insert 现在在分配/插入前返回 `InvalidInput`，同一测试验证没有遗留 255-byte truncated entry |
| `typed-namespace-create` | create/mkdir 接收 absolute UTF-8 path，core 自动创建父目录并在创建后由 adapter 二次修改 mode | `parent inode + FileName + FilePermissions + MutationContext`，path/permission policy 留在 VFS | namespace boundary | 进行中：owned API 已提供 raw-byte regular-file/directory/special-inode/symlink create，并在首次 metadata publish 时应用 uid/gid/umask；ax-fs-ng create 已迁移到 resolved parent inode 与 typed DTO，不再路径查找或二次直改 inode。regular/special/symlink/mkdir 现共用 Linux extent/no-quota 的 `24 + 12 + 3 = 39` credit owner：payload data 在 metadata commit 前 ordered write，child/parent inode、dentry/dir block、本次变化的 block/inode bitmap、GDT、superblock 与 `used_dirs` 在同一 handle 发布。file 与 directory 的目录块写后故障红测均证明旧实现返回 I/O error 后仍留下可达名称；同一测试现经重挂载验证名称、free block/inode、parent nlink 与 `used_dirs` 完整恢复。legacy `create_symbol_link` 也已删除重复分配实现并复用 typed primitive。VFS 已删除 create/set 两阶段 symlink 合约并迁移为必选 typed atomic create；project inheritance/quota 与 caller context 贯通仍为红项。Linux v7.1 依据为 `fs/ext4/namei.c:2815-2880,2990-3050,3370-3445`、`fs/ext4/ext4_jbd2.h:21-50,78-84` |
| `special-inode-rdev` | extents-enabled filesystem 对 CHR/BLK/FIFO/SOCK 无条件写 extent header，且 core 没有 `i_rdev` codec，special inode 还能错误携带普通文件 payload | 仅 DIR/REG/normal symlink 初始化 extent tree；CHR/BLK 使用 Linux old/new device codec，FIFO/SOCK 保持空 `i_block`；typed create 拒绝类型/payload 不匹配 | inode codec/namespace boundary | 绿（typed primitive）：确定性红测证明旧 char inode 带 `EXT4_EXTENTS_FL`；同一测试现要求零 size/block 且拒绝 payload。`DeviceNumber` checked major/minor 与 old/new codec 单测通过，owned `create_special_inode` 持久化 259:511 后由 Linux `debugfs` 解码一致且 `e2fsck -fn` clean。rename whiteout 的同 transaction 创建/回滚仍归属 `typed-rename-flags` 与 `rename-mutation-rollback` 红项 |
| `typed-hard-link` | hard link 重新解析两个 UTF-8 absolute path，并把 target nlink、目录块和 parent inode 分开发布 | `target inode + parent inode + FileName`；target nlink/ctime、raw dentry block、parent inode 以及目录扩块的 allocation metadata 必须属于同一 filesystem-owned JBD2 handle | namespace/JBD2 boundary | 进行中：raw 非 UTF-8 hard-link test 验证同 inode/nlink=2；确定性 direct-write fault 红测证明旧实现返回 I/O error 后仍把 destination dentry 留在磁盘，同一测试现通过 metadata-aware directory cache 与 COW cache snapshot 在重挂载后恢复旧 nlink 且不出现 destination。第二条红测用 15 个 255-byte 名称填满首块，证明旧实现因 block bitmap 未在 handle 内写回而错误返回成功；同一测试现按本次 group free-count 变化定向物化 block bitmap/GDT/superblock，故障后恢复目录 size/mapping、free count、nlink 与 dentry。credit 已按 Linux extent/no-quota 的 `24 + 12 + 1 = 37` 校正。HTree split、quota credit 和 nlink=0 tmpfile/orphan resurrection 尚未实现，因此仍不能宣称完整 hard-link parity。Linux v7.1 依据为 `fs/ext4/namei.c:53-88,2108-2157,2356-2457,3455-3487,3492-3518`、`fs/ext4/ext4_jbd2.h:21-50,77-90` |
| `typed-unlink-open-lifecycle` | core final unlink 立即释放 inode/data block，可能破坏仍由 VFS 引用的 open inode；adapter 另写一套 zero-link 逻辑 | `parent inode + FileName -> UnlinkOutcome`；最后 dentry 消失后 inode 保持 allocated/readable，VFS 最后引用释放后显式 reap | namespace/lifecycle boundary | 绿（名称发布与运行时生命周期）：typed raw-name unlink 返回剩余 nlink，zero-link inode 持续按 inode number 可读写，显式 reap 才释放。dentry、parent/target metadata 与 classic orphan head 位于同一 24-credit transaction，定点写故障重挂载保持名称、nlink、orphan head、allocation 与内容。ax-fs-ng 用唯一、失败可重试的 reap claim 串行 unlink 与 final drop；global page-cache registry 在 final unlink 后解除 filesystem-wide owner，打开的 fd/mmap 仍保有自己的 cache 并可显式 fsync，不可达的 dirty cache 在最终 drop 丢弃。LoongArch64 grouped QEMU 中 open-unlink 与 ext4-unlink-pagecache 测例通过，1400 文件 page-cache 压力由 timeout 收敛到 66s，紧后 `sync` 为 0s。最终 reap bitmap crash atomicity 继续归属 `classic-orphan-recovery`。Linux v7.1 依据为 `fs/ext4/namei.c:3148-3208`、`fs/ext4/ext4_jbd2.h:20-50,86-104` |
| `typed-rmdir-open-lifecycle` | path-based `delete_dir` 递归回收目录，无法保留 VFS 已打开目录的 inode 生命周期 | 仅删除空目录名称，target 进 orphan/zero-link，最后 VFS ref 释放后才降 `used_dirs` 并回收 | namespace/lifecycle/JBD2 boundary | 进行中：owned `remove_empty_directory` 与 ax-fs-ng rmdir 已共用 `UnlinkOutcome`/reap tracker，空目录持有时 inode 保持 allocated，非空目录确定性无变更，`used_dirs` 仅在 reap 降低。empty-dir scan 现严格验证 inode size、首块、dir/dx checksum、record 长度/对齐/name/inode 上界，首两项必须为 self `.` 与非零 `..`，后续 hole 可跳过但任何非零 inode 均判非空；重算合法 checksum 后把 `.` 指向 root 的确定性红测证明旧实现误报 empty，同一测试现返回 typed corruption。rmdir 名称发布现与 unlink 共用 24-credit owner，把 dentry、target zero-link/size、orphan head、parent nlink/ctime/mtime 置于同一 transaction，替代无法覆盖后写故障的手工补偿。最终 reap/`used_dirs` 的 bitmap crash atomicity 仍为红项。Linux v7.1 依据为 `fs/ext4/namei.c:3236-3319`、`fs/ext4/ext4_jbd2.h:20-50,86-104` |
| `symlink-target-transaction` | VFS 先发布空 symlink，再经 `FileNodeOps::set_symlink` 和 rsext4 `set_symlink_target` 二次改目标；第二步失败会留下可见空链接，原地替换旧块也会泄漏部分状态 | Linux 仅提供创建时给定 final target 的 symlink operation；59-byte fast/60-byte long，long disk payload 含 NUL，inode/payload/dentry/allocation 在同一 handle 发布；改变既有 target 必须 unlink/new inode 或 rename replacement | inode/namespace/JBD2 boundary | 绿：先把 `FsContext::symlink` 改为 typed call 得到缺少 `create_symlink` 的确定性编译红；VFS 随后增加必选、对象安全且无两阶段默认实现的 `DirNodeOps::create_symlink`，通用 `create(NodeType::Symlink)` 在进入 backend 前返回 `InvalidInput`。ext4 adapter 一次调用 owned `create_symlink`，tmpfs 在目录项可见前预留容量并初始化 target，overlay copy-up 先读取 target 再一次创建 upper link；FAT/read-only/pseudofs 明确拒绝。`FileNodeOps::set_symlink`、rsext4 `set_symlink_target` 及其不符合 Linux 的 replace 测试均已删除。host VFS 回归验证 generic create 零 backend 调用且 typed create 首次即可读完整 target；tmpfs axtest 验证容量失败后名称不存在且成功路径首次即可读取 final target；成功路径先初始化 sleepable symlink mutex，再以 IRQ-safe directory lock 二次检查并发布，避免把阻塞锁带入 atomic context，竞争失败则回滚 inode 与容量。LoongArch64 原 CI panic 的完整 grouped QEMU 命令现为 system 424/424、总计 2/2。owned 59/60 boundary 回归继续验证 fast/long 编码。Linux v7.1 依据为 `fs/namei.c:5617-5657`、`fs/ext4/namei.c:2778-2802,3335-3358,3361-3446` |
| `typed-rename-flags` | path-based rename 先删除目标再移动，same-path 会删除自身；Starry 在 syscall 层预查 `NOREPLACE` 后丢弃 flags，存在 TOCTOU；无 `EXCHANGE` | `old/new parent inode + raw FileName + RenameOptions -> RenameOutcome`；same-inode no-op，`NOREPLACE` 与 mutation 同锁判定，`EXCHANGE` 原位交换，替换目标沿 orphan/reap 生命周期处理 | namespace/VFS boundary | 进行中：same-path 确定性红测证明旧实现返回 `NotFound` 且删除源项；owned core 已支持 raw-name `REPLACE`/`NOREPLACE`/`EXCHANGE`，覆盖非 UTF-8 跨目录交换、目录环、跨父目录 `..`/nlink 和替换目标延迟 reap。VFS、ax-fs-ng 与 Starry 已贯通不可构造非法组合的 typed options，ext4 adapter 按真实 `RenameOutcome` 发布 zero-link。普通 rename 现按 Linux extent/no-quota 的 `2 * 24 + 12 + 2 = 62` credits，exchange 按 `2 * 24 + 2 * 12 + 2 = 74` credits，共同发布两侧 dentry/parent inode、source/target inode、目录 `..`、orphan head 及可能变化的 allocation metadata；1 KiB journal 会通过 multi-descriptor writer 保持单 transaction。`WHITEOUT`、tmpfs/overlay 的完整 exchange/whiteout 与 legacy path/split-state 删除仍为红项 |
| `classic-orphan-recovery` | `s_last_orphan`/`i_dtime` 只有 codec，无 add/del、mount recovery 或链损坏防护 | replay 后校验经典 orphan 链；zero-link inode 可重启回收；范围、未分配 inode 与环明确拒绝 | inode/JBD2 lifecycle | 进行中：zero-link unlink 头插经典链，显式 reap 支持头/中间节点摘除并在最终 bitmap free 前保留 orphan-next；dentry、target nlink、parent metadata 与 orphan-head publication 现已统一进入 unlink/rmdir 的 24-credit transaction。不干净提交后的两节点链在 JBD2 replay 后、root/`lost+found` 修复前完成回收，自环确定性镜像拒绝挂载。linked extent truncate 现在按已提交 `i_size` 强制清理 EOF 后映射，成功后才摘链；三块 extent/一块 size 的确定性磁盘红测证明旧 mount 返回 `orphan:linked_inode_recovery`，同一测试现保留 nlink=1、仅 logical block 0 且完成摘链。legacy indirect final unlink 现仅发布 zero-link/orphan，保留 data 与 pointer blocks 供 open inode 使用，显式 reap 才释放 mapping。非空 mapping 继续按 Linux `ext4_blocks_for_truncate + 6 - 3`（无 quota）计算上界；restart 已清空 mapping 后的最终 reap 按真实 touched block 收敛到 5 credits，覆盖目标/前驱 inode table、inode bitmap、GDT 与 superblock。非头 orphan 的精确 5-credit 测试验证 predecessor rewrite，block-bitmap 写后报错测试验证物理前像恢复，跨 direct/single/double/triple 的 zero-link legacy reap 则验证 mapping transaction 与最终 inode transaction 分离。Linux image 内的 linked legacy truncate 与 zero-link legacy reap 经非干净 journal commit、重挂载恢复后验证 mapping/accounting/content，并通过 `e2fsck -fn`。local-value external xattr block 现于同一 reap transaction 内按 refcount 减引用或 revoke/free，并在 bitmap 写故障时完整保留 orphan、`i_file_acl` 与 allocation。orphan-file feature、EA-inode value 引用删除、quota credits 与完整 orphan fault matrix 仍为红项，不能宣称 Linux crash parity。Linux v7.1 依据为 `fs/ext4/inode.c:169-334`、`fs/ext4/truncate.h:30-50`、`fs/ext4/orphan.c:90-187,321-376`、`fs/ext4/ialloc.c:255-345`、`fs/ext4/xattr.c:2906-3014` |
| `mkdir-publish-rollback` | child inode/block finalize 后 parent dentry 扩块失败会泄漏分配，并提前增加 parent nlink/used-dirs | 失败时恢复 child allocation、parent link count 与 group directory accounting；最终由统一 journal handle 保证原子性 | namespace/JBD2 boundary | 绿：确定性 ENOSPC 红测中旧实现把 root nlink 从 3 留成 4 且消耗最后 block/inode；resolved-parent primitive 先使同一测试转绿。后续目录块写后故障又证明 best-effort cleanup 会把失败的名称留在磁盘；mkdir 现由 39-credit filesystem transaction 共同拥有 child inode/dir block、parent dentry/inode、allocation bitmap/GDT/superblock 与 `used_dirs`，同一红测重挂载后全部恢复 |
| `feature-gate-strict` | unknown incompat、ENCRYPT、RW QUOTA 均被接受 | incompat 拒绝；未实现 RO_COMPAT 只允许 RO | codec/feature negotiation | 绿：四项确定性单测完成红绿验证 |
| `device-sector-map` | filesystem block number 被直接作为 device sector，512-byte 设备只读一个 sector | typed `SectorId` + private filesystem-block mapper | portable I/O core | 绿：512-byte sector 聚合与 byte-offset superblock 红绿回归通过 |
| `filesystem-block-dynamic` | core 算法仍大量引用 4 KiB 常量 | 1/2/4 KiB geometry、cache、JBD2 与 codec 全部按 mount 派生 | codec/geometry | 绿：Linux 与 rsext4 各自创建的 1/2/4 KiB 镜像均在 512-byte sector 上完成跨块写入、rename、remount 与 `e2fsck -fn`；cache、extent 与 JBD2 buffer 均按 mount geometry 分配 |
| `htree-hash-checked-lookup` | legacy/half-MD4/TEA 是占位算法，未知版本静默返回 0；root parser 用 Rust `size_of` 和目录 inode 号推导磁盘偏移，合法 Linux root 也无法读取；count/limit、depth、entry order、logical block range、cycle 与 checksum 未形成统一 checked path | hash version/depth 只来自 root block；signed/unsigned hash 与 Linux 7.1 一致并返回 major/minor typed result；root/internal block 按固定 wire offset、动态 block size和 dx tail 校验；坏 index 只能走 Linux 的 `ERR_BAD_DX_DIR` linear fallback，I/O/checksum 错误必须保留 typed cause | directory mapping/codec | 进行中：`debugfs dx_hash` 的 default seed、UUID seed、UTF-8 signed/unsigned 六版本向量固定了旧算法红测；同一测试现全部通过。SIPHASH wire version 可被 checked parser 识别，但 fscrypt/casefold prepared-name 与 key hash 尚未实现，相关 incompat feature 在可写 mount negotiation 即被拒绝，不能把格式识别误报为算法支持。合法 4 KiB root 红测证明旧 parser 返回 corruption；当前 root/internal parser 校验 dot/dotdot、reserved/info/version/flags/depth、metadata-csum tail limit、count、排序、Linux 28-bit block 与重复 path，并解码 64 KiB index fake-dirent 的 compact `rec_len`。lookup 采用 root version 和 superblock signedness policy，index/leaf checksum failure 不再降级；mount negotiation 拒绝 default hash 6 或更大值，RW indexed mount 在 policy flags 均为空时持久化 reference architecture 的 signed policy，避免 core 语义依赖 OS/compiler plain-char signedness。collision continuation 按 low-bit 边界推进 frame，覆盖同一 index leaf、跨 parent index、真实 I/O 传播与准确 dirent byte offset；完整 probe 未找到不再触发全目录 linear scan。写侧已完成单块 linear→HTree conversion、existing-leaf insert、按 Linux 记录长度平衡的 leaf split、collision continuation separator、root promotion 和通用多级 internal growth/split planner；planner 从 leaf parent 向 root 寻找首个有容量的祖先，全部满时提升 root，separator 只保留在 parent。Linux `e2fsck -D` fixture 经 9000 项长名称增长后报告 `Indirect levels: 1`，重挂载与 `e2fsck -fn` clean。post-write fault 分别证明 conversion 与 split 的 inode/data/bitmap/index 更新整体回滚。indexed delete/rename 只压缩 leaf dirent 并保持 index 高度与分配，符合 Linux 不做 deletion rebalance 的状态机。HTree readdir 现只遍历 checked leaf，按完整 hash 排序并以 typed cursor 保存 collision ordinal；64-bit Linux cookie/EOF 与外部 seek reset 已贯通 VFS、Starry 和 ArceOS。目录 `i_size_high` 现按 Linux 仅在 `LARGEDIR` 启用时参与解码；当前 writable feature mask 仍拒绝 `LARGEDIR`，因此 feature-enabled 的二级 internal split、真实多级 image differential 与 rollback/credit matrix 仍为红项。casefold/fscrypt name preparation 仍为红项。Linux v7.1 依据为 `fs/ext4/dir.c:346-410,526-637`、`fs/ext4/hash.c:1-322`、`fs/ext4/namei.c:537-540,771-1030,1280-1359,1843-2032,2209-2343,2473-2746`、`fs/ext4/super.c:5230-5271`、`fs/ext4/ext4.h:2483-2525,2635-2650,3413-3429` |
| `linux-default-rocompat-rw` | Linux mkfs 默认设置 `HUGE_FILE`、`DIR_NLINK` | 完整读写语义后纳入 writable mask | inode/namespace lifecycle | 绿：`HUGE_FILE` 统一按 Linux 的 32-bit sector、48-bit sector、filesystem-block 三级 codec 读写，所有 block accounting mutation 使用 checked 状态转换；`DIR_NLINK` 覆盖 65000 到 sentinel 1、连续 mutation 保持 sentinel、无 feature 时分配前返回 `EMLINK`；Linux 默认 feature 的 1/2/4 KiB round-trip、extent/JBD2 replay 与 `e2fsck -fn` 全部通过 |
| `journal-no-direct-fallback` | uninitialized JBD2 performs home write | typed journal-aborted error | JBD2 rewrite | 绿：确定性红绿回归已覆盖 write/umount |
| `jbd2-handle-credits` | metadata queue 满时会在 bulk mutation 中间自动提交，失败后 pending image 无法恢复 | operation handle 预留 credits，禁止 operation 内切 transaction，并在 operation error 时恢复 running queue | JBD2 rewrite | 进行中：私有 handle 按 distinct metadata block 计 credit，handle 内禁止 auto-commit，credit overrun/error 恢复 queue snapshot；journal-disabled handle 也保存并逆序恢复 physical preimage。单 transaction 上限现按 Linux 的 `j_total_len / 3` 再扣 descriptor/commit bookkeeping，首次 dirty 前必须先回收出完整 `j_max_transaction_buffers` 空间；best-effort extend 只检查 running transaction 上限，不等待 log space，失败保持原 reservation 并显式返回 restart-required。descriptor continuation、running/committed/checkpoint owner、durable tail 与环绕写入已有确定性覆盖。nested same-owner start 已按 Linux `h_ref` 语义复用 outer handle 与既有 credit budget；nested error 只恢复该 scope 的 queue/revoke/touched snapshot，outer owner 继续有效。revoke record 已拆为 requested/remaining 独立预算，handle start/extend 仅按 revoke-block ceil 与跨 descriptor 边界的差额占用 buffer credits；未申请或超额 revoke 会在发布前返回 typed `NoSpace`，nested rollback 同时恢复 revoke table 与 remaining credits。reserved handle 由 journal-owned ledger 与 non-copy typed ID 表达，单项和全局 reservation 受半 transaction 上限约束；ordinary start/raw metadata 会保留 detached credits，`start_reserved` 消费 token 后不 commit/checkpoint。首个真实 owner已迁移 unwritten extent 的 prepare→data I/O→conversion。通用 scope-boundary `restart_transaction` 现先提交旧 transaction，再将下一 filesystem step 附着到新 transaction，并保持 detached reserved owner；extent 与 legacy truncate/reap/punch 已作为真实调用方迁移。commit owner 现显式执行受检 `Running → Locked → Switch`，active scoped handle 在进入 Locked 前返回 typed `Busy`，旧 owner 到达 Switch 后才转移给 committing transaction，新 running owner 再回到 Running。metadata mutation 已从可泄漏的 `read_block → buffer_mut → write_block` 三段式迁移为 closure-owned `update_block`；closure/write failure 丢弃未发布 image，commit/checkpoint 在 phase 变化或 home write 前拒绝任何遗留 dirty edit，cache refresh 只 discard clean derived image。已迁移 xattr、namespace、rename、preallocation、shift、range removal。大 shift、其他 extent split/merge、`journal_lock_updates` 特殊操作 barrier 与跨执行流并发 handle 仍是红项。Linux v7.1 依据为 `fs/jbd2/transaction.c:184-907,1883-2025`、`fs/jbd2/journal.c:1397-1452`、`fs/jbd2/commit.c:466-605,631-738`、`fs/jbd2/checkpoint.c:126-353,559-729`、`fs/jbd2/revoke.c:300-721`、`fs/jbd2/recovery.c:198-761` |
| `inode-allocator-reserved-range` | `s_first_ino - 1` 被当成每个 block group 的 bitmap 起点，非首组前若干合法 inode 永远不会被分配 | 仅 group 0 跳过全局 reserved inode；其余组从 relative index 0 扫描 | allocator service | 绿：全空 group 1、16 inodes/group、`s_first_ino=11` 的确定性红测证明旧实现返回 relative index 10/global inode 27；同一测试现返回 relative index 0/global inode 17。bitmap publication、group/super free counter、`itable_unused` 与 rollback owner 未改变。Linux v7.1 依据为 `fs/ext4/ialloc.c:725-735,1073-1083` |
| `jbd2-writer-revoke-checkpoint` | commit 同步覆盖 home block，detach 只删除当前 pending image；较早 committed metadata 可在 block 复用后覆盖新 owner | running、committed 与 checkpoint owner 分离；writer 生成 Linux revoke；descriptor/payload preflush 后 FUA commit，home write durable 后 FUA tail | JBD2 lifecycle/revoke | 进行中（writer revoke、bounded lifecycle 与 tail reclamation 子路径已绿）：commit 不再同步 checkpoint，committed image 在 owner 内可见；csum-v3/64-bit revoke 与三阶段 replay 保护 block reuse。checkpoint 反向扫描选定前缀，同一 home block 只写最新可见 image；一次 home flush 后以一次 FUA 发布新 tail。tail FUA 失败时恢复内存 superblock 且不 drain queue，部分 checkpoint 和 ring wrap 后剩余 transaction 可由 replay 恢复。当前仍是同步单 owner，独立 committing transaction、并发 handle、external journal 与完整 persistence-boundary fault matrix 仍为红项。Linux v7.1 依据为 `fs/jbd2/commit.c:114-175,538-605`、`fs/jbd2/checkpoint.c:126-353,559-729`、`fs/jbd2/revoke.c:300-721`、`fs/jbd2/journal.c:1056-1091` |
| `jbd2-abort-sticky` | descriptor/payload flush 失败只返回一次 I/O error，随后仍可从已推进的 ring cursor 重试、继续 metadata write 或关闭 journal 绕过错误 | 首次提交/恢复错误保留原始 cause；同一 mount 的后续 mutation、handle、flush、unmount 全部稳定拒绝，并持久化 journal errno | JBD2 rewrite | 进行中：所有 auto-commit、handle precommit 与 unmount commit 已收口到单一 transaction owner；任一 commit/persistence failure 锁存首个 cause，本次返回原始 typed error，后续 write/handle/flush/unmount/reinstall 返回 `JournalAborted`。未发布 mutable cache image 属于 operation ownership 错误，在任何 journal I/O 前返回 typed `Busy`，不会伪装成 persistence abort。journal mode 切换改为 fallible state transition：abort 时拒绝，pending queue 或 active handle 时返回 busy，不能再关闭 journal 后绕过未提交 metadata。replay 现在以 typed `JournalReplayPhase` 区分 initialize/scan/revoke/replay/persist，保留 I/O、checksum 与 corruption 原始 domain cause、事务 restart 位置和 progress 持久化次错；mount 返回首错并通过 `Observer` 发送完整 typed failure，不再统一伪装为 corruption，越界 `s_start` 也不再清日志报成功。descriptor read 确定性红测已在旧实现证明 `Corrupted != Io`，payload read、home write、checksum+flush 首错优先、final flush 与 replay superblock write fault 均有定点测试。首次 abort 同时以私有 JBD2 wire code 持久化 `s_errno`，重新计算 checksum，并通过原生 FUA 或明确的 write-then-flush fallback 等待 durability；两种能力都缺失时返回 unsupported，record 失败单独保存且不覆盖首次 cause。当前 single-payload transaction 的 open-superblock、descriptor、payload、commit、checkpoint、close-superblock 六次 write 与四个 flush barrier 已逐项注入并验证 sticky first-error。recovery 已拆为不写 home block 的完整 committed-range scan、按 transaction ID 建表的 revoke pass、以及 sequence-aware replay pass；`T1 payload + T2 revoke` 的确定性红测证明旧 transaction-local set 会错误覆盖 home block，同一测试现保留旧值，反向 `T1 revoke + T2 payload` 与 `u32` TID wrap 比较也已覆盖。精细 on-disk error mapping、scan/pass-end 与 fast-commit 一致性、`ACK_ERR`/shutdown、ext4 `continue`/`remount-ro` policy，以及 multi-payload checkpoint/revoke 的完整 fault matrix 仍为红项 |
| `jbd2-csum-v3-write-replay` | writer emits legacy tags while accepted CSUM_V3/64BIT journals require tag3/high block numbers | Linux-compatible descriptor tags and checksum followed by self/Linux replay | JBD2 rewrite | 绿：writer 生成 tag3/64-bit block number、escaped payload CRC32C、descriptor/commit checksum；replay 在任何 home write 前校验 commit 与全部非 revoke payload，并校验 descriptor/revoke tail；Linux `debugfs` 多块事务与逐边界损坏测试通过；mkfs 将 ext4 `metadata_csum`/`64bit` 映射为对应 JBD2 feature |
| `jbd2-partial-commit-replay` | replay 只接受完整 commit block CRC32C，拒绝 Linux 会按已持久化 commit header 接受的零尾 partial-write 事务 | CSUM_V2/V3 完整校验失败后，以 60-byte wire header 和全零 block tail 重算；匹配则仍作为 committed transaction 回放 | JBD2 checksum/recovery | 绿：确定性用例只污染 commit header 后第一个 tail byte，旧实现返回 `ChecksumMismatch` 且不写 home block；同一测试现完成 payload replay。完整 checksum 匹配时通过短路保持单次 CRC，COMPAT/无 checksum 模式不进入回退。Linux v7.1 依据为 `include/linux/jbd2.h:167-177`、`fs/jbd2/recovery.c:431-468,820-878` |
| `jbd2-stale-checksum-tail` | PASS_SCAN 遇到 descriptor/revoke/commit checksum 失败时立即 abort，无法区分当前 transaction 损坏与 lazy journal initialization 遗留的 stale block；writer 同时把 commit time 固定写 0 | scan 只延迟 block-checksum failure，在结构可解析的 commit block 上按 `commit_time < last_commit_time` 识别 stale tail，相等或递增仍拒绝；真实 writer 由 filesystem clock 写入 seconds/nanoseconds | JBD2 commit/recovery | 绿：CSUM_V3 两个 transaction 的 descriptor、commit 和 revoke 三种定点损坏矩阵均证明 10→9 只回放前一个 transaction；旧实现均返回 incomplete，当前实现正常结束 scan。显式 10→10/11 三类损坏矩阵均保持 `ChecksumMismatch`。32-byte block 改为 typed corruption；纯 header+零尾 descriptor 明确 clean-end，已有 tag 却无 `LAST_TAG` 则拒绝，不再 panic/接受相邻伪 commit。注入时钟用例证明非空 commit 的 `h_commit_sec/h_commit_nsec` 精确写入，负秒/越界纳秒在 owner switch 前返回 `InvalidInput`。Linux v7.1 依据为 `fs/jbd2/commit.c:114-144`、`fs/jbd2/recovery.c:588-645,703-721,794-904` |
| `jbd2-legacy-checksum-write-replay` | validator 拒绝 `FEATURE_COMPAT_CHECKSUM`/`CSUM_V2`，非 CSUM_V3 writer 把 tag/commit checksum 写成零，replay 也不校验旧格式 transaction | checksum mode 必须互斥协商；compat checksum 使用 descriptor+payload 的 raw CRC32-BE；CSUM_V2 使用 10/14-byte tag、低 16-bit payload CRC32C 及 descriptor/revoke/commit block checksum | JBD2 codec/commit/recovery | 绿：私有 typed mode 统一 `None`/`CompatChecksum`/`CsumV2`/`CsumV3`，拒绝混合 feature 与 checksum type 错配；writer/replay 覆盖 compat aggregate、CSUM_V2 32/64-bit tag padding、descriptor/revoke tail、commit 与 payload corruption。e2fsprogs `journal_open -c -v 2` 生成的多块 compat transaction 已由 Linux/debugfs 与 rsext4 分别 replay，rsext4 结果通过 `e2fsck -fn`；现代 e2fsprogs 不直接生成 legacy CSUM_V2，因此该模式由 Linux 7.1 源码布局和独立合成 corruption vectors 覆盖。external journal、async commit 与 fast commit 仍为红项 |
| `jbd2-superblock-checked-codec` | journal superblock 对短于 1024-byte 的块直接 slice/`unwrap` panic，且错误拒绝 Linux V1 | mount 只使用 checked 1024-byte prefix codec；V1/V2 按版本分别校验，V1 不读取或改写 V2 extension fields | JBD2 codec/mount | 绿：deterministic 编译红测先证明 checked decode/encode 缺失，同一 0/1023-byte matrix 现返回 typed corruption；V1 validator 红测证明旧实现以 `jbd2:superblock_header` 拒绝，现有 validator、真实 mount 与 metadata commit 均忽略 V2 feature/UUID/checksum 尾部，sequence/start 写回保持尾部原值。错误公开名 `JournalSuperBllockS` 已破坏性改为 `JournalSuperBlock`。Linux v7.1 依据为 `include/linux/jbd2.h:226-277,1328-1389`、`fs/jbd2/journal.c:1309-1394,1458-1507` |
| `extent-checked-codec` | raw extent nodes are sorted after parsing and malformed roots/children can be treated as holes | checked structural validation preserves on-disk order and propagates corruption | mapping rewrite | 绿：root/child codec 检查 magic、Linux on-disk depth 上限 5、capacity、非空 index、logical/physical overflow 与 leaf/index ordering；确定性红测证明旧 parser 会接受结构合法但 depth=6 的 index，同一测试现固定 depth=5 接受、6/32/33 拒绝，parse、递归校验与 split/promotion 共用同一上限。`EXT4_EXTENTS_FL` 是唯一格式判据，坏 magic 不再降级为 legacy/hole；读取、查找、插入、删除、HTree 和 block resolver 均传播 typed error，不再排序或吞错；hard-link parent corruption 完成确定性红绿验证。Linux v7.1 依据为 `ext4_extents.h:86-87`、`extents.c:491-494,900-906` |
| `extent-empty-index` | crafted empty or malformed internal child can panic | corruption error, no mutation | mapping rewrite | 绿：root 与 external child 在 mutation 前统一 checked decode；空 index、坏 child 与超过 inline root 容量均返回 corruption，确定性测试验证 inode 不被截断或修改 |
| `inode-checked-codec` | `Ext4Inode::from_disk_bytes` 对小于 128 bytes 的输入直接 slice panic，且非法 `i_extra_isize` 被当作字段不存在 | inode cache 在读取任何字段前检查 fixed record 长度、extra region 边界和 4-byte 对齐，并返回 typed corruption | inode codec | 绿：新增的 deterministic 编译红测先证明 checked decode 边界缺失；同一测试现覆盖 0/127/129-byte record、越界与未对齐 `i_extra_isize`，生产 inode cache 只调用 checked decoder。Linux v7.1 依据为 `fs/ext4/inode.c:5275-5287` |
| `group-desc-checked-codec` | 非 32-byte group descriptor 一律按 64-byte 解码，40/48/56-byte 损坏 geometry 可触发 slice panic；非 64-bit 镜像错误采用磁盘 `s_desc_size`；大于 64-byte 的 Linux 合法 descriptor 校验和与写回丢失扩展尾部 | mount 仅走 checked decoder；非 64-bit 固定 32 byte，64-bit 仅接受 64..1024 的 2 次幂；checksum 覆盖完整 record，写回保留 byte 64 后扩展区 | group descriptor codec/geometry | 绿：deterministic 编译红测先证明 `decode_checked` 缺失；同一 size matrix 现拒绝 0/31/33/63/65/96/2048，接受 32/128。128-byte reserved tail 在 encode/sync 路径保持不变且参与 checksum，单 bit 损坏返回 `ChecksumMismatch`。Linux v7.1 依据为 `fs/ext4/ext4.h:453-456`、`fs/ext4/super.c:3243-3267,5284-5295` |
| `extent-block-checksum` | extent block lookup lacks the inode number required by metadata checksum and assumes the checksum tail is always at the end of the block | every resolver carries typed inode identity and verifies the Linux `eh_max`-derived checksum tail | mapping rewrite | 绿：resolver/HTree/mount/adapter 调用链显式传递 `InodeNumber`；external node 读写按 inode generation/number 校验 CRC32C，2 KiB `eh_max` tail offset 与损坏测试通过 |
| `extent-system-zone-validity` | physical extents are checked only against filesystem/device bounds | reject overlap with ext4 system metadata zones, with Linux's owning-inode exception | mapping rewrite | 绿：mount/replay 后完整构建并一次发布 immutable zone index，覆盖 per-group super/GDT/reserved GDT、bitmap、inode table 与 internal journal blocks；普通 inode 指向 block bitmap 的确定性红绿测试完成，journal inode owner exception 单测通过；first-data、溢出和 filesystem/device 上界继续共同生效 |
| `extent-unwritten-preallocation` | resolver 把 unwritten extent 折叠成 hole，partial write 会另行分配重叠 extent；core 没有预分配 API | 保留 hole/initialized/unwritten 三态；按 Linux `ee_len` 边界编码；data I/O 前拆出精确 unwritten 范围，partial write 全块零化，成功后才转 initialized；普通/KEEP_SIZE 预分配只填 hole | mapping rewrite | 绿（core preallocation）：确定性红测证明旧路径以 `extent:overlap_or_order` 失败；同一测试现保持原物理块、data `i_blocks` 与 free count，仅把中间块转 initialized、左右继续 unwritten，未覆盖字节和未写 extent 均读零。满 inline root 的三段拆分会先扩为 external tree 并计入 metadata blocks；普通与 KEEP_SIZE 预分配支持跨 partial block、跳过既有 mapping、最大 32767-block unwritten run。每个 hole chunk 现在按 Linux `ext4_alloc_file_blocks()` 使用独立、由 geometry 推导的 `ext4_chunk_trans_blocks()` 等价 credits，并在同一 filesystem transaction 中发布 extent tree、inode、allocation bitmap、GDT 与 superblock；满 inline root 分裂后的首次 metadata post-write 故障红测曾稳定泄漏一个未发布 leaf，现重挂载后 mapping、`i_blocks` 和 free count 均恢复。写路径先发布仍为 unwritten 的 split/preallocation，再写 data，最后转 initialized；external-leaf finish 的定点 I/O 失败会恢复 leaf 与 prepared inode，即使关闭 journal，底层已写 payload 仍不可见。truncate 现直接枚举 initialized/unwritten extent，确定性红测证明旧 initialized-only resolver 会泄漏 4 个预分配块，同一测试现恢复 free count 与 `i_blocks`。1/2/4 KiB Linux image 与 4 KiB partial-write image 均通过 umount/remount 和 `e2fsck -fn`。Linux v7.1 依据为 `ext4_extents.h:136-203`、`inode.c:6220-6332`、`extents.c:2390-2460,3790-3891,3992-4054,4574-4845`。extent merge 和 delalloc/writeback adapter 仍为红项 |
| `fallocate-preallocation-full-stack` | Starry `mode=0` 仅用 `set_len` 创建 sparse file，`KEEP_SIZE` 直接返回 `EOPNOTSUPP`；VFS 无预分配边界 | syscall 仅解析 Linux flags/errno/seal；VFS 传递 typed extend/keep-size 语义；ext4 adapter 按 inode number 调用 OS-independent core；cached length 与底层 inode 一致 | VFS/Starry integration | 绿（普通与 KEEP_SIZE）：同一直接 ABI 测试在旧实现稳定失败 3 项（普通分配 `st_blocks=0`、KEEP_SIZE 返回 `EOPNOTSUPP`、KEEP_SIZE `st_blocks=0`）。测试现先用 `statfs` 证明 fixture 为 ext4，再严格要求普通/KEEP_SIZE 都预留至少一个 4 KiB 块，KEEP_SIZE 保持 `st_size=0`；x86_64 Starry QEMU 纳入下列 range case 后总计 121 pass/0 fail。 |
| `fallocate-range-zero-punch` | Starry 用 userspace-visible zero writes 模拟 `ZERO_RANGE`/`PUNCH_HOLE`，不会建立 unwritten extent 或释放完整物理块 | typed range API 贯穿 VFS/cache/ext4；ZERO_RANGE 保留 allocation 并把完整块转 unwritten；PUNCH_HOLE 释放完整块并只清零两侧 partial block；保持 size 与 Linux errno 优先级 | mapping/VFS/Starry integration | 绿（功能与 bounded restart）：core 确定性测试覆盖非对齐边界、allocation/free count、legacy direct→single finite punch 与 unwritten truncate；x86_64 Starry 直接 C ABI 测试在 ext4 上严格检查内容、`st_size`、`st_blocks`，与 collapse/insert case 合计 121 pass/0 fail。extent-backed punch 现先完整枚举 initialized/unwritten mapping，再以一个 filesystem transaction 删除全部 full-block segment；第二次 external-leaf write 定点故障在旧实现留下第一段已释放，同一测试现经重挂载恢复全部 extent、`i_blocks` 与 free count。legacy direct/single/double/triple punch 超出 ring 时按当前 committed tree 分段重新规划，保持 `i_size` 且不建立 Linux 不存在的 orphan/range intent；commit block 落盘后、journal tail 更新前断电可 replay 到一致的部分 punch 状态，重试同一区间可完成剩余工作。两侧 partial block 按 Linux 顺序在 metadata transaction 前清零，之后的 metadata 故障不承诺回滚已完成的数据清零。 |
| `fallocate-range-collapse-insert` | core/VFS 无法表达逻辑区间左移或右移，Starry 对两个 mode 返回 `EOPNOTSUPP`；缓存页在映射移动后可继续代表旧 offset | extent core 保持 initialized/unwritten 物理映射并重建 logical keys；按 cluster 对齐，严格执行 EOF/overflow/mode 规则；VFS 在 sleepable I/O lock 下写回并失效 shift point 后全部缓存页 | mapping/VFS/Starry integration | 绿（功能与单次 shift transaction）：旧实现的 core happy-path 测试稳定返回 `Unsupported`，Starry C ABI 初始为 88 pass/9 fail；同一测试现验证内容左/右移、插入 hole、size、对齐、EOF、KEEP_SIZE 互斥和 fd/range/mode errno 顺序，x86_64 QEMU 为 121 pass/0 fail。block-aligned 但 bigalloc cluster-unaligned 的两个确定性测试曾分别暴露后置 checksum 失败与错误成功，现均在 mutation 前返回 typed `InvalidInput`。Linux image 覆盖 1/2/4 KiB block size、360 个稀疏 initialized extent 形成的多 external leaf、unwritten 状态、umount/remount 和两次 `e2fsck -fn`。page-cache listener 定点插入的 clean-page 竞态证明首次 snapshot 会留下 stale offset，同一测例现要求最终持 `io_lock` 后集合稳定才执行 shift。replacement tree、inode root/size/final `i_blocks`、旧 data/metadata block 释放、受影响 bitmap/GDT 与 superblock 现由一个 geometry-bounded filesystem transaction 共同拥有；新 leaf 首次 post-write 和最终 bitmap publish 两个定点故障都经重挂载保持旧 mapping、inode accounting 与 free count。Linux v7.1 依据为 `open.c:250-338`、`extents.c:4859-4933,5278-5739`，其中 collapse 的单 truncate handle 见 `5561-5606`、insert 的预扩 size 与 shift 见 `5659-5731`、handle restart 见 `5300-5513`。超过当前单 transaction ring capacity 的大 shift 仍需实现 Linux 式 restart，继续由 `jbd2-handle-credits` 跟踪。 |
| `fiemap-full-stack` | core/VFS 没有稳定的 inode mapping inspection DTO，Starry 对 `FS_IOC_FIEMAP` 返回 `ENOTTY`，且最初误把 ext4 目录 FIEMAP 当成 unsupported | core 以 byte-addressed typed target/state DTO 枚举 extent 与 legacy mapping；VFS 同时为 file/dir 转发；Starry 精确实现 header/extent ABI、flags、count-only、range、LAST 与 errno 顺序 | mapping/VFS/Starry integration | 进行中：确定性 core 红测证明旧实现返回 `Unsupported`；目录 ABI 断言改为 Linux 语义后，旧链路在 x86_64 QEMU 稳定以 `EOPNOTSUPP` 失败。data mapping 已覆盖 sparse hole、initialized/unwritten、legacy `MERGED`、bounded/count-only、非对齐查询保留完整 extent、regular file 与 directory、1/2/4 KiB Linux image、remount 和两轮 `e2fsck -fn`。`FIEMAP_FLAG_XATTR` 的确定性 Linux-image 红测在旧实现稳定返回 `Unsupported`；同一测试现覆盖 inline inode body、external xattr block、无 xattr 空结果、count-only/range、1/2/4 KiB geometry 与两轮 `e2fsck -fn`。inline checked parser 按 Linux 检查 magic、entry/name/value bounds、EA inode feature/inode number 和 value/name overlap；Starry 对 inline mapping 输出 `DATA_INLINE|NOT_ALIGNED`，新增 XATTR ABI 断言后 x86_64 QEMU 整体为 37 pass/0 fail。Linux 7.1 的 inline physical ABI 特意只使用 inode-table block base 加 `128+i_extra_isize`、不加入 inode slot offset，core 保留这一可见语义并由断言固定。`start == maxbytes` 的复核红测证明先前 `>=` 检查错误返回 `FileTooLarge`；同一断言现要求等于上限成功返回空映射、仅大于上限失败，并继续按 `ext4_max_size`/`ext4_max_bitmap_size` 纳入 i_blocks metadata overhead 与 `HUGE_FILE` 限制。Linux v7.1 依据为 `super.c:3427-3552`、`ext4.h:3454-3459`、`fs/ioctl.c:186-227`、`fs/iomap/fiemap.c:1-88`、`extents.c:5120-5171,5178-5271`、`xattr.c:180-295`、`inode.c:3860-3905,4873-4888`、`file.c:993-1007`、`namei.c:4225-4243`。file inline-data mapping、delalloc `UNKNOWN|DELALLOC` 与独立 extent-status precache 尚未实现，继续登记为红项。 |
| `inode-inline-xattr-preservation` | inode cache 从结构体写回全新零缓冲，普通 data/metadata mutation 会擦除未建模的 inline xattr 尾部；checksum 也只覆盖结构体字段 | cache 保存并写回完整 raw inode；codec 仅覆盖 `i_extra_isize` 声明存在的字段；checksum 覆盖 raw inode 全部字节 | inode codec/xattr | 绿：Linux-image 确定性红测先证明普通文件写后 `debugfs ea_list` 读不到原 inline xattr；同一测试现覆盖 1/2/4 KiB、unmount 和 `e2fsck -fn`。raw-tail checksum 与小 `i_extra_isize` codec 单测固定未建模区域的保真语义。Linux v7.1 依据为 `fs/ext4/inode.c:60-128` 与 `fs/ext4/xattr.h:65-73`。 |
| `persistent-xattr-inline-external` | core 只能检查 xattr 的 FIEMAP 位置，Starry 把 xattr 放在 `Location::user_data` 的临时 map，重查 dentry、hardlink、copy-up 或重挂载后会丢失 | core 以 inode number 提供 checked get/list/create/replace/remove，完整支持 inline/external block、checksum、refcount COW 与 transaction rollback；VFS 仅声明 inode capability，ext4 adapter 落盘，tmpfs inode 自有内存状态，Starry 只负责 Linux ABI/errno/namespace | xattr core/VFS/Starry/JBD2 | 进行中：1/2/4 KiB Linux image 已覆盖 Linux/debugfs 创建的 inline、external 与 absent store，typed CREATE/REPLACE、inline→external→inline、free-block accounting、remount、`debugfs ea_list` 和 `e2fsck -fn` 均通过；VFS/ax-fs-ng 已用 `XattrOps` 取代 dentry side store，Starry x86_64 QEMU xattr case 从 38 pass/45 fail 转为 89 pass/0 fail，覆盖 path/fd、short buffer、hardlink 与 symlink nofollow。overlay read-only/missing-remove 的无副作用红测从 116 pass/3 fail 转为 119 pass/0 fail。当前 local-value external block 的 Linux hash/checksum/refcount COW 已实现；单属性先尝试 inode body、ENOSPC 后只把该属性放入 external block，反向缩小时也只迁回该属性，无关 sibling 保持原 store。filesystem-owned metadata transaction 现为 xattr 同时拥有 superblock/GDT/bitmap/inode cache undo，并在成功返回前把 inode、受影响 bitmap/GDT 与 superblock 定向加入同一 bounded handle；关闭 journal 的 inode-table 定点写失败红测证明旧 cache 泄漏新 inline xattr，同一测例现恢复完整 raw inode。shared-block fixture 现固定两个 inode/refcount=2，验证成功 COW 保留另一 inode；无关 inline update 不再复制或重写 unchanged shared external block。journal credit failure 与 no-journal old-refcount write failure 均经重挂载保持两个旧值、refcount=2、inode 指针与 free count。final reap 现在验证 external block header/checksum/refcount，refcount=2→1 时保留另一 inode，refcount=1 时 revoke/free；bitmap publish 故障后 retry 回归验证 orphan、`i_file_acl`、xattr value 与 free count 全部恢复。EA inode value、ACL/security/trusted policy 和 external deletion 的断电 replay 矩阵仍为红项，不能宣称完整 xattr parity。Linux v7.1 依据为 `fs/ext4/xattr.c:132-300,939-959,1271-1363,1629-2226,2337-2498,2906-3014,3141-3210`、`fs/ext4/xattr.h:30-73` 与 `fs/overlayfs/xattrs.c:35-77`。 |
| `mount-option-block-validity` | core did not protect system metadata blocks | default `block_validity` plus Linux-compatible `noblock_validity` mount/remount lifecycle | mount/remount options | 绿：RW/RO mount 默认建立 layout + internal-journal owner system-zone index，`with_block_validity(false)` 在 initial mount 与 replay reload 保持空索引；owned `remount` 禁用时释放，重新启用时先完整构建后一次发布。crafted block-bitmap extent 确定性红测证明仅修改 option 但不释放 index 仍拒绝；同一测试现要求 disable 允许、reenable 再拒绝，extent 与 legacy indirect 共用同一 index |
| `mmp-readonly-mount` | known MMP incompat 在 Linux 允许的只读 inspection mount 也被无条件拒绝 | 只读 mount 完全跳过 MMP block I/O；可写 mount 必须在任何其他 mutation 前 claim owner，周期 refresh，并在 RW→RO/unmount 的 ext4/JBD2 clean 持久化后发布 MMP clean | mount feature negotiation / MMP lifecycle | 进行中（portable core 与 adapter lifecycle 绿）：feature 单测在旧实现稳定返回 `UnsupportedFeature(bits=0x100)`；当前 Linux `mkfs.ext4 -O mmp` 镜像可 RO/no-replay mount 并读取根 inode，卸载后 64 MiB 镜像逐字节不变。注入确定性 entropy/delay 后，同一镜像完成 magic/checksum 校验、随机 sequence claim、稳定性复查、refresh 与 clean unmount，写序确认 MMP clean 是最后一次 metadata write，最终 `e2fsck -fn` clean；缺少 capability 时初始 RW 与 RO→RW 都在 mutation 前返回 typed `UnsupportedCapability`并保留 options。当前 ArceOS 没有可信 entropy provider，因此其 writable MMP 保持 `EOPNOTSUPP`；平台 RNG、真实多主机互斥和完整断电故障矩阵仍为红项 |
| `mount-remount-full` | mount options 仅有 readonly/replay，remount 无统一 state transition | 完整对齐 Linux ext4 mount/remount options，ro↔rw、journal/data mode、barrier/discard/error policy 与失败回滚 | mount/remount/JBD2 options | 进行中：owned core 已实现 RW→RO 的 pending metadata sync/journal checkpoint/clean-superblock 后发布，以及 RO→RW 的 device-readonly、writable feature、recovery/orphan、journal-state 预检和 dirty/recovery superblock 持久化后发布；replay policy 仍是 mount-time immutable option。确定性红测证明旧实现无条件返回 `remount:mode`，同一测试现完成 RW→RO mutation gate 与 RO→RW 恢复；one-shot flush fault 保持旧 options，物理只读设备返回 typed `ReadOnly`，read-only unmount 不再向设备写入且已卸载 owner 拒绝原地 remount。MMP 初始 RW 与 RO→RW 现先 claim owner；RW→RO 与 unmount 保持 owner 到 ext4/JBD2 clean 持久化完成后才发布 MMP clean。clean release 失败的 remount 会重新 claim 并恢复 RW 持久状态，无法恢复则锁存 failed state；refresh I/O 失败同样拒绝后续 mutation。ax-fs-ng 的只读 sync/shutdown 现也在 sleepable mutex 下调用同一 owned core，MMP worker 在锁外等待、仅在 refresh 时取得独占 owner；adapter 的 read-only 查询也直接读取 core options，不再维护第二状态源，但 VFS 仍未提供 remount 入口。journal/data mode、barrier/discard/error policy、quota、平台 entropy 与更完整的磁盘副作用回滚仍为红项，不能声称 Linux remount 完整性 |
| `extent-mutation-rollback` | split/remove/rebuild 的 metadata write 或 bitmap I/O 失败可留下泄漏、部分释放或不可达节点 | plan/validate/journal persist，任一失败保持旧树与 bitmap/i_blocks 一致 | mapping rewrite | 红（preallocation、shift、单 ring range-remove、leaf insertion normalization/merge-up 与 restartable legacy truncate/reap/punch 子路径已绿）：HUGE_FILE checked accounting 已在分配/释放前预检；preallocation 的每个 extent insertion chunk 现由 filesystem transaction 完整拥有，新 metadata 构建与 bitmap/GDT/superblock/inode 发布任一步失败都会共同撤销。collapse/insert 先只读规划，再在一个 transaction 内保留旧树、构建 replacement、共同发布 root/size/final `i_blocks`、释放旧 data/metadata block，并显式刷新新旧 block 所在 group；不能仅凭最终 free counter 判断 bitmap dirty，因为同组 alloc/free 可能抵消。replacement node post-write 与 block-bitmap publish 的确定性红测曾分别留下未发布 leaf 或部分 allocator 状态，同一测试现经重挂载验证 mapping、free count 与 inode accounting 完整恢复。leaf insertion 依照 `extents.c:1786-1932` 先尝试左邻，再持续合并右邻；initialized/unwritten 状态、逻辑/物理连续性或 wire length 上限任一不满足时保持两个完整 extent，不能把左侧填满后制造 Linux 不会生成的 tail。punch/truncate 也先读取完整 external tree 与 initialized/unwritten extent 集合，按所有旧节点、涉及 allocation group、inode 和 superblock 预留 credits，再在同一 transaction 中逐段删除并最终发布 inode、bitmap/GDT 与 superblock；第二次 leaf write 故障的 punch/truncate 红测现均恢复全部 mapping、size、`i_blocks` 与 free count。被移除的空 extent 或 legacy indirect metadata block 现在与 detach mutation 位于同一 handle，并生成 writer revoke；稍后 transaction 的 revoke 会抑制较早 committed image 的 replay/checkpoint，同 transaction metadata reuse 则取消 revoke。其他尚未迁移的 extent split/merge fault matrix 与独立 committing owner 仍未完成，故本总项继续保持红色。Linux split failure 恢复原 extent 依据为 `extents.c:3226-3302`；punch/truncate 事务与 restart 依据为 `inode.c:4255-4533,4566-4697`、`extents.c:2701-2728,2837-3090`；revoke/checkpoint 依据为 `fs/jbd2/revoke.c:300-660`、`fs/jbd2/checkpoint.c:126-353`。 |
| `mkdir-mutation-rollback` | child inode 初始化后，父 link/group accounting 或目录项插入失败可留下孤儿 inode、泄漏块或部分发布的计数 | mkdir 的 inode、block、父目录项、父 link count 与 group stats 属于同一可回滚 transaction | namespace/JBD2 rewrite | 绿：link 上限在分配前预检；39-credit namespace transaction 共同恢复 child inode/block、parent dentry/inode、allocation bitmap/GDT/superblock 与 directory count。ENOSPC 和目录块写后 I/O 两类确定性红测分别覆盖分配失败与持久化失败，均保持重挂载状态不变 |
| `rename-mutation-rollback` | 跨父目录 rename 在新项、旧项、父 link count 或 `..` 更新任一步失败时可留下部分状态 | rename 的全部目录项、link count、`..` 与被替换 inode 更新崩溃原子且可回滚 | namespace/JBD2 rewrite | 绿（已实现 flags）：查找、same-inode/no-replace/type/ancestry/link-count/free preflight 在独占 `&mut` owner 下完成，真正 mutation 由 62/74-credit filesystem transaction 统一拥有。旧目录块写后故障红测证明旧实现返 I/O error 后仍丢失源名；同一测试现重挂载恢复源名/内容并移除目标名。exchange 第二侧目录块、跨父目录 `..` 块、replacement inode-table 三个附加故障点分别验证两侧名称、父 nlink/`..`、target nlink/orphan head 全部恢复。手工局部 rollback 已删除。`WHITEOUT` 尚未进入 mutation，因此继续由 `typed-rename-flags` 红项追踪。Linux v7.1 依据为 `fs/ext4/namei.c:3765-4195`、`fs/ext4/ext4_jbd2.h:21-50,78-90` |
| `io-failure-no-panic` | mount/commit paths contain `expect` | all errors propagated | codec/JBD2 rewrite | 进行中：mount/JBD2 与 extent root/child traversal 已移除 panic/静默失败；inode allocation bitmap 查询由吞掉 I/O/corruption 并返回 `false` 改为 `Ext4Result<bool>`，共享故障开关确定性红测已证明旧实现将 read failure 报作 free，同一测试现保留原始 `Io`。缓存中的四处 `unwrap` 已逐控制流复核，均由 non-empty cache-line 不变量支配，不属于可达错误；其余生产路径仍待继续审计 |
| `legacy-indirect-13-blocks` | non-extent path is unsupported | Linux-compatible mapping | mapping rewrite | 进行中：checked read 与 allocate-before-publish write 已覆盖 direct/single/double/triple、hole、整块 pointer validity、system zone、cycle、data+metadata `i_blocks` 与运行时失败反向 rollback；跨 direct/single 的 Linux image 已通过 umount、e2fsck、remount/read、再次 e2fsck。full ownership preflight 不受 `i_size` 裁剪，完整收集 data 与 child-first metadata，拒绝跨树重复物理块和隐藏损坏。recursive shrink 先做完整 ownership preflight，再仅沿 cutoff 路径与右侧子树自底向上、从右向左规划 pointer edit 和 data/metadata free；inode image 先移除映射并重算 `i_blocks`，随后才把块归还 allocator。确定性红测已覆盖 EOF 外 hidden single tree、single/double/triple partial leaf、double/triple 子树边界和 full-root 回收；inode finalize 定点 I/O 失败会恢复 pointer block、inode image 与 free-count。4 KiB Linux image 现以稀疏 marker 分别建立 single/double/triple 根（triple EOF 约 4 GiB），裁剪完整根后验证 `i_blocks`、重挂载内容并两次通过 `e2fsck -fn`。final unlink 只改变 dentry/nlink/orphan，显式 reap 复用强制 mapping cleanup；data、pointer metadata、orphan 和 inode bitmap 的成功路径均有 unit 与 Linux image recovery/e2fsck 回归。truncate grow 对 extent/legacy 都只发布 sparse `i_size`，旧 partial EOF 在 grow 前清零，1/2/4 KiB Linux image 既有回归保持通过。单 ring 容量内的 punch/truncate 已由同一个 filesystem transaction 发布 pointer、inode、allocator 与计数；超容量 truncate/reap/punch 已按 allocation group 和连续 logical run 分段 restart，并覆盖 sparse direct/single/double/triple。punch 保持 Linux 的非 orphan 语义，每个 committed chunk 自洽，崩溃后可从当前树重试而不会自动补完操作 |
| `legacy-indirect-truncate-atomicity` | pointer/inode/bitmap/free 的任一后置 I/O 失败可能留下部分裁剪、泄漏或 accounting 不一致 | 一个 filesystem-owned journal handle 同时拥有 pointer、inode、bitmap、group/super counters 与完整 undo；replay 后只出现旧树或新树 | mapping/JBD2 rewrite | 进行中（bounded transaction、超 ring truncate/reap/punch restart 与 writer revoke 子路径已绿）：完整 ownership plan 先收集 pointer home block 和 allocation group，以 `pointer edits + detached metadata revokes + 2 * groups + inode + superblock` 预留 distinct credits；已经作为 pointer edit 写入的同一 metadata block 不重复计 revoke。完整计划超出 ring 时，先验证最小 chunk 可容纳，再由 UserResize transaction 原子发布目标 `i_size` 与 classic orphan。后续每个 chunk 从当前已提交 pointer tree 重新进行不受 `i_size` 限制的 ownership scan，只在 commit 成功后推进内存 cursor；崩溃不依赖持久化 cursor，只依赖 orphan、目标 size 与当前 committed tree。sparse direct/single/double/triple fixture 同时覆盖 logical hole、两个 allocation group和多个 commit；首个 chunk commit block 落盘后、journal tail 更新前断电的 truncate 测试经 replay + orphan recovery 释放全部十个 data/metadata block。punch 复用同一 child-first chunk 与 writer revoke，但按 Linux `ext4_punch_hole()` 保持 `i_size` 且不加 orphan；相同 commit boundary 断电只 replay 已 durable chunk，重试同一区间完成剩余树。zero-link reap 保持 orphan，分段清空 mapping 后再以精确 5-credit transaction 回收非头 orphan、inode bitmap/GDT/superblock；此最终 transaction 已无待 detach 的 mapping metadata，因此仍为 5 credits。单 chunk 仍无法容纳时在 partial EOF 清零、size/orphan publication 和 commit 前返回 `NoSpace`。已脱链 pointer metadata 现在在同一 handle 记录 writer revoke，较早 committed image 不会在 allocator reuse 后通过 replay 或 checkpoint 覆盖新 owner。完整 persistence-boundary fault matrix 与独立 committing owner 仍未实现，故本总项不能转绿。Linux v7.1 依据为 `fs/ext4/indirect.c:689-746,724-748,857-985,1000-1110,1112-1215,1225-1419`、`fs/ext4/inode.c:4427-4543,4567-4693`、`fs/ext4/orphan.c:90-187,321-376`、`fs/jbd2/revoke.c:300-660` |

2026-08-12 的 extent range-removal restart 检查点将表中“超过 ring capacity 的
extent handle restart”子项推进为绿色：完整计划超过 JBD2 capacity 时，punch 与
truncate 按 allocation-group 边界建立 bounded transaction；每段成功后才推进
运行时逻辑 cursor，下一段重新读取已提交 extent tree，不复用已经失效的 path。
用户 truncate 在首段前先持久化新 `i_size` 与 classic orphan intent，全部段完成后
才摘链。这个 cursor 不是新增磁盘字段；崩溃恢复只依赖新 `i_size`、orphan 链和
已提交的 extent tree。`extent_restart` 的 bounded-credit 测试证明一次操作产生多个 commit；
第一个 removal chunk 的 commit record 已写入而 journal tail 尚未更新时断电，重挂载通过 replay
与 orphan recovery 收敛，并逐块验证 data/extent metadata 已释放、刻意保留的 gap
仍分配、`i_blocks`、size 和 free count 一致。另一个 capacity 小于单 chunk credit
的确定性红测曾证明旧 restart 先把 size/orphan 留在当前 mount 再返回 `NoSpace`；
现在最小 chunk credit 会在任何 partial-block 清零或 truncate intent 发布前预检，
同一测例确认 inode、orphan head 均未变化且没有写出 commit record。上述总项仍保持红色，因为大 shift、
其他 extent split/merge、独立 committing owner 与完整 fault matrix 尚未完成；不能把本检查点
扩大解释为完整 JBD2 或 mapping crash parity。

同日的 legacy indirect restart 检查点复用同一 durable truncate intent，但不复用
extent cursor。Core 在每个 chunk 前重新枚举 inode 当前拥有的全部 legacy 映射；该
枚举刻意不按已经持久化的新 `i_size` 截断。chunk 从最高 logical block 向左收敛，
只合并同 allocation group 的连续映射，并按完整 plan 的 pointer edits、未被 edit
覆盖的 detached-metadata revoke、bitmap/GDT、inode 与 superblock footprint 预留
credits。`extent_restart` 中的多条专项回归分别
验证：跨 direct/single/double/triple 与 logical hole 的多 transaction 成功路径；
首个 chunk commit 后、journal tail 更新前掉电的 replay + orphan recovery；zero-link
非头 orphan 的 mapping/final-inode 两阶段 reap，以及 mapping 已清空后不再需要 revoke
credit 的精确 5-credit predecessor update。
旧实现的首个确定性红结果为 `indirect:transaction_credits`；初版 restart 又因目标
size 已发布后复用 EOF-limited resolver 而错误保留全部 pointer，现由独立 ownership
scanner 修复。writer revoke 已补齐 detach/reuse 的磁盘记录与 replay/checkpoint 过滤；
完整 fault matrix 与独立 committing owner 仍未完成，因此相关总项继续保持红色。

专项 fixture 还在 double-indirect root 下放置一个不含 data pointer 的 allocated leaf。
初版 restart 只按 data mapping 选择 cursor，因而该空 branch 会让最小 chunk 错误报
`indirect:restart_credits`；确定性红测随后要求 chunk 同时覆盖已扫描到上一个 cursor
之间的 logical gap，并在 data cursor 耗尽后提交仍有 metadata removal 的尾段。当前
测试会逐层读取四个 marker、验证 logical hole 为零，并在最终状态逐块检查空 leaf
与所有 data/pointer block 已释放，避免仅清空 inode root 的实现假绿。

2026-08-12 的后续 JBD2 tail 检查点已完成上文各总项中曾登记的
“逐 transaction tail 回收未完成”子项，以下语义均有确定性测试：
最老 committed transaction 前缀的 partial checkpoint、剩余 transaction replay、
ring wrap 后重用、tail FUA 失败不前进 replay boundary、flush 合并多个
transaction 的 home write 与单次 tail FUA，以及同 home block 只写最新可见
image。上文历史检查点中的旧表述仅保留为时序记录，不再是当前
红项；各总项仍因独立 committing owner、并发 handle、external journal 与完整
persistence-boundary fault matrix 保持红色。

Draft 期间这些测试可以保持失败，但测试本身不得 `ignore`、弱化断言或伪造成功。
PR 转 Ready 前本表必须为空。

Linux image create/write/rename/e2fsck 回归已移除唯一的 `#[ignore]` 与默认镜像缺失时的
静默 skip。未设置 `RSEXT4_TEST_IMAGE` 时由固定参数的 host `mkfs.ext4` 创建 64 MiB、
4 KiB block fixture；显式 fixture 仍复制后执行。`mkfs.ext4`、`debugfs`、`e2fsck` 或
`truncate` 缺失会直接使测试失败，不能把未验证当作通过。

当前 core 已完成 Linux HTree hash、checked lookup、linear→indexed conversion、existing-leaf
insert、leaf split、root promotion 与通用多级 internal growth/split planner。indexed mutation 不再清除 `EXT4_INDEX_FL`；伪造 flag 而
没有合法 root 的目录按 corruption 拒绝，不再把损坏状态静默降级。`DIR_NLINK` sentinel 1
继续按 Linux `is_dx()` 与 `ext4_inc_count()` 保持。Linux 删除 indexed dirent 不重平衡或回收
leaf；core 已对齐该 leaf-only mutation。目录 size 解码也已遵循 Linux `ext4_isize()`：未启用
`LARGEDIR` 时忽略非 regular inode 的 `i_size_high`。但 writable feature gate、真实二级 internal
image differential、rollback/credit matrix 与 casefold/fscrypt name preparation 仍在红测台账中。

## 7. 性能门槛

第一阶段 host harness 的工作负载和输出格式冻结。相同机器、toolchain、CPU
affinity、镜像与 workload 下，预热 3 次、测量至少 10 次；现有语义的 median
吞吐/IOPS 回退不得超过 5%，p95 latency 回退不得超过 10%。新增语义报告相对
Linux 7.1 的代价，不套用不存在的 dev 对照。

> 开发期原始 CSV 与临时 benchmark harness 已在 PR 收尾时从仓库清理；下文保留
> 固定环境、统计口径、汇总值和性能结论。

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
20 次测量。harness marker 同时补齐
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
的支配，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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
固定且公开的 10-buffer 上限；该检查点当时的 single-descriptor writer 从
filesystem block size、descriptor header、tag/UUID/checksum tail 和 journal ring
geometry 推导每个 transaction 的安全 update 上限。后续 multi-descriptor 改造已
在主追踪表登记；本节只保留该历史性能检查点的原始实现边界与测量数据。

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

本次探索使用 3 次预热与 10 次测量，汇总如下：

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

正式检查点使用 3 次预热与 20 次测量，汇总如下：

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

正式检查使用 3 次预热与 20 次测量，汇总如下：

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

### 7.19 journal flush 与 remount 检查点

采集时间：2026-08-11；被测实现 commit 为
`6374efcebfa2d90a17996807b027877c19275775`，固定 CPU 2，环境、owned API
harness 和 workload 与 7.18 相同。本检查点使 `Jbd2Dev::flush` 强制提交 pending
metadata transaction，并把 owned `sync` 收口到单一 flush/commit 路径，删除一次
空 commit 和多余设备 flush；同时加入 owned RO/RW remount 状态机。remount 不在
该 sequential workload 的计时区间，因此结果只用于验证现有 write/read/sync
工作负载未回退，不能把变化归因于 remount。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=6374efcebfa2d90a17996807b027877c19275775 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6488946 write_p95_ns=7037221 read_median_ns=6203958 read_p95_ns=7585968 sync_median_ns=32273 sync_p95_ns=35688
```

相对 dev 基线，write median/p95 分别回退约 3.7%/2.2%，read median/p95
分别回退约 1.9%/6.2%，sync p95 回退约 3.4%，均在冻结门槛内；sync median
回退约 13.3%，但 sync workload 的 latency 门槛按 p95 判定。相对 7.18 的 owned
API 检查点，write median/p95 改善约 1.7%/9.0%，read median/p95 改善约
7.4%/5.5%，sync p95 改善约 37.7%。本检查点使现有 sequential host workload
重新通过，但完整 workload/feature、Linux 7.1 新功能开销和最终三架构矩阵仍未完成，
不能作为最终性能验收。

### 7.20 legacy indirect truncate 检查点

采集时间：2026-08-11；被测实现 commit 为
`a1e72761288d58198232e441df521f4b52d08f38`，固定 CPU 2，环境、owned API
harness 和 workload 与 7.19 相同。本检查点实现 legacy indirect truncate 的完整
ownership preflight、single/double/triple-indirect 右侧裁剪、pointer block 回滚与
inode 映射先发布后释放。冻结的 sequential workload 不创建或裁剪 legacy indirect
inode，因此本结果只保护共享 write/read/sync 热路径，不能作为 truncate 路径的因果
性能证明。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=a1e72761288d58198232e441df521f4b52d08f38 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6563417 write_p95_ns=7134188 read_median_ns=6976323 read_p95_ns=7841817 sync_median_ns=33291 sync_p95_ns=48397
```

相对 dev 基线，write median/p95 分别改善约 3.9%/2.7%，read median/p95 分别
改善约 3.4%/7.5%，均通过冻结门槛；sync median/p95 分别回退约 28.9%/25.2%，
其中 sync p95 超过 10% latency 上限。相对 7.19，write median/p95 分别回退约
1.1%/1.4%，read median/p95 分别回退约 12.4%/3.4%，sync median/p95 分别回退约
3.2%/35.6%。两个最长 sync 样本 49.157 us 与 48.397 us 均保留在原始数据中，
未丢弃样本，也未用选择性复测覆盖；本检查点原样登记为性能红项。由于 legacy
truncate 不在该 workload 热路径，当前证据不能把回退归因于 truncate 实现，留待
整体性能收敛阶段用冻结 harness 和同机 dev A/B 定位。

### 7.21 legacy indirect reap 检查点

采集时间：2026-08-11；被测实现 commit 为
`678a92512c8939ea02bf352a2c0114c9cce644c7`，固定 CPU 2，环境、owned API
harness 和 workload 与 7.19 相同。本检查点让 final unlink 仅发布 zero-link/orphan，
显式 reap 与 mount recovery 复用强制 mapping cleanup，完成 legacy indirect data、
pointer metadata 和 inode bitmap 回收。冻结的 sequential workload 不执行 unlink、reap
或 orphan recovery，因此结果只保护共享 write/read/sync 热路径，不能作为新删除路径
的因果性能证明。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=678a92512c8939ea02bf352a2c0114c9cce644c7 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=7097313 write_p95_ns=7451501 read_median_ns=7420004 read_p95_ns=7875142 sync_median_ns=34526 sync_p95_ns=38863
```

相对 dev 基线，write/read median 分别回退约 4.0%/2.8%，write/read/sync p95
分别回退约 1.6%、改善约 7.1%、回退约 0.6%，均在冻结硬门槛内。sync median
回退约 33.7%，但 sync workload 的 latency 门槛按 p95 判定。相对 7.20，write
median/p95 分别回退约 8.1%/4.4%，read median/p95 分别回退约 6.4%/0.4%，
sync median 回退约 3.7%，sync p95 改善约 19.7%。8.533 ms write 与 57.090 us
sync 最大样本均保留，未选择性剔除或复测覆盖；7.20 及更早红检查点也保持原始结论。
当前检查点通过 sequential host 门槛，但完整 workload/feature 与最终同机 A/B 仍未完成。

### 7.22 unwritten extent 与 preallocation 检查点

采集时间：2026-08-11；未优化实现 commit 为
`e4f4286d6b079fd639fcd14a7bd74691aa1dbbc2`，批处理优化后 commit 为
`dadce8ebee8ae8e985e2acfeb59d075aebb7f8ae`，固定 CPU 2，环境、owned API harness
和 workload 与 7.19 相同。本检查点引入 Linux 格式的 unwritten extent、
`KEEP_SIZE`/extend-size preallocation、部分写前精确切分和数据持久化后
转 initialized 的两阶段发布。新分配顺序写会经过该路径，因此本 workload
可以捕捉共享热路径回退；它不包含单独 fallocate syscall 或部分覆盖的
延迟分布，不能替代新语义的 Linux 7.1 differential 开销报告。

首次 3 次预热与 20 次测量暴露出 20 MiB 新分配连续区间被拆成
5120 次单块 cache 修改：

```text
RSEXT4_BENCH_SUMMARY commit=e4f4286d6b079fd639fcd14a7bd74691aa1dbbc2 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=13151344 write_p95_ns=14424605 read_median_ns=6525344 read_p95_ns=6983230 sync_median_ns=283976 sync_p95_ns=345254
```

该结果相对 dev
基线，write median/p95 分别回退约 92.6%/96.6%，sync p95 回退约 793%，
原样登记为性能红项。之后将已准备的完整物理 run 批量写入 cache，不改变
转换与事务边界。

优化后重新执行完整 3+20；一组 commit marker 不匹配真实 HEAD 的输出作废，
并以 `git rev-parse HEAD` 填充 marker 重新采集，作废不是因为样本结果。
汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=dadce8ebee8ae8e985e2acfeb59d075aebb7f8ae arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6524295 write_p95_ns=8584700 read_median_ns=6960653 read_p95_ns=9592953 sync_median_ns=32947 sync_p95_ns=41631
```

相对未优化检查点，write median/p95 分别改善约 50.4%/40.5%，sync
median/p95 分别改善约 88.4%/87.9%。相对 dev 基线，write/read median
分别改善约 4.4%/3.6%，sync p95 回退约 7.7%；但 write/read p95 分别
回退约 17.0%/13.1%，超过 10% latency 门槛。所有高延迟样本均保留，
不用作废组或选择性复测覆盖；本检查点仍登记为性能红项，留待完整
workload 与最终同机 dev A/B 的整体性能收敛。

### 7.23 xattr metadata transaction 检查点

采集时间：2026-08-11；被测实现 commit 为 `a51017e08`，固定 CPU 2，环境、
owned API harness 和 sequential workload 与 7.22 相同。本检查点首次引入
filesystem-owned metadata snapshot，并修正 journal-owned device-cache buffer 的
commit/abort 所有权。冻结的 sequential workload 不执行 xattr，因此本结果只守护
共享 allocator、inode、JBD2 与 sync 热路径，不能作为 xattr transaction clone 开销
或 Linux 7.1 xattr 相对开销的替代数据。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=a51017e08 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6266323 write_p95_ns=6973303 read_median_ns=6029894 read_p95_ns=7303204 sync_median_ns=30408 sync_p95_ns=39628
```

相对 dev 基线，write median/p95 分别改善约 8.2%/4.9%，read median/p95 分别
改善约 16.5%/13.9%，sync p95 回退约 2.5%，均通过冻结硬门槛；sync median
回退约 17.8%，但 sync latency 硬门槛按 p95 判定。相对 7.22 优化检查点，
write median/p95 分别改善约 4.0%/18.8%，read median/p95 分别改善约
13.4%/23.9%，sync median/p95 分别改善约 7.7%/4.8%。独立 xattr workload
及 metadata snapshot 优化复测见 7.24；Linux 7.1 syscall 对照仍未完成，因此本项
不能提前判绿。

### 7.24 external xattr 与 touched metadata COW 检查点

采集时间：2026-08-11；固定 CPU 2、memory backend、4 KiB filesystem block、
`metadata_csum+64bit+journal`，每组 3 次预热与 20 次测量。新增
`xattr-external` workload 对 512-byte external value 分别测量 set 后 sync、get、
remove 后 sync，且不改变既有 sequential workload 的计时边界。汇总如下。

| 实现 | commit | set+sync median/p95 (ns) | get median/p95 (ns) | remove+sync median/p95 (ns) |
| --- | --- | ---: | ---: | ---: |
| transaction 前 | `783661ce7-pretxn` | 35,692 / 36,927 | 3,127 / 3,484 | 24,375 / 24,847 |
| 完整 metadata clone | `a51017e08` | 38,735 / 42,981 | 3,295 / 3,535 | 25,907 / 26,135 |
| touched payload COW | `1ffabfbb5` | 37,708 / 38,414 | 2,804 / 3,306 | 25,075 / 25,288 |

相对完整 clone，touched payload COW 的 set median/p95 分别改善约 2.7%/10.6%，
get median/p95 改善约 14.9%/6.5%，remove median/p95 改善约 3.2%/3.2%。
相对 transaction 前实现，set median/p95 回退约 5.6%/4.0%，get median/p95
改善约 10.3%/5.1%，remove median/p95 回退约 2.9%/1.8%。该 workload 对应新增的
持久化 external-xattr 语义，dev 没有等价实现，故不套用虚假的 dev 回退门槛；
Linux 7.1 的同镜像 syscall/fsync 对照仍为红项。memory backend 的
`Ext4::sync` 也不能冒充 Linux `fsync(fd)`，最终验收必须在固定 Linux 7.1 环境重跑。

同 commit 的 sequential workload 连续采集两组，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=1ffabfbb5 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential sample_set=first write_median_ns=9310397 write_p95_ns=10172142 read_median_ns=12778695 read_p95_ns=13765920 sync_median_ns=50382 sync_p95_ns=55585
RSEXT4_BENCH_SUMMARY commit=1ffabfbb5-repeat arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential sample_set=repeat write_median_ns=6635219 write_p95_ns=7151323 read_median_ns=7288561 read_p95_ns=8656543 sync_median_ns=32627 sync_p95_ns=58098
```

采集期间 CPU governor 为 `powersave`，两组 write/read 分布明显双峰，无法用任一组
单独证明或否定 5%/10% 门槛。两组与全部高延迟样本均原样保留，不作废、不选择性
覆盖；当前 sequential 门禁保持红色，待可固定 governor 的同机环境以冻结 harness
重新完成 dev/最终实现 A/B。

### 7.25 preallocation filesystem transaction 检查点

采集时间：2026-08-12；被测实现 commit 为 `d4bbc59cd`，固定 CPU 2，环境、
owned API harness 和 sequential workload 与 7.22 相同。本检查点将每个
preallocation hole chunk 的 extent tree、inode、allocation bitmap、GDT 与
superblock 放入同一个 filesystem-owned transaction，并按 Linux
`ext4_chunk_trans_blocks()` 从当前 group/GDT geometry 推导 credits。顺序写入新
文件会复用该 preallocation 路径，因此本 workload 可以直接捕捉新增 transaction
边界对共享写路径的影响。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=d4bbc59cd arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6357845 write_p95_ns=6471216 read_median_ns=5941441 read_p95_ns=6380433 sync_median_ns=32170 sync_p95_ns=34831
```

相对 dev 基线，write median/p95 分别改善约 6.9%/11.8%，read median/p95
分别改善约 17.7%/24.8%，sync p95 改善约 9.9%，均通过冻结硬门槛。sync
median 回退约 24.6%，但 sync workload 的 latency 门槛按 p95 判定；20 个样本
全部保留，没有丢弃离群值或用选择性复测覆盖。该 memory backend 结果只保护
现有 core sequential 热路径，完整 fallocate workload 与最终同机 dev/Linux 7.1
A/B 仍须在整体性能收敛阶段完成。

### 7.26 extent range-removal restart 检查点

采集时间：2026-08-12；被测实现 commit 为
`1db33f3a858eecb00c82c99efa1551e66845e9dc`，固定 CPU 2，memory backend、
4 KiB filesystem block、`metadata_csum+64bit+journal` 与 20 MiB sequential
workload 均保持冻结配置。本检查点只让超过 journal ring capacity 的 extent
punch/truncate 进入 restart 路径，冻结 workload 不执行范围删除，因此结果只能保护
共享 write/read/sync 热路径，不能作为 restart 路径本身的因果性能数据。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=1db33f3a858eecb00c82c99efa1551e66845e9dc arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=7258847 write_p95_ns=9203180 read_median_ns=8303538 read_p95_ns=10098886 sync_median_ns=35503 sync_p95_ns=45936
```

相对 dev 基线，write/read median 分别回退约 6.3%/15.0%，write/read/sync p95
分别回退约 25.5%/19.1%/18.9%，超过冻结硬门槛；sync median 回退约 37.5%。
采集时 CPU governor 为 `powersave`，20 个样本出现与 7.24 相似的明显双峰，且本次
新增分支不在 workload 热路径，因此当前证据既不能把回退归因于 extent restart，
也不能将本检查点判绿。全部样本（包括 11.943 ms read 和 73.523 us sync）原样
保留，不选择性剔除或用复测覆盖。最终性能收敛必须在能够固定 governor 的同机环境
对 dev 与最终实现重做完整 A/B，并单独增加大范围 punch/truncate workload 报告
相对 Linux 7.1 的开销。

### 7.27 legacy indirect bounded transaction 检查点

采集时间：2026-08-12；被测实现 commit 为
`ed5577756e4ab07b0b92fdf3bfa1b487fb418b27`，固定 CPU 2，memory backend、
4 KiB filesystem block、`metadata_csum+64bit+journal` 与 20 MiB sequential
workload 均保持冻结配置。本检查点把单 ring 容量内的 legacy indirect punch/truncate
迁入 filesystem-owned bounded transaction，并将 zero-link reap 拆成 orphan 保护的
mapping transaction 与最终 inode transaction。冻结 workload 不创建 legacy indirect
mapping，也不执行 punch/truncate/reap，因此本数据只监测共享路径，不能作为新路径
本身的因果性能结论。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=ed5577756e4ab07b0b92fdf3bfa1b487fb418b27 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6289045 write_p95_ns=6387234 read_median_ns=5951789 read_p95_ns=6262131 sync_median_ns=32500 sync_p95_ns=33948
```

相对 dev 基线，write median/p95 分别改善约 7.9%/12.9%，read median/p95
分别改善约 17.6%/26.2%，sync p95 改善约 12.2%，共享 workload 通过冻结硬门槛；
sync median 回退约 25.9%，但 sync latency 继续按 p95 门槛判定。相对 7.26 的
双峰样本，write median/p95 分别改善约 13.4%/30.6%，read median/p95 分别改善
约 28.3%/38.0%，sync median/p95 分别改善约 8.5%/26.1%。两次采集的 governor
均为 `powersave`，因此这里不把波动改善归因于本次非热路径改动；20 个样本全部保留，
包括 49.147 us 的 sync 尾样本。最终性能收敛仍必须在可固定 governor 的同机环境
重做 dev/final 全 workload A/B，并新增 legacy indirect truncate/reap 专项 workload。

### 7.28 legacy indirect truncate restart 检查点

采集时间：2026-08-12；被测实现 commit 为
`07e8243e9f5d253c2718853743a62e0fc032d1db`，固定 CPU 2，memory backend、
4 KiB filesystem block、`metadata_csum+64bit+journal` 与 20 MiB sequential
workload 均保持冻结配置。本检查点实现超 ring legacy indirect truncate/reap restart，
但冻结 workload 不创建 legacy mapping，也不执行 truncate/reap；因此只能观测共享
write/read/sync 热路径，不能作为 restart 路径的因果性能结论。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=07e8243e9f5d253c2718853743a62e0fc032d1db arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=8183734 write_p95_ns=8533919 read_median_ns=10598332 read_p95_ns=12150067 sync_median_ns=44336 sync_p95_ns=49554
```

相对 dev 基线，write median/p95 分别回退约 19.9%/16.3%，read median/p95
分别回退约 46.8%/43.3%，sync median/p95 分别回退约 71.7%/28.2%，全部超过
冻结硬门槛，本检查点判红。采样时 CPU governor 为 `powersave`，系统 load average
为 8.13，另一个无关 release `rustc` 进程占用约 8 个逻辑核；这说明本轮同机条件
不受控，但不能把门槛改判为绿。20 个样本全部保留，没有选择性复测或剔除。
最终性能收敛必须在受控系统负载下同时重跑 dev 与最终实现，并增加 legacy indirect
truncate/reap 专项 workload；只有同一组 A/B 数据满足门槛后才能清除该红项。

### 7.29 JBD2 writer revoke 与 checkpoint 检查点

采集时间：2026-08-12；被测实现 commit 为
`6ae468a65b88ae0be3821813baf2dc5e42dc1550`，固定 CPU 2，memory backend、
4 KiB filesystem block、`metadata_csum+64bit+journal` 与 20 MiB sequential
workload 均保持冻结配置。本检查点将 running transaction 与 committed checkpoint
image 分离，写出 Linux-compatible revoke record，并在 home write 持久化后才以 FUA
推进 journal tail。冻结 workload 的写、读与 sync 会经过共享 journal 路径，但不构造
allocator reuse/revoke 场景，因此不能替代专项 replay 与 fault-injection 测试。

正式检查使用 3 次预热与 20 次测量，汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=6ae468a65b88ae0be3821813baf2dc5e42dc1550 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6309883 write_p95_ns=7230443 read_median_ns=5940001 read_p95_ns=7577594 sync_median_ns=31856 sync_p95_ns=34724
```

相对 dev 基线，write median/p95 分别改善约 7.6%/1.4%，read median/p95 分别
改善约 17.7%/10.7%，sync p95 改善约 10.1%；现有 sequential workload 的 median
吞吐与 p95 latency 均通过冻结硬门槛。sync median 回退约 23.4%，按 latency p95
门槛不单独判红，但完整结果继续保留。相对 7.28 的受干扰红样本，write median/p95
分别改善约 22.9%/15.3%，read median/p95 分别改善约 44.0%/37.6%，sync median/p95
分别改善约 28.1%/29.9%。本轮 governor 仍为 `powersave`，采样前 load average 为
3.45，不能据此把改善归因于 revoke/checkpoint 实现；最终仍需受控同机 dev/final
全 workload A/B 与 revoke/checkpoint 专项 workload。

### 7.30 JBD2 tail、dirty eviction 与 unlink cache owner 检查点

采集时间：2026-08-12；core 样本 marker 为 `5adaafb44`（后续 amend 只改动
ax-fs-ng，rsext4 代码与当前 `c0245c1bf` 相同）。环境与 7.29 一致：
CPU 2、`powersave` governor、memory backend、4 KiB filesystem block、20 MiB
sequential workload、3 次预热与 20 次测量。汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=5adaafb44 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6701571 write_p95_ns=7395983 read_median_ns=7400465 read_p95_ns=9004776 sync_median_ns=36196 sync_p95_ns=55591
```

| core workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 门禁 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| sequential write | 6.827 ms | 6.702 ms | -1.8% | 7.335 ms | 7.396 ms | +0.8% | 绿 |
| sequential read | 7.219 ms | 7.400 ms | +2.5% | 8.481 ms | 9.005 ms | +6.2% | 绿 |
| sync latency | 25.823 us | 36.196 us | +40.2% | 38.644 us | 55.591 us | +43.9% | 红 |

因此当前 host core 检查点不判绿；不用 governor 波动或非热路径解释放宽
sync p95 硬门槛。同一代码的 LoongArch64 grouped QEMU 提供了另一个独立的
全栈趋势证据：

| QEMU workload | 修复前 | 当前 | 变化 |
| --- | ---: | ---: | ---: |
| `qemu/system` | 1800.24s timeout | 452.07s pass | 至少 -74.9% |
| `test-pagecache-cap` | 后续 `sync` 无法在 case timeout 内完成 | 66s pass | 已解除 1400 个 unlink cache owner |
| `test-sync` | timeout | 0s pass | 全局 dirty owner 扫描收敛 |
| grouped cases | system fail | system + tty 2/2 pass | 功能门禁转绿 |

QEMU 收敛来自三个可独立验证的边界：data-block dirty LRU 一次回收
四分之一并合并连续 device write；整块 cache writeback 不再先读 home block；
final unlink 解除 global page-cache owner，不再由全局 `sync(2)` 反复写回已不可达
文件。该 QEMU 数据只用于功能门禁和趋势，不替代最终受控 host
dev/final A/B；下一阶段必须继续优化 sync p95 并在固定 governor 后重采。

### 7.31 Linux FUA commit publication 检查点

Linux v7.1 `fs/jbd2/commit.c:115-168,805-915` 在非 async commit 路径以
`REQ_PREFLUSH | REQ_FUA` 的 commit record 作为 transaction publication：
preflush 排序 descriptor/payload，FUA 使 commit record 本身 durable；等待该同步
write 完成后不会再提交第二个 post-commit flush。旧 core 在显式 preflush 和 FUA
commit 后仍无条件 flush。确定性红测先把该路径的预期 flush 数从 2 改为 1，旧实现
稳定失败；删除冗余 flush 后同一测试转绿，fault matrix 也移除了 Linux 中不存在的
`commit-barrier` 故障边界。FUA 不可用时 `CachedDevice` 仍严格执行
write-then-flush fallback，不伪造 durability。

同一修改还避免 single-transaction checkpoint 在已经没有更早 image 时填充
`later_blocks` 集合；多 transaction 覆盖与 revoke 仍使用原有反向过滤。固定 CPU 2、
`powersave` governor、3 次预热、20 次测量的汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=c822b89df arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6385677 write_p95_ns=6892860 read_median_ns=6001960 read_p95_ns=6938485 sync_median_ns=33227 sync_p95_ns=35032
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| sequential write | 6.827 ms | 6.386 ms | -6.5% | 7.335 ms | 6.893 ms | -6.0% | 绿 |
| sequential read | 7.219 ms | 6.002 ms | -16.9% | 8.481 ms | 6.938 ms | -18.2% | 绿 |
| sync/unmount latency | 25.823 us | 33.227 us | +28.7% | 38.644 us | 35.032 us | -9.3% | p95 绿；median 红 |

相对 7.30，sync median/p95 分别改善 8.2%/37.0%，p95 已回到硬门槛内；但
`host_perf` 的历史 `sync_ns` 实际计量 `unmount()`，包含 clean-superblock mutation，
不能等同普通 `sync(2)`。在保持冻结 marker 可比性的同时，后续 harness 需要新增独立
sync 与 unmount workload；在其余冻结 workload 完成 A/B 前，总性能门禁继续保持红色。

### 7.32 Group descriptor dirty owner 检查点

旧 `sync_group_descriptors()` 每次同步都会读取、重新编码并写回全部 primary GDT，
即使 mount 后没有任何 descriptor mutation。确定性红测使用 counting `BlockIo`：清零
mkfs/mount I/O 后立即 `Ext4::sync()`，旧实现稳定写 primary GDT 一次；新实现由
filesystem state 持有 per-group dirty bitmap，所有 `get_group_desc_mut()` 置位，成功
publish 对应 descriptor block 后才清位，metadata transaction abort 则与 descriptor
一起恢复 dirty bitmap。相同测试现要求 GDT write 为 0，同时 device flush 必须大于 0，
因此没有以跳过 durability boundary 换取假性能。

20 次 sequential 汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=9eb326e2d arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6311198 write_p95_ns=6853818 read_median_ns=5999018 read_p95_ns=7331847 sync_median_ns=32512 sync_p95_ns=33487
```

| workload | 相对 dev median | 相对 dev p95 | sequential 门禁 |
| --- | ---: | ---: | --- |
| sequential write elapsed | -7.6% | -6.6% | 绿 |
| sequential read elapsed | -16.9% | -13.5% | 绿 |
| sync/unmount latency | +25.9% | -13.3% | p95 绿 |

该优化主要约束 clean sync，当前 create+20 MiB write 的 unmount 本就有 allocation
descriptor 必须发布，因此 median 仅比 7.31 改善约 2.2%。它仍消除了空闲系统上重复
`sync(2)` 的 O(group-count) 写放大。完整 all-features、no-default-features、30 个
Linux image/e2fsck differential、三配置 clippy 和 architecture boundary audit 均通过。

### 7.33 Superblock dirty owner 检查点

旧 `sync_filesystem()` 即使 superblock 没有任何变化，也会重新汇总 group counters、
更新 checksum 并发布 primary superblock。与 7.32 的 per-group descriptor dirty bitmap
配套，本阶段在 filesystem state 中加入 superblock dirty owner：mount/replay state、
allocation group counter、classic orphan head、lost+found hint 和首次启用 `EXT_ATTR`
显式置位；metadata transaction snapshot 同时保存并回滚 dirty 状态；只有成功发布
superblock 后才清位。clean `Ext4::sync()` 的 counting `BlockIo` 红测在旧实现上稳定观察
到一次 sector 0 write，新实现要求 superblock/GDT write 均为 0，同时保留至少一次 device
flush，避免以删除 durability boundary 换取假性能。

固定 CPU 2、`powersave` governor、3 次预热、20 次测量。首轮 write p95 受后半段主机
抖动影响为 7.728 ms，超过 dev 门槛 0.36 个百分点；没有删样本，而是在完全相同配置
下复测。复测 marker 为：

```text
RSEXT4_BENCH_SUMMARY commit=a95663e96-repeat arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6333066 write_p95_ns=6659600 read_median_ns=5907374 read_p95_ns=6311756 sync_median_ns=30622 sync_p95_ns=35290
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| sequential write | 6.827 ms | 6.333 ms | -7.2% | 7.335 ms | 6.660 ms | -9.2% | 绿 |
| sequential read | 7.219 ms | 5.907 ms | -18.2% | 8.481 ms | 6.312 ms | -25.6% | 绿 |
| sync/unmount latency | 25.823 us | 30.622 us | +18.6% | 38.644 us | 35.290 us | -8.7% | p95 绿；median 红 |

冻结的 sequential `sync_ns` 仍然计量 clean-unmount，而 unmount 必须改变并发布
`EXT4_VALID_FS`/`RECOVER`，因此本阶段消除的 clean-sync write amplification 不会被该
字段直接体现。该语义差异继续作为后续独立 sync/unmount workload 的前置理由；完整
all-features、no-default-features、Linux image/e2fsck differential、三配置 clippy、
format 与 dependency boundary 已通过。

### 7.34 JBD2 committing transaction owner 检查点

Linux v7.1 `fs/jbd2/commit.c:434-590,1115-1162` 在任何 journal I/O 前先锁定
`j_running_transaction`，随后在 `T_FLUSH` 阶段把它发布为
`j_committing_transaction` 并清空 running owner；提交完成后再从 committing owner
移入 checkpoint list。旧 Rust core 直到 FUA commit 成功后才从同一个
`commit_queue` 取走 update/revoke，因而 I/O 已开始的失败 transaction 仍被错误表示为
running transaction。

本阶段把 JBD2 内存状态拆为 `running_transaction`、单一
`committing_transaction` 和 oldest-first `checkpoint_transactions`。同步 core 的提交阶段
显式经过 `Flush -> Commit -> DataFlush -> JournalFlush`：开始 commit 时立即移动
update/revoke 所有权；FUA commit 成功后才清空 committing owner 并进入 checkpoint；任一
I/O 失败则保留 committing transaction 和精确阶段，同时由 sticky abort 禁止后续 handle、
write、flush、mode change 或 reinstall。读路径仍按 running、committing、checkpoint 的
新旧次序合并 update/revoke 可见性。

确定性红测在 descriptor/payload preflush 注入错误：旧实现的 running queue 仍含一个
update，稳定失败；新实现要求 running 为空、committing 持有一个 update 且阶段为
`DataFlush`。成功路径同时断言 committing 已清空且 checkpoint 恰持有一个 transaction。
56 个 JBD2 fault/replay/checkpoint 测试以及完整 Linux image differential 均通过。

固定 CPU 2、`powersave` governor、3 次预热、20 次测量的汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=1983fc88e arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6350459 write_p95_ns=6735837 read_median_ns=6112545 read_p95_ns=7269978 sync_median_ns=32787 sync_p95_ns=34825
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| sequential write | 6.827 ms | 6.350 ms | -7.0% | 7.335 ms | 6.736 ms | -8.2% | 绿 |
| sequential read | 7.219 ms | 6.113 ms | -15.3% | 8.481 ms | 7.270 ms | -14.3% | 绿 |
| sync/unmount latency | 25.823 us | 32.787 us | +27.0% | 38.644 us | 34.825 us | -9.9% | p95 绿；median 红 |

该状态重构没有引入 sequential write/read 或 sync p95 回退；历史 `sync_ns` 仍是
clean-unmount，median 红项继续保留到独立 sync/unmount workload 和后续整体优化完成。

### 7.35 独立 sync/unmount workload 检查点

冻结的 sequential `sync_ns` 实际只计量 `unmount()`，无法区分 dirty metadata
publication、无新增 mutation 的 clean sync 和最终 clean-state unmount。本阶段保留旧
sequential/xattr marker 不变，新增 `RSEXT4_BENCH_WORKLOAD=sync-cycle`：每个样本在同一
mount 上创建文件并写入 20 MiB，依次计量第一次 dirty `sync()`、第二次 clean
`sync()` 和 `unmount()`。该 workload 在加入前以 unsupported workload 稳定失败；加入
后 sequential、xattr 与 sync-cycle 三条分支均通过 smoke 和 example clippy。

基线 `6e27704c4` 没有 owned `Ext4<D, S>`、`BlockIo` 或现成 host harness，因此不能直接
运行当前源码。为避免伪称二进制同 harness，基线使用操作序列等价的旧 API adapter：
同样的 128 MiB memory device、4 KiB block、journal、20 MiB payload、3 次预热、20 次
测量，调用旧 `mkfs -> mount -> mkfile -> write_inode_data -> sync -> sync -> umount`。
设备 trait 和 owned API
不同是明确可比性边界，但操作顺序、数据量和统计口径一致。

```text
RSEXT4_BENCH_SUMMARY commit=6e27704c4 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=6751 dirty_sync_p95_ns=7061 clean_sync_median_ns=2962 clean_sync_p95_ns=3087 unmount_median_ns=10336 unmount_p95_ns=11147
RSEXT4_BENCH_SUMMARY commit=00e64e1fe arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=22339 dirty_sync_p95_ns=25484 clean_sync_median_ns=247 clean_sync_p95_ns=275 unmount_median_ns=9527 unmount_p95_ns=12010
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| dirty sync | 6.751 us | 22.339 us | +230.9% | 7.061 us | 25.484 us | +260.9% | 红 |
| clean sync | 2.962 us | 0.247 us | -91.7% | 3.087 us | 0.275 us | -91.1% | 绿 |
| clean-state unmount | 10.336 us | 9.527 us | -7.8% | 11.147 us | 12.010 us | +7.7% | 绿 |

dirty sync 的回退不是统计噪声，也不会用 clean sync 改善抵消：当前正确的 JBD2
descriptor/payload preflush、FUA commit、checkpoint 和 tail publication 都落在第一次
sync 中，而 dev 的旧 transaction owner 与 durability 语义并不完整。该 workload 因此
成为新的硬红项；后续必须优化正确实现或获得明确批准，不能通过删除持久化边界、合并
三个阶段或放宽阈值转绿。

### 7.36 JBD2 checkpoint 最终持久化边界检查点

Linux v7.1 `fs/jbd2/checkpoint.c:326-353` 在回收 journal tail 前先 flush
filesystem device，再由 `__jbd2_update_log_tail()` 以 `REQ_FUA` 发布新 tail；
`fs/jbd2/journal.c:2419-2473` 的 `jbd2_journal_flush()` 完成该序列后不会再执行一次
无条件 device flush。旧 Rust core 已经在 checkpoint 中完成 home-block flush 和 FUA tail
publication，却仍在 `Jbd2Dev::flush()` 末尾重复调用 `inner.flush()`。

确定性 counting `BlockIo` 测例把 fallback-FUA 设备上的 dirty sync 和 clean-state
unmount 从宽松的 `flushes > 0` 收紧为精确四次。旧实现稳定观测到五次并失败；新实现仅在
本轮确实 checkpoint 了 transaction 时以 FUA tail 作为最终 durability boundary，无 journal
work 的 clean sync 仍调用一次真实 device flush。相同测例同时固定 dirty/clean/unmount 的
write 与 flush 阶段相互独立，防止用合并 workload 隐藏回退。56 个 JBD2 定向测试、完整
`cargo test -p rsext4 --all-features` 和三配置 targeted clippy 均通过。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量的汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=46784bf62 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=21578 dirty_sync_p95_ns=22474 clean_sync_median_ns=247 clean_sync_p95_ns=280 unmount_median_ns=9342 unmount_p95_ns=9568
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| dirty sync | 6.751 us | 21.578 us | +219.6% | 7.061 us | 22.474 us | +218.3% | 红 |
| clean sync | 2.962 us | 0.247 us | -91.7% | 3.087 us | 0.280 us | -90.9% | 绿 |
| clean-state unmount | 10.336 us | 9.342 us | -9.6% | 11.147 us | 9.568 us | -14.2% | 绿 |

相对 7.35，dirty sync median/p95 分别改善 3.4%/11.8%，unmount median/p95 分别改善
1.9%/20.3%；但是 dirty sync 相对 dev 的正确性成本仍远超 5%/10% 门槛，继续保留为硬红项。
后续优化必须减少实际 metadata/journal I/O 或状态转换成本，不能删除 descriptor/payload
preflush、FUA commit、checkpoint home flush 或 FUA tail publication 中任一必要边界。

### 7.37 Linux sync commit/checkpoint 分界检查点

Linux v7.1 `fs/ext4/super.c:6430-6473` 的 `ext4_sync_fs(wait=1)` 只启动并等待最新
transaction commit；它不调用 `jbd2_journal_flush()`，不强制 checkpoint，也不把 journal
标记为空。完整 checkpoint、home metadata writeback、tail cleanup 和 FUA empty publication
属于 `fs/jbd2/journal.c:2419-2473` 的 full journal flush/unmount 路径。旧 Rust
`Ext4::sync()` 通过 `Jbd2Dev::flush()` 同时 commit 和 checkpoint，因而普通 sync 比 Linux
更强并把 home-write 成本错误归入 dirty-sync 阶段。

本阶段新增内部 `commit_for_filesystem_sync()`：有 running transaction 时执行
descriptor/payload preflush 和 FUA commit，保留 committed owner 等待后续 checkpoint；没有
transaction 时仍执行一次 device flush，覆盖可能独立存在的 data writeback。unmount 继续在
clean-state transaction commit 后显式 checkpoint 全部 owner 并 FUA 发布 tail。counting-I/O
红测在旧实现稳定观测 primary superblock/GDT 各一次 home write 和四次 flush；同一测例现
要求普通 dirty sync 不写 home superblock/GDT、只执行 preflush 与 FUA fallback 两个 flush，
而 clean sync 保留一个 flush、unmount 才执行 home checkpoint 和四个完整持久化边界。

改变 sync 语义后，restart/direct-fault 测试夹具不再依赖普通 sync 的隐藏 checkpoint
副作用：需要替换 journal geometry 或关闭 journal 的夹具先显式 full checkpoint；模拟掉电
则销毁旧 JBD2 owner，以新 owner 包装保留的 device bytes。另一个确定性红测证明旧
`set_journal_superblock()` 会静默丢弃 committed checkpoint owner；现在 running、committing、
checkpoint、active handle 或 log accounting 非空时统一返回 typed `Busy`。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量的汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=b1bafeabd arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=19313 dirty_sync_p95_ns=22365 clean_sync_median_ns=218 clean_sync_p95_ns=240 unmount_median_ns=9746 unmount_p95_ns=11293
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| dirty sync | 6.751 us | 19.313 us | +186.1% | 7.061 us | 22.365 us | +216.7% | 红 |
| clean sync | 2.962 us | 0.218 us | -92.6% | 3.087 us | 0.240 us | -92.2% | 绿 |
| clean-state unmount | 10.336 us | 9.746 us | -5.7% | 11.147 us | 11.293 us | +1.3% | 绿 |

相对 7.36，dirty sync median 改善 10.5%，clean sync median/p95 改善 11.7%/14.3%；
checkpoint 成本移回 unmount 后，unmount 相对 dev 仍在 5%/10% 门槛内。dirty sync p95 仅
改善 0.5%，且相对 dev 仍是硬红；剩余优化目标已收窄到 commit/checksum/metadata cache CPU
路径，不能再归因于普通 sync 误做 checkpoint。

### 7.38 JBD2 transaction payload 借用检查点

Linux v7.1 `fs/jbd2/commit.c:631-915` 让 descriptor tag checksum 和 journal write 直接
消费 transaction-owned metadata buffer；只有 payload 首四字节等于 JBD2 magic 时才建立
escaped image。旧 Rust writer 在任何 journal I/O 前都把每个 `Box<[u8]>` 无条件复制到
`Vec<u8>`，当前 sync-cycle 的六个 metadata update 因而额外分配并复制 24 KiB。

本阶段将普通 payload 改为借用 `committing_transaction` 的唯一 buffer，只为 magic payload
分配 zeroed escaped image。descriptor 仍先于其 payload 写出，CSUM_V3 tag 仍覆盖实际写入
journal 的 escaped bytes，成功后原 update 才移动到 checkpoint owner；任何 I/O error 都不会
临时取走 committing owner。确定性单测同时断言普通 payload 指针相同，以及 escaped journal
image、tag checksum 与未转义 checkpoint home image。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量。汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=14220cfeb arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=18821 dirty_sync_p95_ns=21773 clean_sync_median_ns=218 clean_sync_p95_ns=286 unmount_median_ns=10279 unmount_p95_ns=15770
```

| workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| dirty sync | 6.751 us | 18.821 us | +178.8% | 7.061 us | 21.773 us | +208.3% | 红 |
| clean sync | 2.962 us | 0.218 us | -92.6% | 3.087 us | 0.286 us | -90.7% | 绿 |
| clean-state unmount | 10.336 us | 10.279 us | -0.6% | 11.147 us | 15.770 us | +41.5% | p95 红 |

相对 7.37，dirty sync median/p95 分别改善约 2.5%/2.6%，但正确 commit 与 dev 的旧
flush-only sync 仍不是等价 durability，硬红项保持不变。unmount 的 30.494 us 最大样本没有
剔除，因而 p95 原样判红；该波动也不能抵消 dirty sync 的稳定改善。下一步继续优化所有
metadata checksum 共用的 CRC32C 热路径，而不改变 transaction I/O 或 publication 边界。

### 7.39 x86_64 GPR CRC32C 检查点

通用 slicing-by-8 CRC32C 在 7.38 的 dirty sync 中仍需扫描六个 4 KiB payload、descriptor
和 commit block。x86_64 的 SSE4.2 CRC32 指令可以直接更新同一个 raw accumulator；但 core
不能要求 OS 保存 SIMD/FPU 状态，也不能把该能力伪装成 `BlockIo`。本阶段在私有算法模块中
用 CPUID leaf 1 ECX bit 20 运行时检测能力，并用 inline assembly 的 64-bit/8-bit GPR operand
执行 `crc32`。泛型 binary 在不支持的 CPU 上继续使用 slicing-by-8，不新增依赖、OS runtime
trait、全局 logger 或 lock，也不改变 ext4/JBD2 seed、finalize 和端序语义。

确定性测试先因 x86 模块不存在而编译红；实现后按三个 seed、offset 0..7 和长度 0..257
逐项比较 hardware 与 software accumulator，canonical、incremental、metadata 和 59 个 JBD2
测试也全部通过。release binary 的反汇编确认生成 GPR `crc32 r64,r64`/`crc32 r32,r8`；helper
没有使用 XMM/YMM operand。测试机为 Intel Core i7-10700，CPU 广告 `sse4_2`。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量；两组汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=795c113e3 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=8115 dirty_sync_p95_ns=8784 clean_sync_median_ns=236 clean_sync_p95_ns=291 unmount_median_ns=5196 unmount_p95_ns=5485
RSEXT4_BENCH_SUMMARY commit=795c113e3 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sequential write_median_ns=6315245 write_p95_ns=6558738 read_median_ns=5920310 read_p95_ns=6177423 sync_median_ns=18570 sync_p95_ns=19945
```

| sync-cycle workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| dirty sync | 6.751 us | 8.115 us | +20.2% | 7.061 us | 8.784 us | +24.4% | 红 |
| clean sync | 2.962 us | 0.236 us | -92.0% | 3.087 us | 0.291 us | -90.6% | 绿 |
| clean-state unmount | 10.336 us | 5.196 us | -49.7% | 11.147 us | 5.485 us | -50.8% | 绿 |

相对 7.38，dirty sync median/p95 分别改善约 56.9%/59.7%，unmount median/p95 分别改善
约 49.5%/65.2%。但 dev 的 flush-only sync 虽不具备当前 durable JBD2 commit 语义，当前数据
仍按冻结门槛保守判红，不用语义差异擅自豁免 5%/10% 上限。

| sequential workload | dev median | current median | 变化 | dev p95 | current p95 | 变化 | 当前结论 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| write elapsed | 6.827 ms | 6.315 ms | -7.5% | 7.335 ms | 6.559 ms | -10.6% | 绿 |
| read elapsed | 7.219 ms | 5.920 ms | -18.0% | 8.481 ms | 6.177 ms | -27.2% | 绿 |
| unmount latency | 25.823 us | 18.570 us | -28.1% | 38.644 us | 19.945 us | -48.4% | 绿 |

因此共享 sequential 热路径重新明确通过，dirty-sync 差距也从约三倍收敛到约 1.2 倍。
剩余红项继续定位 commit state、descriptor 构造和小 metadata checksum；不能删除 preflush、
FUA commit 或其他 Linux durability boundary。

### 7.40 legacy indirect punch restart 检查点

Linux v7.1 `fs/ext4/inode.c:4427-4543` 的 `ext4_punch_hole()` 与 truncate 有一个关键
差异：它不调用 `ext4_orphan_add()`，也不把 punch range 编码进 inode 或其他磁盘结构。
legacy 路径进入 `ext4_ind_remove_space()`；`fs/ext4/indirect.c:724-748` 在 credit 不足时
通过 `ext4_journal_ensure_credits_fn()` 结束当前 transaction、重新取得 handle 后继续。
因此 Linux 保证的是每个 commit boundary 上 pointer tree、inode accounting 与 allocator
自洽，而不是掉电后自动补完整次 punch。掉电可能留下已经提交的部分 hole；调用方重试同一
区间必须能从该状态继续。此前台账要求“durable range intent”并不符合 Linux，现已按源码
证据删除，不能为追求比 Linux 更强的伪原子性发明私有 on-disk protocol。

旧 Rust 实现先构建完整 `LegacyTruncatePlan`，只要 footprint 超过 journal ring 就在任何
mutation 前稳定返回 `NoSpace(indirect:transaction_credits)`。确定性红测使用两个 allocation
group，并同时放置 direct、single、double、triple data、logical hole 和空 double-indirect
leaf；27-block journal 按 Linux `/3` 上限只提供 7 个 user credits，full-range punch 必然超过
单 transaction。当前实现先预检至少一个
data 或 metadata-cleanup chunk 能装入 ring，再从最高 logical block 向左提交 child-first
chunk；每次 commit 后重新扫描当前已提交 ownership tree，不复用失效 pointer path。最后只
发布 write-access metadata 时间，`i_size` 保持原值，`s_last_orphan` 始终为零。

同一 fixture 的 commit block 落盘后、journal tail 更新前断电测试证明 JBD2 replay 后 inode
仍保持原 size、没有 orphan，且当前部分 punch tree 可再次执行相同 range operation。重试、
unmount、remount 后全部 data/pointer block 已释放，特意保留的 gap 仍 allocated，`i_blocks`
与 free count 一致。`extent_restart` 现有 8 个 bounded restart/power-cut case 全部通过。
这只清除了 legacy punch 的超 ring 子项；完整 persistence-boundary fault matrix、concurrent
handle、独立 committing owner 与大 shift restart 仍保持红色。reserved handle 后续由 7.50 单独收敛。

### 7.41 JBD2 legacy checksum mode 检查点

Linux v7.1 `include/linux/jbd2.h:150-204` 明确规定 checksum v1、v2、v3 互斥。
`FEATURE_COMPAT_CHECKSUM` 不是 journal superblock v1：它在 v2 superblock 上使用从
`0xffff_ffff` 开始、不做 final XOR 的 big-endian CRC32，依次覆盖每个完整 descriptor block
及其 journal payload，并把结果写入 commit header 的 type `1`、size `4`、checksum[0]。
`CSUM_V2` 与 `CSUM_V3` 都使用 UUID-seeded CRC32C；区别在于 v2 tag 只保存低 16 bit，
32/64-bit block number 的 wire size 分别是 10/14 bytes，v3 则使用固定 16-byte tag 保存
完整 32 bit。二者都在 descriptor/revoke 尾部保存 whole-block CRC32C，并对 checksum 字段
清零后的完整 commit block 计算 CRC32C。对应证据为 `fs/jbd2/commit.c:90-144,329-369,
391-409,620-760`、`fs/jbd2/recovery.c:175-220,400-488,810-850` 与
`fs/jbd2/journal.c:2312-2350,2688-2699`。

旧实现只接受 CSUM_V3：COMPAT/CSUM_V2 superblock 会返回 `Unsupported`，legacy tag
checksum 和 commit tuple 始终为零，payload corruption 因而可通过 replay。确定性红测先分别
固定这些失败。当前私有 `Jbd2ChecksumMode` 在 mount、capacity、writer 和 replay 共用一次
语义，拒绝混合 mode/checksum-type；CSUM_V2 生成低 16-bit tag checksum、零 padding、
descriptor/revoke tail 与 commit checksum，replay 在任何 home write 前验证 descriptor、
revoke、commit 和全部 payload。COMPAT writer/replay 则按 Linux 顺序聚合 raw CRC32-BE，
同时保留 Linux 对全零“unused checksum tuple”的兼容。新 journal 也显式声明已经实现的
`INCOMPAT_REVOKE`，不再生成 feature 位与实际 revoke record 能力不一致的日志。

除了 synthetic 32/64-bit CSUM_V2 wire vectors 与逐边界 corruption 测试，本阶段新增真实
e2fsprogs differential：`debugfs journal_open -c -v 2` 生成多 metadata block 的
`FEATURE_COMPAT_CHECKSUM` transaction，Linux/debugfs baseline 与 rsext4 replay 后目录树一致，
`needs_recovery` 被清除且 `e2fsck -fn` clean。现代 e2fsprogs 1.46.5 不直接生成 legacy
CSUM_V2（`-v 2` 实际生成 compat CRC32），因此 CSUM_V2 继续以 Linux 7.1 源码布局和独立
reference CRC32C fixture 验证，不伪称拥有工具生成的 fixture。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量，首组与完整重复组
均保留，首组的 28.016 us 最大样本没有剔除：

```text
RSEXT4_BENCH_SUMMARY commit=837ff2407 arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=7975 dirty_sync_p95_ns=11099 clean_sync_median_ns=205 clean_sync_p95_ns=291 unmount_median_ns=5174 unmount_p95_ns=6690
RSEXT4_BENCH_SUMMARY commit=837ff2407-repeat arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=8426 dirty_sync_p95_ns=11066 clean_sync_median_ns=236 clean_sync_p95_ns=289 unmount_median_ns=5394 unmount_p95_ns=6975
```

| sample | dirty median | 相对 7.39 | dirty p95 | 相对 7.39 | clean median | unmount median |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| first | 7.975 us | -1.7% | 11.099 us | +26.4% | 0.205 us | 5.174 us |
| repeat | 8.426 us | +3.8% | 11.066 us | +26.0% | 0.236 us | 5.394 us |

median 未显示 checksum-mode dispatch 的稳定回退，但两组 p95 都高于 7.39；该结果原样保留
为性能红项，不能以“新增兼容功能”豁免。相对 dev 的 dirty-sync median/p95 仍超过 5%/10%
硬门槛，后续继续优化 descriptor/revoke scratch allocation 与 commit CPU 路径，但不得改变
preflush、FUA commit、checksum 覆盖范围或 checkpoint/tail durability boundary。

### 7.42 JBD2 scratch block 复用检查点

Linux v7.1 `fs/jbd2/commit.c:579-805` 复用 journal descriptor/payload submission 所需的
buffer owner；持久化语义来自 descriptor/tag/checksum、提交顺序与 completion/error 检查，
不是每条 record 新建 heap object。Rust writer 此前对每个 revoke record、每个 descriptor
record 和最终 commit block 分别创建一个 filesystem-block-sized `Vec`。默认 sync-cycle 虽然
只有一个 descriptor，也仍在 descriptor 写完后重新分配 commit buffer；大 transaction 和
大量 revoke 则按 record 数重复分配。

本阶段让同一 transaction 的 revoke records 共用一个 scratch block，每条 record 开始前清零；
descriptor records 同样共用一个 scratch block，descriptor 全部同步写出后再把该 buffer 的
所有权直接移动为 commit buffer 并清零。`BlockIo` 仍只看到原来的同步 slice write，
descriptor/payload preflush、FUA commit、checkpoint 和 tail publication 的次数与顺序完全不变。
清零不能省略：CSUM_V2/V3 descriptor/revoke tail checksum 覆盖完整 block，padding 必须稳定为
零。multi-descriptor、writer revoke、CSUM_V2 corrupt-revoke 定向测试通过，随后 all-features、
no-default 完整测试和 rsext4 3/3 clippy 均通过。

固定 CPU 2、`powersave` governor、20 MiB、3 次预热、20 次测量，两组汇总如下：

```text
RSEXT4_BENCH_SUMMARY commit=2ffe3de0b arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=7911 dirty_sync_p95_ns=8792 clean_sync_median_ns=194 clean_sync_p95_ns=263 unmount_median_ns=5182 unmount_p95_ns=6058
RSEXT4_BENCH_SUMMARY commit=2ffe3de0b-repeat arch=x86_64 backend=memory feature=metadata_csum+64bit+journal workload=sync-cycle dirty_sync_median_ns=8325 dirty_sync_p95_ns=9163 clean_sync_median_ns=226 clean_sync_p95_ns=305 unmount_median_ns=5558 unmount_p95_ns=5768
```

| sample | dirty median | 相对 7.41 同组 | dirty p95 | 相对 7.41 同组 | 相对 7.39 median/p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| first | 7.911 us | -0.8% | 8.792 us | -20.8% | -2.5% / +0.1% |
| repeat | 8.325 us | -1.2% | 9.163 us | -17.2% | +2.6% / +4.3% |

因此本轮 legacy checksum 检查点出现的 p95 扩张已经收回，且对 7.39 的正确 CSUM_V3 热路径
保持在 5% 内；但相对冻结 dev 的 dirty-sync median/p95 仍分别至少回退 17.2%/24.5%，继续
登记为硬红项。下一步优化 transaction owner/revoke clone 与 descriptor/payload 两次 escape
准备前，必须先证明不会在 I/O error 时丢失 committing owner，也不能缓存会跨 transaction
失效的借用。

### 7.43 JBD2 nested same-owner handle 检查点

Linux v7.1 `fs/jbd2/transaction.c:470-481,1883-1893` 在当前 task 已持有同一 journal
handle 时不创建第二个 reservation，也不拒绝调用；它只递增 `h_ref`，最后一次 stop 才把
handle 从 transaction 分离。nested start 传入的 `nblocks` 不扩展 outer credit budget。

旧 Rust owner 只有一个 `active_handle` slot，任何 nested metadata helper 都稳定返回
`Busy(jbd2:nested_handle)`，使已经处于 filesystem transaction 内的 helper 无法安全组合。
确定性红测先在 outer 2-credit handle 中写一个 metadata block，再 nested start 并写第二块；
旧实现按预期在 nested start 失败。当前闭包 lifetime 充当 scoped `h_ref`：nested start 直接复用
outer owner，即使传入超过整个 ring 的 credit 值也不会建立第二份 reservation，两个 distinct
block 仍共同受 outer 2-credit 上限约束，transaction sequence 在 outer return 前保持不变。

rsext4 的 handle 还承担 operation-error queue rollback，这比 Linux 的裸 JBD2 handle 多一层
filesystem-owner 配合语义。因此 nested error 会恢复进入 inner scope 时的 running update、
revoke 与 touched-block snapshot，并丢弃失败 scope 产生的 device-cache alias；outer 已完成的
update 和 credit accounting 保持可用。第二个回归在 inner 写入后注入 typed I/O error，验证
inner block 不会进入最终 commit，而 outer 随后仍能更新并提交自己的 block。

本检查点只清除 exclusive core 中的 nested same-owner 子项，不声称实现跨 task 并发。
后续 7.48 已补齐普通 start 的最大事务空间保证与 best-effort credit extend primitive，7.50
进一步补齐 reserved handle 的 transfer/free/start 状态机；通用 restart、显式 locked/barrier 生命周期
和真正的 concurrent running transaction attachment 继续保持红色。

### 7.44 HTree hash 与 checked lookup 检查点

Linux v7.1 `fs/ext4/hash.c:1-322` 的目录哈希不是通用 digest crate 的直接调用：legacy
hash 有 signed/unsigned byte 两种历史语义，half-MD4 每 32 bytes 更新四字 seed，TEA 每
16 bytes 更新 state，二者都返回 major/minor；全零 UUID seed 必须替换为 ext4 固定 seed，
major 最低 collision bit 清零且保留 EOF sentinel。`fs/ext4/namei.c:771-932` 又规定 root
只保存 base version 0/1/2 或 fscrypt SIPHASH 6，unsigned policy 来自 superblock flag；root
depth、count/limit 与每级 block path 在选择 leaf 前验证。

旧 Rust 实现对三种算法分别使用 djb2、LCG 和四轮 xor 占位逻辑，unsigned 版本与未知版本
全部返回 0。parser 则把含 255-byte name array 的 Rust `repr(C)` 结构大小当成 wire size，
甚至用 `dot.inode` 和 `dotdot.inode` 相加计算 root info offset，因而固定的合法 Linux root
稳定返回 `CorruptedHashTree`。两个确定性红测分别用 e2fsprogs `debugfs dx_hash` reference
vector 和 4 KiB wire root 固定这些失败。

当前独立 Rust 实现提供 typed `DirectoryHash { major, minor }`，覆盖 default/UUID seed、ASCII、
UTF-8 高位 byte 与 signed/unsigned 0..5 全部参考向量；SIPHASH 因尚无 fscrypt directory key
编排而返回 typed unsupported，不能伪造 hash 0。root parser 使用固定 0/12/24/32 offset，
internal node 使用 0/8 offset，并按动态 block size、metadata checksum dx tail、28-bit logical
block mask 校验 limit/count、排序和引用范围。独立复核确认 Linux `dx_get_block()` 保留
28 bit；确定性 parser 红测已把错误的 24-bit mask 校正为 `0x0fff_ffff`，并补齐 64 KiB index
fake-dirent 的 compact `rec_len` 解码。lookup 从 root 取得 version/depth，检查 index/leaf
checksum 与重复 block path；启用 `metadata_csum` 的 leaf 缺少完整 dirent tail 会按 Linux 拒绝，
filesystem I/O/checksum cause 不会被 linear fallback 吞掉。
mount negotiation 同时拒绝 SIPHASH 6 和超范围的 default hash。RW indexed mount 若尚未
记录 signed/unsigned policy，则持久化本地 Linux 7.1 reference architecture 使用的 signed
policy；RO mount 不修改磁盘。这样 core 不会把 C compiler 的 plain-char signedness 当成隐式
OS capability，已有显式 unsigned flag 的镜像仍严格走 unsigned 版本。

真实 differential 由 `mkfs.ext4` 创建 64 MiB 镜像，`debugfs` 写入 800 个 entry，再由
`e2fsck -D` 建立 HTree；rsext4 可查到最后一个 leaf 的 payload，unmount 后 `e2fsck -fn`
保持 clean。后续 collision fixture 令 target 只存在于 continuation leaf：旧实现只查首 leaf 后
稳定返回 `CorruptedHashTree`，当前 frame path 按 `(next_hash & !1) == target_hash` 推进，并在
当前 node 耗尽时回溯 parent、重读下一 index chain。跨 parent 的定点 I/O failure 保留 typed
I/O cause；`DirEntryIterator` 的第二项也由错误的 `rec_len` 修正为真实 byte offset，避免命中后
rename/unlink 修改错误位置。写侧 insert/split/index growth 的后续检查点见 7.45，indexed delete
见 7.46；HTree readdir 见 7.47；casefold/fscrypt name preparation 或完整 `LARGEDIR` mutation
继续登记为红项。

### 7.45 HTree insert、leaf split 与 index growth 检查点

Linux v7.1 `fs/ext4/namei.c:1280-1359,1843-2032,2473-2650` 把 HTree 写入分成三个
不可拆散的状态转换：先把 active dirent 以稳定 hash 顺序建立 map，并保留其原始磁盘
`rec_len`；leaf 满时按原始长度从高 hash 端累计到半块附近，再把两侧压缩为最小长度并重建
dirent checksum tail；最后把
`hash2 + continued` separator 插入父 index。同 hash 正好跨 split point 时 `continued=1`，否则
lookup collision chain 会漏项。父 index 满时先平分 internal node 并向上提升 separator；root 满
时把原 entries 搬到新 internal node，root 缩为单 entry 且 `indirect_levels++`，然后重新 probe。

旧实现无 HTree mutation：无论 existing leaf 是否有空间，都会线性写目录块并清除
`EXT4_INDEX_FL`；leaf 满时也只追加 classic block。因此第一条 Linux image 红测在 portable
owned API 插入后稳定发现 index flag 丢失。当前写侧先从 checked root/version/path 选择 leaf，
有空间时原位拆分 dirent slack；无空间时由纯 Rust planner 重算 hash、稳定排序、按 Linux
record-size 规则取 split point并写 continuation separator。64 KiB compact `rec_len` 的 encode 与
decode 共用 checked codec，leaf、root 和 internal node 分别重建 Linux checksum tail。

同一 filesystem metadata transaction 拥有新 leaf/index block 分配、extent mapping、inode
size/blocks、两侧 leaf、父 index、bitmap/GDT/superblock 和 inode-table publication。真实
`mkfs.ext4 + debugfs + e2fsck -D` fixture 连续插入 1000 项覆盖 leaf split；9000 个 255-byte
附近长名称进一步填满 root、触发 root promotion 与 internal split，Linux `htree_dump` 最终
报告 `Indirect levels: 1`，两者均经 `e2fsck -fn` clean。故障测试先用同镜像副本校准第一次
leaf split 的确切序号，再关闭 journal 直写并在包含目标 dirent 的 block image 已写后返回 I/O
error；重挂载验证名称不存在、旧项可查、directory size/blocks/flags 以及 free block/inode 全部
恢复，避免随机 UUID/hash seed 或固定序号造成概率性证明。

单块 linear directory 首次无空位时不再追加普通第二块，而是按 `make_indexed_dir()` 在同一
transaction 中把 block 0 重写为 dx root、追加并平衡两个 leaf、设置 `INDEX_FL` 后插入触发项。
真实 Linux image 的 owned API 红测由旧实现的 flag 未设置转绿，Linux `htree_dump` 可解析全部
长名称且 `e2fsck -fn` clean。conversion 的 direct post-write fault 也经同镜像校准触发点，重挂载
恢复单块 linear size/blocks/flags、free counters 与旧名称集合，并逐字匹配转换前的 block 0。
这里的 rollback owner 不是 `make_indexed_directory()` 局部补偿：外层
`with_metadata_transaction()` 恢复 superblock/GDT/bitmap/inode/data cache snapshot，关闭 journal
时的 JBD2 direct handle 另为每个 metadata home block 保存 preimage 并在 error 上逆序写回。因此
block allocation、extent mapping、root/leaf publication 与 inode update 只能在该外层 transaction
中调用，不能把 helper 暴露为可脱离事务使用的公共 API。

通用 planner 已覆盖三层 path 中“拆最深满 node”“向上寻找首个有空间 parent”以及“所有 parent
均满时提升 root”的选择，并以单元测试固定 Linux `dx_set_count()`/`dx_set_limit()` 覆写 right
node 首项 hash、separator 只存在于 parent 的 wire 语义。默认非 `LARGEDIR` image 的 root growth
仍只验证到 `Indirect levels: 1`；启用 feature 后的真实二级 internal image、磁盘 checksum/reprobe、
rollback/credit matrix 与 fscrypt SipHash/casefold prepared name 继续登记为红项。

### 7.46 HTree leaf-only delete 检查点

Linux v7.1 `fs/ext4/namei.c:2657-2746` 的 `ext4_generic_delete_entry()` 与
`ext4_delete_entry()` 不删除 dx entry、不释放空 leaf，也不重平衡或降低 HTree 高度。有前驱的
目标把其 `rec_len` 合并进前驱并清零整个旧 record；块首目标保留原 `rec_len`，但清零 inode、
name length、file type 与 payload。随后只重算 leaf dirblock checksum 并在原 namespace
transaction 中发布。因此旧台账中的“delete rebalance”不是 Linux parity 目标，已更名为
leaf-only delete。

旧 core 仅清除目标 inode，留下独立 free record，合法但不等同 Linux 的物理布局。确定性单测先
固定前驱 `rec_len` 未合并的失败，再由同一测试验证合并、完整 wipe 和 64 KiB compact
`rec_len` 的块首删除。真实 Linux image 进一步在 HTree leaf 中执行 unlink 与 rename replacement，
重挂载后核验全部剩余名称、replacement inode、`INDEX_FL` 和 directory size；删除所有真实名称
后仍保留原 index/leaf allocation，Linux `htree_dump` 可解析且 `e2fsck -fn` clean。

### 7.47 HTree readdir cookie 与 OS 游标检查点

Linux v7.1 `fs/ext4/dir.c:346-410,526-637` 以 `(major, minor)` 表示 HTree 位置：64-bit
cookie 为 `((major >> 1) << 32) | minor`，EOF 为 `2^63-1`；同一完整 hash 的碰撞项共享
ABI cookie，由每个打开目录的 `extra_fname` 私有状态保存精确续读位置。外部 `llseek` 会使该
私有状态失效并从 cookie 解码后的碰撞链首项重建。`fs/readdir.c:341-410` 还规定前一条记录的
`d_off` 在下一条 emit 时覆写，最后一条使用最终 `ctx->pos`，因此 filesystem sink 必须返回“下一
候选项”的 cursor。

旧 core 对 indexed directory 仍按 logical block byte offset 扫描，并会把 root/internal dx metadata
当普通 dirent 解析；ax-fs-ng 又强制把合法 ext4 raw name 转成 UTF-8。真实 `mkfs.ext4 + debugfs +
e2fsck -D` 的 802-entry fixture 先稳定返回 `Linear` cursor，成为确定性红测。当前 core 使用
`DirectoryCursor::{Start, Linear, HTree, End}`，验证 root/internal/leaf checksum 与 block 引用，只
收集 leaf active record；dot/dotdot 固定 hash 0/2，其余名称按 root effective hash version 计算
`(major, minor)`，稳定排序后为完整 hash 碰撞分配 ordinal。readdir 以 cursor 的 major hash 重新
probe HTree，只收集当前 index range 及 low-bit collision-continuation leaves，并用一项 lookahead
构造下一候选项 cursor；不再为小批读取扫描并排序整棵目录树。一次读取 802 项与逐项续读的名称
序列完全一致，9,802 项二级 HTree 以 127 项分批读取不丢项，任何 cursor kind 混用都返回 typed
invalid input。旧实现的一项读取稳定触发 8 次设备读取；同一 Linux image 回归现要求不超过 4 次，
固定了“禁止完整扫描”的确定性红绿证据。

`axfs-ng-vfs::DirectoryCursor` 把 ABI-visible `offset` 与 backend-private `continuation` 分开，sink
名称改为 raw bytes。ext4 adapter 实现 Linux 64-bit cookie/EOF codec，把 collision ordinal 仅放入
continuation；Starry 的 open-directory description 保存完整 cursor，只有输出 buffer 写入用户地址
成功后才提交，从而避免 `EFAULT` 跳项，目录 `lseek` 以新 visible offset 重建并清零 continuation。
ArceOS materialized directory 同样改为 peek/commit，buffer 容量不足不再提前消费未输出项，
`d_ino`/`d_off` 不再伪造为 1/0。ax-fs-ng adapter 每次最多向 core 请求 128 项，并在释放
filesystem sleepable mutex 后才调用 sink。

Linux `dir_private_info` 不跨 hash range 保留 dx probe path；它为一个 open description 缓存当前
`ext4_htree_fill_tree()` 产生的排序范围，并在 inode version 变化或 `llseek` 后丢弃。portable core
现以 opaque `DirectoryReader` 保存相同层次的派生范围缓存：显式 `DirectoryCursor` 始终是唯一
语义位置，缓存可在 I/O/copy-to-user 失败后保留或任意丢弃，重试仍从调用方 cursor 精确定位。
`axfs-ng-vfs` 只声明 opaque per-open state capability；ax-fs-ng 在 sleepable filesystem mutex 内
填充范围、释放锁后调用 sink；Starry 的 `DirectoryPosition` 由 sleepable mutex 串行化完整
`getdents64`/`lseek` 状态转换，并通过共享 `Arc<Directory>` 保持 dup/fork 的 open-file-description
语义。确定性红测先在已缓存首个 range 后 unlink 并 reap 后续项：去掉 i_version cache clear 会
稳定重新返回已删除名称，恢复失效逻辑后同一 reader 从原 cookie 续读且镜像经 `e2fsck -fn` clean。

Linux `fs/ext4/namei.c:2148-2156,2675-2698,3641-3665` 在目录项插入、删除和替换后递增目录
inode version；`fs/ext4/inode.c:4822-4832,5453-5461` 始终持久化低 32 位，仅在
`i_version_hi` 落入 `i_extra_isize` 声明的范围时读写高 32 位。确定性红测先证明旧 core 在
create 后父目录版本保持不变；当前所有 linear/HTree create、link、unlink、rmdir、rename mutation
统一经 parent metadata update 递增 on-disk version，`InodeInfo::change_attribute` 以稳定 DTO 暴露
完整可持久化值。`axfs-ng-vfs::DirectoryCursor` 另存生成 continuation 时观察到的 change attribute；
ext4 adapter 在每个 bounded batch 前重新读取当前值，发生 mutation 时保留 ABI-visible cookie、
清零 collision continuation 并从当前 hash range 重建。外部 `llseek` 生成的 cursor 没有观察版本，
首次续读会绑定当前值。同父 rename 的 add/replace 与 delete 可像 Linux 一样分别递增，不人为按
syscall 去重。失败 mkdir 回归同时验证未发布 parent mutation 不改变低/高版本字段。

Linux `ext4_dir_llseek()` 对 HTree 把 hash-space EOF 同时作为 `SEEK_END` origin 与最大合法
offset，非 HTree 才使用 inode byte size。旧 Starry 一律以 `Location::len()` 处理目录
`SEEK_END`：确定性 QEMU 红测中，indexed directory 返回 `st_size`，随后 `getdents64` 又返回
24 bytes 而非 EOF。core 现通过 `directory_end_cursor()` 区分 `End` 与 linear byte-size，ext4
adapter 将 `End` 编码为 64-bit Linux EOF `2^63-1`，VFS 只转发 typed capability，Starry 负责
checked `SEEK_SET/CUR/END` 算术并在 seek 后清除 backend-private continuation。同一 system case
现为 159/159，通过 EOF、rewind 和再次读取。

固定 x86_64、CPU 2、release、memory backend、4 KiB block、`metadata_csum+64bit+journal`、
800 个普通项、3 次预热和 10 次测量；计时区间只包含完整 HTree readdir。这里的基线
`6db697494` 是同一 PR 内的全树扫描检查点，只证明本次增量遍历的因果收益，不替代最终相对
`6e27704c4` dev 的全量性能门槛。

| batch | 指标 | `6db697494` | `d5cacf015` | 变化 |
| --- | --- | ---: | ---: | ---: |
| 1 | median | 68,644,211 ns | 8,370,758 ns | -87.81% |
| 1 | p95 | 74,392,227 ns | 9,328,437 ns | -87.46% |
| 128 | median | 701,628 ns | 183,039 ns | -73.91% |
| 128 | p95 | 804,165 ns | 239,969 ns | -70.16% |

同一环境随后把 `7bb3b194d` 的“每次调用重新 probe 当前 range”与实现提交 `336b58863` 的
per-open hash-range cache 精确 A/B；batch=1 的 syscall 风格
小缓冲路径不再每条记录重新读取、解析和排序当前 leaf range，batch=128 同样减少相邻调用的
重复 probe；cache 是可丢弃派生状态，不改变 `calls` 或 cursor 语义。

| batch | 指标 | `7bb3b194d` | `336b58863` | 变化 |
| --- | --- | ---: | ---: | ---: |
| 1 | median | 8,426,396 ns | 230,818 ns | -97.26% |
| 1 | p95 | 10,088,703 ns | 233,711 ns | -97.68% |
| 128 | median | 181,615 ns | 129,908 ns | -28.47% |
| 128 | p95 | 189,715 ns | 132,837 ns | -29.98% |

当前 Starry 的 x86_64/riscv64/aarch64/loongarch64 都是原生 64-bit ABI，syscall table 仅注册
`getdents64`，没有 32-bit task/compat `getdents`，因此“32-bit getdents cookie”在当前交付边界
明确为 N/A；若未来引入 compat task ABI，必须新增独立 `linux_dirent32` checked narrowing，并对
inode/cookie 溢出返回 `EOVERFLOW`，不能截断 64-bit cursor。每个 open description 的 HTree
hash-range cache 已按上述 Linux owner 完成；Linux 本身不会跨 range 保留 dx path，因此不把长期
path cache 误列为 parity 目标。casefold/fscrypt prepared-name hash 仍在后续台账中。

### 7.48 JBD2 transaction limit、start reservation 与 credit extend 检查点

Linux v7.1 `fs/jbd2/journal.c:1412-1452` 把 `j_max_transaction_buffers` 固定为
`(j_total_len - j_fc_wbufsize) / 3`，再为每个 transaction 预留一个 commit block 与覆盖最大
transaction 的 descriptor blocks；`fs/jbd2/transaction.c:190-303` 从中扣除 bookkeeping 得到
user credits。更关键的是，handle 在第一次 dirty 任何可能仍被旧 transaction checkpoint 拥有的
buffer 前，必须先保证 `jbd2_log_space_left() >= j_max_transaction_buffers`。commit path 因锁顺序
不能在写 transaction 时再强迫 checkpoint，这个 start-time reservation 是避免 checkpoint/commit
相互等待的正确性条件，不只是吞吐优化。

旧 core 反向求解“整个剩余 ring 最多能塞多少 payload”，64-block csum-v3 journal 因而给出
61 credits；加入 Linux 期望 19 credits 的同一测试后，旧提交 `09460a38d` 稳定得到
`left: 61, right: 19`。当前实现用固定总 journal geometry 计算 `/3` 上限，再扣动态 block size、
checksum 与 64-bit tag 对应的 descriptor/commit overhead；64-block csum-v3、4096-block/1 KiB
csum-v3 与 16-block test journal 分别固定为 19、1341、3 user credits。`s_first` 只界定可环绕的
普通 log records，不从 Linux 的 `j_total_len / 3` 再扣一次；若极端 geometry 令真实 ring 无法提供
一个最大 transaction，start 会在 dirty 前返回 typed `NoSpace`。fast commit feature 当前仍由
feature negotiation 明确拒绝，因此 `j_fc_wbufsize` 为零；实现 fast commit 时必须先切出该区域，
不能让普通 transaction 借用。

新 handle 与无显式 handle 的 metadata write 现在共用同一 first-dirty gate。确定性测试先连续
提交三个最大 transaction 填满 15-record test ring，再开始第四个 transaction：operation closure
执行前最老 checkpoint owner 已被回收，可用记录恢复为完整 5-record maximum；无显式 handle 的
单块 metadata write 也执行同一 reservation。`extend_transaction_handle()` 是 crate-private、
best-effort 且不等待 log space：在已经通过 start gate 的 running transaction 内只增加 reservation，
超过固定 transaction 上限则保持原 credits 并返回 `RestartRequired`。journal-disabled direct owner
同步扩大其 rollback preimage budget，不伪造 JBD2 transaction 状态。standalone revoke 也在修改
running revoke table 前通过相同 gate；极端 `s_first` fixture 在旧实现稳定错误成功，当前返回
typed `NoSpace` 且不发布 revoke。

采用 `/3` 后，原 `extent_restart` fixture 为保持 3/7/8 user-credit 边界分别把 test journal 从
6/10/11 blocks 调整为 15/27/30 blocks；`file_operations` 的 3-credit 负例同样从 6 调整为
15 blocks。更大的合法 ring 不再人为迫使每次 owner-level restart
立即 checkpoint，因此三条恢复测试改在第一个 commit block 已写入、journal tail 更新前断电，
重挂载仍必须通过 replay 与 orphan/current-tree recovery 收敛。这不是放宽断言，而是把故障点固定
在 Linux 明确的 durable commit boundary，避免依赖旧的非 Linux ring geometry。

性能 A/B 固定 CPU 2、`powersave` governor、release、memory backend、4 KiB block、20 MiB、
3 次预热和 20 次测量；baseline 为本检查点前的 `09460a38d913`，implementation 为
`222da1964`。普通 workload 不会
接近新的 `/3` 上限，因此本轮只要求保持在门槛内，不把“没有触发 checkpoint”误称为性能优化。

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,332,353 ns | 6,333,900 ns | +0.024% | 6,452,026 ns | 6,457,446 ns | +0.084% |
| sequential read | 5,937,828 ns | 5,920,991 ns | -0.284% | 6,123,831 ns | 6,309,344 ns | +3.029% |
| sequential unmount | 17,977 ns | 17,038 ns | -5.223% | 18,945 ns | 18,008 ns | -4.946% |
| dirty sync | 7,850 ns | 7,972 ns | +1.554% | 9,670 ns | 8,498 ns | -12.120% |
| clean sync | 214 ns | 207 ns | -3.271% | 306 ns | 224 ns | -26.797% |
| sync-cycle unmount | 5,018 ns | 5,173 ns | +3.089% | 5,436 ns | 5,533 ns | +1.784% |

六项 median/p95 全部满足相对本 PR 前一检查点的 5%/10% 门槛；相对冻结 dev 的全局 dirty-sync
硬红项仍按 7.35/7.42 保留，本次局部 A/B 不覆盖也不豁免该最终门禁。

本检查点不宣称完整 `jbd2_journal_restart()`：现有 truncate/reap/punch 由 filesystem owner 在每个
已提交 chunk 后重新规划，通用 closure 不能在 prefix commit 后继续沿用外层全量 rollback snapshot。
reserved handle 的 transfer/free/start 与一半上限由后续 7.50 补齐；locked transaction/concurrent
attachment 继续登记为红项。

### 7.49 JBD2 revoke requested/remaining 与 descriptor credit 检查点

Linux v7.1 `fs/jbd2/transaction.c:470-500,642-729` 与 `fs/jbd2/revoke.c:376-401` 将
metadata buffer credits、revoke descriptor buffer credits 和 revoke record credits 分开：start 把
`ceil(revoke_records / j_revoke_records_per_block)` 加入 total buffer credits，同时保存 requested 与
remaining revoke records；每个实际 revoke 只消耗 remaining record。extend 只为跨过新的 revoke
descriptor 边界增加 buffer credit，stop 再按本 handle 实际使用的 revoke 数修正 transaction
outstanding credits。Linux `fs/jbd2/journal.c:1397-1410` 还规定每块记录数必须同时考虑 32/64-bit
block number 与 CSUM_V2/V3 tail。

旧实现把每个 distinct revoke block 当一个普通 metadata credit。加入 typed request 后，同一确定性
测试申请 1 个 metadata block 和恰好一整个 4 KiB、64-bit、CSUM_V3 revoke block 的 509 条记录；
沿用旧计数的实现稳定在 operation closure 前返回
`NoSpace(op=jbd2:handle_credits)`。当前 `TransactionCredits` 保持 metadata/revoke 两个独立维度，
该场景只预留 1 个 metadata credit 与 1 个 revoke descriptor credit，commit 的实际 ring footprint
固定为 revoke record、metadata descriptor、payload、commit 共 4 records。

`ActiveJournalHandle` 现在保存 `revoke_credits_requested` 与 `revoke_credits_remaining`。未申请 revoke
或消耗超过 requested 时，在修改 running revoke table 前返回 `NoSpace(op=jbd2:revoke_credits)`；
outer operation error 恢复完整 queue/revoke snapshot，nested error 还恢复 scope 进入时的 remaining
credits。extend 边界测试以 2 个 metadata credits 加 508 条 revoke 起步：扩到 509 条仍复用同一
descriptor 并返回 `Extended`，第 510 条需要第二个 descriptor、超过 3-credit fixture 上限后返回
`RestartRequired`，原 reservation 保持不变。

真实 owner 已同步迁移：xattr external-block replacement 按可能释放的旧 block 申请 revoke record；
extent shift/removal 与 legacy indirect truncate/reap/punch 分别携带 metadata upper bound 和 detached
metadata record count，restart planner 用动态 32/64-bit checksum-aware descriptor cost 比较 journal
user capacity。原 restart fixture 不再依赖“每条 revoke 一个 block”的错误预算：legacy 单 chunk 的
6-credit ring 使用 24 blocks，extent 单 chunk 的 8-credit ring 使用 30 blocks；四个 power-cut/replay
case 与 restart 后 allocation/mapping/e2fsck 断言保持不变。

性能 A/B 以本检查点前的 `d37d299c1` 为 baseline、`a3b761580` 为 implementation，固定 CPU 2、
release、memory backend、4 KiB block、20 MiB payload。常规 sequential 与 sync-cycle 各执行 3 次
预热、50 次测量，clean-sync 单次仅约 200 ns，
多轮 50-sample A/B 的差值方向会随 timer/scheduler noise 翻转；因此没有挑选其中一轮，而是对两个
revision 对称扩为 10 次预热、500 次测量，最终 sync 判定使用
这组完整扩样结果。

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,264,560 ns | 6,256,878 ns | -0.123% | 6,871,347 ns | 6,862,395 ns | -0.130% |
| sequential read | 5,989,700 ns | 5,953,986 ns | -0.596% | 7,547,108 ns | 6,891,809 ns | -8.683% |
| sequential unmount | 17,650 ns | 17,587 ns | -0.357% | 19,312 ns | 19,065 ns | -1.279% |
| dirty sync（500 次） | 7,953 ns | 7,767 ns | -2.339% | 10,000 ns | 9,572 ns | -4.280% |
| clean sync（500 次） | 206 ns | 191 ns | -7.282% | 266 ns | 237 ns | -10.902% |
| sync-cycle unmount（500 次） | 5,308 ns | 5,095 ns | -4.013% | 6,671 ns | 6,220 ns | -6.761% |

六项 median/p95 全部满足相对本 PR 前一检查点的 5%/10% 门槛；该局部 A/B 不覆盖也不豁免
7.35/7.42 相对冻结 dev 的全局 dirty-sync 红项。

本检查点本身不顺带实现 reserved child handle、跨执行流 concurrent attachment 或通用
`jbd2_journal_restart()`；reserved child handle 由后续 7.50 以独立 ledger/ownership 状态机补齐，
其余两项继续保持红色。

### 7.50 JBD2 reserved handle ownership 检查点

Linux v7.1 `fs/jbd2/transaction.c:184-619,698-815,1883-2025` 把 reserved handle 定义为尚未
附着 transaction 的 credits owner。普通 parent start 同时把 `blocks + rsv_blocks` 计入 running
transaction outstanding，并把 `rsv_blocks` 加入 journal-wide `j_reserved_credits`；单项 reservation
与全局 reservation 都不能超过 user transaction capacity 的一半，parent stop 若未 transfer token
则自动 unreserve。调用方移交后必须清空 parent 的 `h_rsv_handle`，随后 `start_reserved()` 消费 token
并附着 running transaction；该路径不能等待 commit、checkpoint 或 log space。Linux ext4 的真实
owner 是 delayed-allocation writeback：`fs/ext4/inode.c:2920-2944` 创建 reservation，
`inode.c:2396-2405` 转移给 `io_end`，`fs/ext4/extents.c:5089-5117` 在 data I/O 完成后启动 token，
将 unwritten extent 转为 initialized。

为固定旧实现差异，64-block CSUM_V3 journal 的 user capacity 是 19：测试让 parent 申请 1 个
metadata credit 和 10 个 reserved credits。旧 typed token 骨架稳定进入 operation closure 并返回
token；Linux 半 transaction 上限只允许 9，当前同一测试在 closure 前返回
`NoSpace(op=jbd2:reserved_credits)`。第二组状态机回归覆盖 1+1 credits 的 parent/child 在同一
transaction sequence 内写两个 metadata blocks，证明 `start_reserved` 不会隐式 commit；另覆盖
ordinary handle 对 detached reservation 的 capacity 保留、两份 reservation 聚合超过一半时返回
typed `Busy`、parent error 自动释放、显式 free、sticky abort 时 failed start 消费 token，以及
unmount/reinstall/disable journal 不得遗忘 live token。

Rust token 是 non-copy `ReservedJournalHandle`，只携带私有 typed ID；credits 与 buffer-cost 真相只
存在 `Jbd2Dev` 的私有 ledger，外部调用方不能构造或复制 token，当前唯一 crate-private owner 也不会
把它带出所属 mount。ledger 中不存在 ID 时返回 typed invalid-owner 错误；这里不把单进程自增 ID
夸大为跨 mount 的全局身份保证。Linux 在并发 task 等待其他 reservation 释放；portable core 由
adapter 以 sleepable mutex 独占进入，若在 guard 内等待另一个 owner 永远无法获得 `&mut Ext4`，所以
aggregate half-limit 冲突返回 typed `Busy` 交给 adapter 释放 guard 后重试。这是 OS 无关
capability/ownership 边界，不把 task/waitqueue 注入 core。

首个生产 owner 对齐 unwritten write：core 先扫描所有 planned run，仍以普通 handle 逐段发布
still-unwritten split；最后一个 prepare handle 在 finish footprint 同时满足“单项不超过一半”和
“parent + child 不超过 user capacity”时创建 token。data I/O、leaf snapshot 或 mapping validation
失败会显式 free；I/O 成功后 conversion 消费 token，把所有 prepared leaf 与 inode size/metadata
作为一个 no-wait transaction step 发布。超半 transaction 的大 conversion 保留既有普通 bounded
handle 路径，不以 reservation 绕过容量上限。现有 external-leaf finish fault、inline-root split、
partial write、preallocation、truncate/punch/zero/insert/collapse 的确定性用例保持原断言。

这一检查点还把 `fs/jbd2/transaction.c:184-815,1883-2025` 从 whole-file coarse 清单拆为 symbol-level
segment，绑定 Rust owner、差异理由和 `jbd2-handle-credits` 测试 ID；其余区间仍明确保持 coarse。
`T_LOCKED/T_SWITCH` barrier、真正跨执行流 attachment 与通用 in-closure `journal_restart` 尚未完成，
不能因为 adapter 当前串行就标 N/A。

独立复核同时发现 bulk read 仍用 `block_size * count` 计算 byte footprint，极端输入在 debug build
panic。确定性红测固定 `usize::MAX * 2`，当前 read/write 共用 `checked_block_bytes()` 并返回 typed
`Overflow`，不让 host 字长或编译模式改变错误传播。

性能 A/B 以本检查点前的 `4d1f03698` 为 baseline、`e05159fa1` 为 implementation，固定 CPU 2、
`powersave` governor、release、memory backend、4 KiB block、20 MiB payload；sequential 与 sync-cycle
各执行 3 次预热、50 次测量，本轮所有 median/p95 都直接采用
完整样本，没有删除或选择性复测：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,293,462 ns | 6,310,666 ns | +0.273% | 7,703,955 ns | 6,964,578 ns | -9.597% |
| sequential read | 5,957,194 ns | 5,943,815 ns | -0.225% | 8,106,962 ns | 7,360,879 ns | -9.203% |
| sequential unmount | 17,569 ns | 17,936 ns | +2.089% | 26,805 ns | 19,557 ns | -27.040% |
| dirty sync | 8,247 ns | 7,961 ns | -3.468% | 12,509 ns | 9,887 ns | -20.961% |
| clean sync | 233 ns | 232 ns | -0.429% | 341 ns | 283 ns | -17.009% |
| sync-cycle unmount | 5,366 ns | 5,123 ns | -4.529% | 7,992 ns | 5,738 ns | -28.203% |

六项 median/p95 均满足相对本 PR 前一检查点的 5%/10% 门槛。该局部 A/B 证明 reservation ledger
与第一个 unwritten owner 没有引入现有 host workload 回退；它不覆盖也不豁免 7.35/7.42 相对冻结
dev 的全局 dirty-sync 红项。

### 7.51 ax-fs-ng readonly lifecycle 与 sleepable owner 检查点

旧 adapter 在 `sync_to_disk()` 与 `shutdown_filesystem()` 取得 filesystem mutex 前检查复制的
`readonly` bool 并直接返回成功。这绕过 owned core 的两个不同不变量：readonly `Ext4::sync()` 仍需
执行真实 device flush；readonly `Ext4::unmount()` 虽不写盘，仍需发布 UnmountStarted/Unmounted 并
把 mount owner 转为 unmounted。共享只读镜像的确定性红测先得到 flush 计数 `3 == 3` 而非预期
`3 -> 4`，第二个红测证明 shutdown 后 `remount(options)` 仍错误成功。

当前 RO/RW 两条路径都先取得同一个 `SleepMutex<Ext4State>`，再分别调用 owned core 的 typed
`sync()` / `unmount()`；adapter 不再复制 cache、journal 或 lifecycle 状态机。修复后同一测试观察到
一次真实 flush，并由 `Busy(op=remount:unmounted)` 证明 owner 已终止。完整
`FilesystemOps::is_readonly()` 也不再读取 mount-time 副本，而是在同一 mutex 下查询 core options；
确定性 remount 回归先证明旧 adapter 在 core 已转为 RO 后仍返回 RW，同一测试现随 core 状态更新。
`cargo test -p ax-fs-ng --features ext4` 为 92+3 全绿，`cargo xtask clippy --package ax-fs-ng`
为 6/6 全绿。该修改只删除 readonly fast-return 与重复状态源，冻结 host harness 使用 writable mount，
因而没有把无关的 sequential A/B 伪装成 readonly lifecycle 性能证据。

### 7.52 JBD2 transaction restart 检查点

Linux v7.1 `fs/jbd2/transaction.c:642-697,698-742,743-806,807-815` 依次定义 best-effort
extend、旧 handle stop accounting、`jbd2__journal_restart()` 与 metadata-only wrapper。restart 先让
旧 handle 脱离 transaction，再请求旧 TID commit，重建普通/revoke credits，最后把同一个 handle
附着到可保证新预算的 transaction；attached reserved handle 的所有权必须跨越这个切换。

确定性红测先只加入 `restart_transaction()` 入口而让它继续调用普通 handle start；
`transaction_restart_switches_before_attaching_the_next_handle` 稳定失败于 transaction sequence
`left: Some(1), right: Some(1)`，证明旧实现只是依赖下一次 admission 的容量副作用，没有表达 restart
边界。当前实现要求旧 scoped handle 已结束；若仍有 active journal/direct handle，typed
`Busy(op=jbd2:restart_with_active_handle)` 在 operation 开始前返回。journal 模式先提交旧 running
transaction，再建立新 scoped handle；direct/no-journal 模式没有虚构 commit。另一个回归把 non-copy
reserved token 留在 journal 私有 ledger 中，切换后再消费到新 transaction，证明 restart 不遗失或
复制 detached owner。

Linux 的 `jbd2_log_start_commit()` 可以只发起异步请求；当前 portable core 由 adapter 的 sleepable
mutex 以 `&mut Ext4` 独占执行，没有可并发推进 commit 的 OS task。这里选择同步完成旧 commit 后再
附着下一 scope，提供更强但兼容的持久化顺序，同时不向 core 注入 task、waitqueue 或 mutex。后续
`T_LOCKED/T_SWITCH` 切片仍需显式表达 barrier/admission phase，不能用当前串行执行把 Linux 状态机
标成不适用。

filesystem 层把 snapshot capture/restore 抽成单一 owner，并增加
`restart_metadata_transaction()`：它只快照即将开始的新 step；先前 transaction 可能已经 durable，
失败时若回滚整个多 transaction 操作会让内存与可 replay 的磁盘进度分叉。extent 与 legacy
truncate/reap/punch 的首个 chunk 继续允许加入已经建立的 truncate/orphan intent；第二个及后续
chunk 明确走 restart，pointer/extent edit、inode image、bitmap/GDT、superblock 与 cache invalidation
仍由各自完整 step 共同拥有。

源码映射把原 `transaction.c:642-815` coarse segment 无缺口拆为四个 symbol-level segment，分别绑定
extend、stop、restart 和 barrier precondition 的 Rust owner/差异理由/测试 ID。三个 journal unit
回归覆盖 sequence switch、reserved owner 和 active-handle rejection；既有 `extent_restart` 八个
bounded restart/power-cut case 覆盖真实 extent/legacy owner、replay、orphan 与最终 accounting。

性能 A/B 以本检查点前的 `c3a619d97` 为 baseline、`6513a1f98` 为 implementation，固定 CPU 2、
`powersave` governor、release、memory backend、4 KiB block、20 MiB payload。首轮 3 次预热、50 次
测量中，sequential clean-unmount p95 从 20,175 ns 增至 23,256 ns（+15.27%），虽然 median 为
-1.61%，仍按门槛判红。随后两端对称扩为 10 次预热、200 次测量，sequential 六项全部过门槛；但
sync-cycle unmount p95 仍从 6,125 ns 增至 7,270 ns（+18.69%）。最终采用 10 个交错批次，每批每端
3 次预热、20 次测量，奇偶批次反转执行顺序；合并的 200+200 个 sync-cycle 样本消除按 revision
顺序累积的主机尾延迟漂移。没有删除首轮、扩样轮或交错轮的异常结果，也没有只重跑 implementation。

最终判定使用 200+200 个 expanded sequential 样本与 200+200 个 interleaved sync-cycle 样本，800
条原始记录汇总如下：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,337,896 ns | 6,307,339 ns | -0.482% | 6,941,844 ns | 7,075,199 ns | +1.921% |
| sequential read | 6,036,110 ns | 5,948,381 ns | -1.453% | 7,201,588 ns | 7,532,967 ns | +4.601% |
| sequential unmount | 17,781 ns | 17,521 ns | -1.462% | 19,115 ns | 19,546 ns | +2.255% |
| dirty sync | 8,052 ns | 7,951 ns | -1.254% | 10,047 ns | 9,962 ns | -0.846% |
| clean sync | 219 ns | 215 ns | -1.826% | 287 ns | 295 ns | +2.787% |
| sync-cycle unmount | 5,200 ns | 5,328 ns | +2.462% | 6,371 ns | 6,516 ns | +2.276% |

六项 median/p95 全部满足相对本 PR 前一检查点的 5%/10% 门槛。冻结 workload 不执行大范围
truncate/punch/reap，因此该结果只证明共享 write/read/sync 热路径没有回退，不能伪装成 restart
冷路径自身的因果性能数据。完整 `cargo test -p rsext4 --all-features` 在最终代码形状上为 277 个
unit 加全部 integration/Linux image/e2fsck 绿；`--no-default-features`、三组目标 clippy、格式、
portable-core boundary 同步通过。

### 7.53 JBD2 running/locked/switch phase 检查点

Linux v7.1 `fs/jbd2/commit.c:466-505` 在 commit thread 取得 running owner 后，先将
`T_RUNNING` 改为 `T_LOCKED`，等待 `t_updates == 0`，再进入 `T_SWITCH`；只有完成该 admission
closure 后才能把旧 transaction 转为 committing owner。`fs/jbd2/transaction.c:817-906` 分别定义
update drain、特殊操作 barrier 和 balanced unlock。reserved handle 可以加入 Locked，但 Switch 已不再
等待 handle；普通 handle 从 Locked 起就不能新加入。

确定性红测先只在 `Jbd2RunningTransaction` 加入 phase 字段，不改变旧 `mem::take`：把带一个 update
的 transaction 人工置为 Locked 或 Switch 后，`start_committing_transaction()` 两次都错误返回
`Ok(true)` 并拿走旧 owner。当前 commit start 先验证只能从 Running 进入，再显式执行
`Running → Locked → Switch`；任一非法起点返回 typed corruption 且保留原 update/owner。Switch 后
`mem::take` 创建默认 Running 的新 owner，旧 sequence/update 则只存在 committing owner。

这里没有把 Linux waitqueue、`j_state_lock` 或 barrier mutex 翻译进 portable core。所有 commit 入口
收口到 `Jbd2Dev::commit_pending_transaction()`，它在改变 phase 前拒绝 active journal/direct handle；
adapter 的 sleepable mutex 与 core `&mut` 独占保证两次 phase transition 间没有另一个 OS execution
flow 可以进入，所以 update drain 是受类型所有权证明的立即条件，而不是忙等。现有 active-handle
回归补充断言：失败的 unmount/flush 必须保持 Running，随后同一 handle 仍可发布 update。

源码映射把 `commit.c:466-537` 拆为 phase transition 与仍待细化的 Linux reserved-buffer/checkpoint
cleanup；把 `transaction.c:816-907` 拆为 wait-updates、lock-updates、unlock-updates。普通 commit phase
已实现，但 `jbd2_journal_lock_updates()` 面向 freeze/特殊操作的 reservation-drain 与 balanced barrier
owner 尚未实现，继续保持同一红测台账，不能把 `&mut` 独占误报为该 API 全语义完成。

性能 A/B 以本检查点前的 `e5f6e295a` 为 baseline、`67249506b` 为 implementation，固定 CPU 2、
`powersave` governor、release、memory backend、4 KiB block、20 MiB payload。sequential 采用 10 个
交错批次、每批每端 3 次预热与 20 次测量，得到 200+200 个样本。sync-cycle 首轮同样为 200+200；
其中只有约 240 ns 的 clean-sync 被计时噪声放大为 median +5.83%、p95 +21.10%，真正执行 commit 的
dirty-sync 已为 median +0.40%、p95 +5.97%。没有删除该首轮或单独重跑 implementation，而是继续
追加 15 个双端交错批次，将完整 sync-cycle 判定窗口扩为 500+500。

最终 1,400 条原始记录汇总如下：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,413,302 ns | 6,411,346 ns | -0.030% | 8,195,429 ns | 7,595,639 ns | -7.319% |
| sequential read | 6,656,752 ns | 6,575,437 ns | -1.222% | 8,109,444 ns | 7,989,184 ns | -1.483% |
| sequential unmount | 18,610 ns | 18,568 ns | -0.226% | 25,176 ns | 26,375 ns | +4.762% |
| dirty sync（500 次） | 8,803 ns | 8,822 ns | +0.216% | 13,131 ns | 12,689 ns | -3.366% |
| clean sync（500 次） | 247 ns | 253 ns | +2.429% | 411 ns | 437 ns | +6.326% |
| sync-cycle unmount（500 次） | 5,914 ns | 5,909 ns | -0.085% | 8,344 ns | 8,494 ns | +1.798% |

六项 median/p95 全部满足相对前一检查点的 5%/10% 门槛。phase 切换只在有 pending metadata 的
commit start 执行；clean-sync 不进入该路径，其扩样用于量化主机 timer/scheduler noise，不作为
实现收益。完整 `cargo test -p rsext4 --all-features` 在最终代码形状上为 279 个 unit 加全部
integration/Linux image/e2fsck 绿；`--no-default-features` 为 278 个 unit 加全部 integration/Linux
image/e2fsck 绿，三组目标 clippy、格式与 portable-core boundary 同步通过。

### 7.54 JBD2 unused buffer access 与 cache publication 检查点

Linux v7.1 `fs/jbd2/commit.c:506-527` 在 transaction 已进入 `T_SWITCH` 后遍历
`t_reserved_list`：未实际 dirty 的 `BJ_Reserved` 必须 refile，`b_committed_data` undo image 必须释放，
但绝不能因为 commit cleanup 而写 home block。`commit.c:528-537` 随后只回收已经异步写回的
checkpoint `buffer_head` 内存；它同样不能推进 journal tail 或丢失 replay owner。

旧 Rust API 将一个 block mutation 拆为公开的 `read_block()`、`buffer_mut()`、`write_block()`。
确定性红测先修改 block 20 的 cache image 却不调用 `write_block()`，再为 block 10 排队一项正常
journal update 并 commit。旧实现返回 `Ok(())`：commit block FUA 后的 `invalidate_cache()` 把 block 20
绕过 journal 直接写到 home；这不是普通 cache I/O failure，而是 durability owner 泄漏。

当前 raw mutable buffer 与单 block publish 不再是公开 API。crate 内的 `update_block()` 在单一 closure
中拥有 mutable image：closure 失败或 direct/journal publish 失败都会 `discard_active()`；只有成功结果
才能把 immutable copy 移交给 running transaction 或 direct device。extent node、legacy pointer、GDT、
superblock、mkfs、host integration fixture 与 axtest 已全部迁移；底层 `BlockDev::buffer_mut()` 也仅在
`blockdev` owner 内可见，其他模块无法绕开 closure 后再触发 cache eviction。commit/checkpoint 在任何 phase 或
home write 前检查 cache 不存在 unfinished edit；回放、commit、checkpoint 后只丢弃 derived clean cache，
不再以“invalidate”为名执行 home writeback。第二个回归证明 closure 返回 I/O error 后，未发布 byte
保持为零，后续独立 transaction 仍可正常 commit，journal 不被错误 abort；第三个回归在 closure 成功
后注入 direct publish failure，再调用 flush，证明失败镜像已经丢弃，不会被通用 cache writeback 重试。

源码映射将 `commit.c:506-527` 标为 Rust 表示中不适用的 `buffer_head` reservation mechanics：完成的
metadata image 直接由 transaction-owned copy 表达，未完成 image 无法从 closure 逃逸。Linux 由
`j_state_lock` 串行化的 `t_reserved_list` refile、`BJ_Reserved` write-access reservation 与
`b_committed_data` undo-image release 均没有对应的可逃逸 Rust 对象；这些对象生命周期由 closure 返回及
transaction copy 的所有权转移一次性表达，而不是遗漏清理。`528-537` 的
异步 checkpoint-buffer reclamation 也因 core 使用同步 checkpoint transaction owner 而不适用；
immutable image 只有在 home flush 与 FUA tail publication 均成功后才 drain。这里的 N/A 只针对
Linux 内存/cache mechanics，checkpoint、replay 与 durable tail 的磁盘语义仍由后续 core 区间承担。

性能 A/B 以本检查点前的 `3c961e45e` 为 baseline、`6c61a4c2a8` 为 implementation，固定 CPU 2、
`powersave` governor、release、memory backend、4 KiB block 与 20 MiB payload。两端按 25 个交错
批次运行，每批每个 workload 都先预热 3 次、再测量 20 次，因此 sequential 与 sync-cycle 各有
500+500 个样本。首轮 200+200 中约 10 us 的 dirty-sync/unmount median 被调度噪声放大到刚超过
5%；没有删除样本或单跑 implementation，而是对两端对称扩样。最终 2,000 条原始记录汇总如下：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,412,558 ns | 6,404,780 ns | -0.121% | 7,321,295 ns | 7,223,260 ns | -1.339% |
| sequential read | 6,505,964 ns | 6,527,686 ns | +0.334% | 9,476,199 ns | 9,507,542 ns | +0.331% |
| sequential unmount | 18,521 ns | 18,316 ns | -1.107% | 22,831 ns | 23,583 ns | +3.294% |
| dirty sync | 8,521 ns | 8,482 ns | -0.458% | 13,452 ns | 14,463 ns | +7.516% |
| clean sync | 236 ns | 211 ns | -10.593% | 307 ns | 286 ns | -6.840% |
| sync-cycle unmount | 5,715 ns | 5,737 ns | +0.385% | 8,793 ns | 9,275 ns | +5.482% |

所有 median/p95 回退均满足 5%/10% 门槛。clean-sync 不进入 metadata publication 路径，其纳秒级
改善只作为噪声观测，不声明为实现收益；真正受影响的 sequential metadata 与 dirty-sync 路径没有
可测回退。

最终代码形状下，`cargo test -p rsext4 --all-features` 为 282 个 unit 加全部
integration/Linux image/e2fsck 绿，`--no-default-features` 为 281 个 unit 加全部 integration/Linux
image/e2fsck 绿；三组目标 clippy、格式与 portable-core boundary 同步通过。

### 7.55 ax-fs-ng native FUA capability 检查点

Linux v7.1 `fs/jbd2/commit.c:152-156` 在 barrier 开启且非 async commit 时用
`REQ_PREFLUSH | REQ_FUA` 发布 commit record；`fs/jbd2/journal.c:1767-1774` 也要求以
`REQ_FUA` 发布 journal tail，避免复用日志空间前旧 tail 尚未落盘。portable core 因而只接受两种
明确的 durability：底层真实实现 `WriteFlags::FUA`，或设备不支持 FUA 但支持 flush 时由私有
`BlockDev` 同步执行普通 write 后 flush。底层 `BlockIo` 默认实现继续拒绝 FUA，adapter 不能把
普通 write、no-op flush 或单个硬件队列的能力伪装成稳定介质语义。

旧 `ax-fs-ng::Ext4Disk` 无条件报告 `fua=false`，`NativeHandleBlockDevice` 也只能提交
`RequestFlags::NONE`。确定性 adapter 红测先要求 FUA-capable mock 在一次 metadata FUA write 后
得到一次 FUA write、零普通 write、零 flush；旧实现首先在 capability 断言失败。当前 adapter 用
独立的 `supports_fua()`/`write_block_fua()` 能力表达该动作，不向 `rsext4` 泄漏 rdif、IRQ、queue
或锁类型；只读与 unsupported 在设备 I/O 前返回 typed error。native handle 只在 hctx 集合非空且
所有 hardware queue 的 `supported_flags` 都包含 `RequestFlags::FUA` 时宣告能力，避免一次
filesystem write 被调度到能力较弱的 queue。

第二个 runtime 回归把 4 个 512-byte block 交给 `max_blocks_per_request=1` 的 queue。旧统一写路径
会给全部拆分 request 填 `RequestFlags::NONE`；当前 `write_blocks_with_flags()` 将同一 typed flag
传到每个 transfer chunk，测例在 IRQ completion 前确定性观察到 4/4 FUA request。FUA write 不持
hctx 的 `IrqMutex` 等待 I/O：capability 查询只在短临界区读取 immutable queue limits，释放后才进入
software channel admission 与 completion wait。flush 仍是独立 request 和设备级 barrier，供 core 的
write-then-flush fallback 保序。

host core benchmark 的 memory `BlockIo` 不经过 `ax-fs-ng`/rdif adapter，也不提供 native FUA，
因此本切片没有可归因的 host A/B workload；把该数字混入冻结的 core performance gate 会只测到相同
fallback。这里不声明未经硬件测量的 latency 收益，只记录可观察的调用差异为 1 次 native FUA、0 次
额外 flush。验证覆盖 `ax-fs-ng` 无 feature、fat-only、ext4-only、fat+ext4 四种 host 组合；ext4-only
为 86 个 unit 加 3 个 integration，fat+ext4 为 88 个 unit 加 3 个 integration，全部通过。physical
block size 由下一检查点接通；discard capability 继续保留在 `blockio-adapter-capabilities` 红项中。

### 7.56 logical/physical block geometry 检查点

Linux v7.1 `include/linux/blkdev.h:389-394` 将 logical、physical、alignment offset、minimum I/O
和 optimal I/O 作为不同 queue limit；`block/ioctl.c:683-692` 也分别通过 `BLKSSZGET` 与
`BLKPBSZGET` 对外报告。ext4 的块寻址仍以 logical sector 为单位：例如 external journal mount 在
`fs/ext4/super.c:5985-5998` 只要求 filesystem block 不小于设备 logical block；mballoc 的 stripe
对齐来自 ext4/RAID geometry，不是把 filesystem block 强制提升到 physical block。

旧 rdif `DeviceInfo` 只有 `logical_block_size`，`NativeHandleBlockDevice` 与 `Ext4Disk` 因而把设备
physical geometry 永久压成 logical。确定性 adapter 红测构造 512-byte logical、4096-byte physical
设备，旧 `Ext4Disk::geometry()` 稳定返回 physical=512；当前 `DeviceInfo` 明确保存两者，未显式
报告的 driver 按 Linux block layer 的默认规则使用 physical=logical，已知信息则通过 typed builder
覆盖。region adapter 只改变 LBA 范围和起点，不改变底层 physical block size；partition alignment
offset 尚未建模，不能用拒绝非 physical-aligned partition 的方式伪造该语义。

rdif transfer planner 在请求 admission 前按 Linux `block/blk-settings.c:340-365` 规范化 geometry：未
报告或比 logical 更小的 physical size 提升为 logical，只有规范化后非 2 次幂的 descriptor 被拒绝。
portable core 对 injected service 执行同一规则，但不会使用 physical size 改写 LBA 或 block mapping。
专门回归验证 1 KiB filesystem block 在 4 KiB physical/512-byte logical device 上可用，防止后续把
performance hint 错当成 mount compatibility 条件。adapter 在取得 mount owner 前还会把 `usize`
geometry checked-convert 为 core 的 `u32` wire type；超出边界时返回 typed overflow，而不是把失败
静默伪装成零后再误报为坏 superblock。

本切片只增加 immutable descriptor 字段和 mount-time validation，不改变冻结 benchmark 的 timed
read/write/sync/unmount 主路径，因此没有可归因的 host A/B，也不声明性能收益。验证要求覆盖 rdif
descriptor/planner、rsext4 checked geometry、ax-fs adapter 与现有 feature matrix；alignment offset、
io_min/io_opt 和 discard 继续作为独立 capability 红项，而不是把尚无 consumer 的字段一起塞入接口。

### 7.57 MMP mount lifecycle 检查点

Linux v7.1 `fs/ext4/super.c:5496-5500` 只在可写 mount 时调用
`ext4_multi_mount_protect()`；只读 mount 不读取、不校验也不写入 MMP block。`RO→RW` remount 则会在
`super.c:6769-6775` 重新检查 writable feature 并建立 MMP owner。旧 rsext4 把
`EXT4_FEATURE_INCOMPAT_MMP` 一律视作 unsupported incompat，确定性 feature 红测因此在只读协商时
返回 `UnsupportedFeature(bits=0x100)`。

当前 feature negotiation 为只读与可写维护不同的最小 incompat mask：只读额外接受 MMP 且
完全不进入 protection block I/O；可写路径则在 superblock、journal 或 namespace mutation 前建立
MMP owner。`rsext4` core 实现 `fs/ext4/mmp.c:1-404` 的 checked codec、metadata checksum、
clean/FSCK/stale-sequence 策略、均匀随机 sequence claim、超时后二次确认、周期 refresh 与 clean
release。claim/release 持久化使用 metadata+FUA 语义；refresh 再读取时同时比较 sequence 和
node identity，任一错误都锁存 failed owner，阻止后续可写操作。初始 mount、RO→RW、
RW→RO 和 unmount 的回滚共用这一份 core state；Linux 依据还包括
`fs/ext4/super.c:6769-6775,6828-6829`。

OS 边界没有被压成巨型 runtime trait。Core 仅声明小型 `EntropySource` 和 `Delay`，并接收纯数据
`MmpIdentity`；wall clock 只写磁盘 timestamp，monotonic clock 只计算 refresh elapsed。周期调度、
task 生命周期与锁完全属于 adapter：`ax-fs-ng` worker 在 sleepable wait 期间不持 filesystem lock，
仅在调用 `refresh_mmp` 时取得 `Ext4` 独占 owner。当前 ArceOS 没有可信 entropy provider，因此
其 writable MMP 在任何磁盘 mutation 前返回 typed `UnsupportedCapability`/`EOPNOTSUPP`，而不使用
时间或地址伪造随机性。

adapter 不再把所有挂载写成固定的 `TGOSKits/ax-fs-ng` diagnostic identity。node 字段保持为空，
因为当前 ax-fs-ng 没有全局 UTS identity；device 字段由真实 `FsBlockDevice::name()` 与 region
起始 LBA 编码，并为 region 后缀保留固定宽度，避免多个设备分区都显示成同一条写死标签。MMP
互斥正确性仍只依赖随机 sequence 与 checked refresh，identity 只用于故障诊断，不能替代 entropy。

Linux differential 用 `mkfs.ext4 -O mmp` 生成固定 64 MiB 镜像。测例首先验证 RO/no-replay
mount 可读取根 inode，卸载后整张镜像与 snapshot 逐字节相同；缺少 entropy 时初始 RW 和
RO→RW 均精确拒绝且 options 不变。注入固定 entropy、逻辑 delay 和 identity 后，同一镜像完成
claim、refresh 与 clean unmount，最后从 `s_mmp_block` 读回 clean sequence 并要求 `e2fsck -fn`
clean。Unit fault injector 另外覆盖 checksum corruption、64-bit block location、FSCK owner 立即拒绝、
stale owner 两次等待/复查、所有写入的 metadata+FUA flags，以及 refresh write fault 后 mutation 持续拒绝。Linux 的
Linux-image fault test 还在 ext4/JBD2 clean 提交后定点使最终 MMP CLEAN 写失败：该 mount 立即进入
terminal unmounted state，保留原始 I/O error，并拒绝 sync、remount 和再次 release。不能重试这个
结果不确定的 CLEAN 写，因为第一次写可能已经到达设备，随后的重试可能覆盖另一个已经 claim
的 owner。Linux 的
`ext4_setup_system_zone()` 本身也不把 `s_mmp_block` 加入 block-validity tree，因此 portable core 不
擅自扩大 metadata zone。本检查点的周期 wait/refresh 没有纳入冻结 host benchmark workload，故不声明
无对照数据的性能变化；平台 RNG、真实双主机互斥、priority I/O 与完整 persistence-boundary
断电矩阵继续登记为本总项的红色验收子项。

### 7.58 extent leaf insertion normalization 检查点

Linux v7.1 `fs/ext4/extents.c:1786-1860` 的 leaf normalization 先用
`ext4_can_extents_be_merged()` 检查 initialized/unwritten 状态一致、逻辑与物理区间都连续，并要求合并
长度能由对应的 on-disk `ee_len` 表示。`ext4_ext_try_to_merge()` 优先把新 extent 向左合并；若左侧没有
发生合并，才从新 extent 开始向右合并。`ext4_ext_try_to_merge_right()` 会继续扫描同一 leaf 的全部可合并
右邻，而不是只做一次 pair merge。这里的 leaf 范围也是 Linux 的边界；跨 leaf 合并不是遗漏的语义。

旧 Rust insertion 只检查 predecessor，成功后立即返回。确定性红测先放入 `[0,2)` 与 `[4,6)` 两个
物理同样连续的 initialized extent，再插入 `[2,4)` bridge；旧实现稳定留下两个 extent，断言得到
`left: 2, right: 1`。旧实现还有一个 Linux 不存在的上限处理分支：当总长度超过 wire limit 时先把左侧
填满，再把余量改写成 tail，这会改变 caller 提交的 extent boundary。

当前 `merge_leaf_extent_neighbors()` 先尝试 predecessor/new pair，再从存活项持续向右；底层
`merge_leaf_extent_right()` 对逻辑与物理终点使用 checked arithmetic，并按 initialized 32768、unwritten
32767 的真实编码上限决定能否整体合并。超过上限、状态不同或任一方向不连续都保持原来的两个完整
extent。外部 leaf 在 normalization 后通过既有 checked node codec 写回；专门用例强制构造 depth-1 tree，
丢弃内存中的 tree 后重新从 block device 遍历，证明三段合并后的 extent 已持久化而非只修改 clone。
现有 remove 与 insertion 现在共用 `collapse_external_root_child()`：先 checked 验证 inode `i_blocks`
可扣减并把 child image 发布为 inode root，再记录 revoke、释放 bitmap/GDT owner，最后扣减 metadata
block sectors。insertion 只在 depth=1、root 单 child、child 是 leaf 且 entries 可内联时尝试；它先按
Linux 精确扩展 2 个 metadata credit 和 1 个 revoke credit。无 scoped handle 或 extension 返回
`RestartRequired` 时保留合法外部树，不把 best-effort normalization 误报成插入失败。本切片没有把
普通 insert 扩大为跨 leaf 重平衡；Linux 的 right-merge 本来就不跨 leaf。

回归矩阵覆盖 bridge 同时合并左右邻、状态不同时只合并合法一侧、initialized 超限时不做 partial merge、
unwritten 恰好到上限与超过上限、以及 external leaf reload。本检查点只对齐 Linux
`extents.c:1786-1932`，不宣称整个 `extents.c` 已完成。

首个正确性实现 `dae8d9499` 相对 `4cf9505a3` 的 500+500 交错 A/B 发现 dirty-sync median +7.51%、
clean-sync p95 +10.76%、unmount median +5.15%，因此保留为性能红证据，没有用功能测试绿色掩盖热路径
回退。旧实现的常见 append 会原地扩大 predecessor；首版却先 `Vec::insert` 再立即 `remove`。后续
`9ea3c593b` 恢复原位 append fast path，只有不能向左合并时才插入新 entry，再继续执行 Linux 的
right-merge。

最终 A/B 固定 CPU 2、`powersave` governor、release、memory backend、4 KiB block 与 20 MiB payload。
sequential 使用 25 个交错批次、每批每端 3 次预热与 20 次测量，得到 500+500；sync-cycle 首个
500+500 窗口的 dirty p95 被少数 batch 尾延迟推高 15.9%，但 median 为 -2.0%，独立批次分析也没有
发现执行顺序或前后时段漂移。没有删除该窗口，而是对称追加第二个 500+500，最终 3,000 条原始记录
汇总如下：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,443,749 ns | 6,404,617 ns | -0.607% | 7,192,451 ns | 7,189,518 ns | -0.041% |
| sequential read | 6,283,132 ns | 6,328,687 ns | +0.725% | 8,002,624 ns | 8,152,259 ns | +1.870% |
| sequential unmount | 18,680 ns | 18,330 ns | -1.874% | 22,291 ns | 21,239 ns | -4.720% |
| dirty sync（1,000 次） | 9,764 ns | 10,096 ns | +3.400% | 23,781 ns | 23,376 ns | -1.703% |
| clean sync（1,000 次） | 277 ns | 279 ns | +0.722% | 791 ns | 819 ns | +3.540% |
| sync-cycle unmount（1,000 次） | 6,761 ns | 6,630 ns | -1.938% | 16,857 ns | 16,817 ns | -0.237% |

六项 median/p95 全部满足 5%/10% 门槛。sequential append 直接经过本切片，证明共享写入热路径没有
回退；sync-cycle 只守护共同 transaction/sync 路径，不能声明 leaf normalization 改善了 sync 性能。

### 7.59 insertion-time extent merge-up 检查点

Linux v7.1 `fs/ext4/extents.c:1862-1906` 只在 root depth 为 1、root 恰有一个 child、child entries 不超过
inode root capacity 时执行 `ext4_ext_try_to_merge_up()`。它不能先释放 child 再发布 inode：必须先取得
额外 journal credits，把 leaf image 拷回 inode，随后以 revoke-aware metadata free 回收 external block。
`1912-1932` 的 wrapper 在 leaf 邻接合并后无条件尝试该 best-effort normalization。

旧 Rust insert 在 external child 写回后只保存原 index root。确定性红测手工建立一个合法 depth-1、
single-child tree，child 只有一个 extent；在同一 scoped journal transaction 内插入第二个仍可内联的
extent。旧实现返回成功但 root depth 稳定为 1，新断言要求 depth=0，因此在修复前精确失败为
`left: 1, right: 0`。

当前 insert 只在结构条件满足后读取 checked child，再调用
`extend_active_transaction_credits(metadata=2, revokes=1)`。成功扩展后，共用 helper 按 inode root 发布、
revoke、block bitmap/GDT 回收、`i_blocks` 扣减的顺序完成状态转换；remove 原有 root promotion 也改用
同一 owner，避免两套退化顺序漂移。无 transaction 的 low-level caller 保留 depth-1 tree；第五个 entry
超过 inline capacity 时同样保留 child。direct transaction 的 fault test 在 child 已更新、root 已准备
内联后定点使 block bitmap publish 失败，并证明 transaction 返回原始 I/O error 后，inode root、external
child、extent 集合、free count 与 `i_blocks` 全部恢复，child 也没有提前重入 allocator。

本检查点补齐 `extents.c:1861-1932`，与 7.58 的 right-merge 一起使 `1786-1932` 成为连续 reviewed
segment。

首个功能提交 `9af7a5f36` 相对 `3acc11385` 的完整 A/B 汇总如下：sequential 为 500+500，sync-cycle 在
独立重复后为 1,000+1,000。dirty-sync 与 sequential 已过门槛，但第二轮 clean-sync median 仍为
+7.69%，完整窗口为 +8.86%；这条路径虽然不执行 merge-up，也不能以 structural cold path 为由豁免。
分析定位到普通 inline-leaf insertion 每次都会进入 merge-up 函数，直到函数内部才拒绝 root shape。

`b80dd453f` 在 caller 已持有的 parsed root 上先做 depth/index count fast reject，真正的 merge-up 与共用
collapse helper 标为 cold、non-inline。最终同机 A/B 固定 CPU 2、`powersave` governor、release、
memory backend、4 KiB block、20 MiB payload，10 个交错批次、每批每端 3 次预热与 20 次测量，800 条
原始记录汇总如下：

| workload/metric | baseline median | implementation median | 变化 | baseline p95 | implementation p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| sequential write | 6,441,373 ns | 6,472,611 ns | +0.485% | 7,164,373 ns | 7,267,198 ns | +1.435% |
| sequential read | 6,207,498 ns | 6,347,436 ns | +2.254% | 8,028,244 ns | 7,853,175 ns | -2.180% |
| sequential unmount | 18,226 ns | 18,414 ns | +1.032% | 21,313 ns | 21,865 ns | +2.590% |
| dirty sync | 8,314 ns | 8,098 ns | -2.598% | 12,905 ns | 9,927 ns | -23.076% |
| clean sync | 218 ns | 208 ns | -4.587% | 286 ns | 267 ns | -6.643% |
| sync-cycle unmount | 5,588 ns | 5,434 ns | -2.756% | 8,433 ns | 7,020 ns | -16.756% |

六项 median/p95 全部满足 5%/10% 门槛。这里不把改善声明为 merge-up 收益；它只证明冷语义没有污染
普通 leaf insertion 及共同 sync/unmount 热路径。

### 7.60 非首组 inode reserved-range 检查点

Linux v7.1 `fs/ext4/ialloc.c:725-735` 在选定 block group 的 bitmap 上从 group-local candidate
offset 查找空位；`1073-1083` 只在 `group == 0` 时用 `EXT4_FIRST_INO` 检查全局 reserved inode。
旧 Rust allocator 却对每个 group 都从 `s_first_ino - 1` 开始扫描，使非首组前若干合法 inode
永久不可分配，并让后续分配选择与 inode-table accounting 偏移。

确定性单测使用 16 inodes/group、`s_first_ino=11` 和全空 group 1 bitmap。旧实现返回 relative
index 10/global inode 27，新断言要求 index 0/inode 17，修复前精确失败。当前 allocator 只在
group 0 使用 reserved-range 起点，其他组从 0 扫描；候选 bitmap 仍由既有 `alloc_inodes()`
transaction owner 在完整请求成功后发布，bitmap checksum、group/super free counter、
`itable_unused` 与失败 rollback 都不需要新增状态源。

本检查点没有声称吞吐收益，也不单独使用 sequential extent harness 做无关性能归因；最终 PR
仍按 7.8 的冻结 dev/head workload 做整体性能验收。

### 7.61 external xattr orphan reap 检查点

Linux v7.1 `fs/ext4/xattr.c:2906-3014` 在 inode 最终释放前调用
`ext4_xattr_delete_inode()`：先读取并校验 `i_file_acl` block，shared block 减引用并更新
checksum，最终引用则以 metadata revoke 释放；`i_file_acl=0` 必须和 release 位于同一
transaction。EA-inode value 的引用遍历是另一项 feature，本检查点不把它伪装成已支持。

确定性红测在一个空文件上写入 512-byte user xattr，确认它独占一个 external block。记录创建
xattr 前的 free-block 数后执行 unlink + explicit reap；旧实现清空并释放 inode，却没有遍历
`i_file_acl`，因此 free count 稳定少 1（28153 对 28154）。当前实现复用 xattr owner 的
header/checksum/refcount 校验，refcount=1 时在同一 reap handle 内申请一条 revoke、释放 bitmap/
GDT/superblock 并清 inode 指针，同一测试在当前 mount 与重挂载后都精确恢复 free count。

共享矩阵把两个 inode 固定到同一 block/refcount=2：第一次 reap 后 block 仍 allocated、另一 inode
可读且持久 refcount=1；第二次 reap 才释放。no-journal 定点 bitmap write failure 则证明 release
失败返回原始 I/O error，并恢复 orphan membership、inode allocation、`i_file_acl`、xattr value 与
free count；同一 inode 随后可重试 reap 成功。refcount 0 与 1025 也在 mutation 前返回 corruption。
本检查点没有声称热路径性能收益；最终 PR 继续用冻结的 dev/head workload 做整体性能验收。

### 7.62 JBD2 partial commit block recovery 检查点

Linux v7.1 `include/linux/jbd2.h:167-177` 定义的 `struct commit_header` 线格式长度为 60 bytes。
`fs/jbd2/recovery.c:431-468,820-878` 在 CSUM_V2/V3 commit 的完整 block CRC32C 失败后，建立一个
全零 journal block，只拷贝该 60-byte header、清零 `h_chksum[0]` 后重新校验；若匹配则把它识别为
tail 未完整持久化但已提交的 transaction，继续推进 sequence 并在后续 pass 回放。COMPAT checksum
使用 descriptor/payload 聚合 CRC32-BE，不进入这条回退。

旧 Rust scanner 只有完整 block 校验。确定性红测先用正常 writer 建立 CSUM_V3 transaction，再只把
commit block 第 61 个 byte 从零改为 `0x7e`；header、stored checksum、descriptor 和 payload 都不变。
旧实现精确返回 replay-phase `ChecksumMismatch`，home block 保持全零。当前 checksum owner 按 wire
constant 处理 60-byte header，以固定小块增量喂入零 tail，不分配第二个 journal block；scanner 只在
完整 checksum 不匹配时调用该 helper。相同测试现返回 `ReplayStatus::Complete`，home block 等于原始
`0xa5` payload。既有 commit checksum corruption 用例仍要求 header 内损坏被拒绝，因此回退没有扩大到
任意损坏 transaction。

这一冷恢复分支不改变 writer、正常完整 commit replay、sync 或 unmount 热路径，本检查点不单独声明
性能收益；最终 PR 仍以冻结 dev/head workload 做整体性能验收。本检查点只对齐
`recovery.c:431-468,820-878`，不宣称其余 recovery 路径已经完成。

### 7.63 JBD2 stale checksum tail 与 commit time 检查点

Linux v7.1 `fs/jbd2/recovery.c:588-645` 在每个 recovery pass 中把
`need_check_commit_time=false`、`last_trans_commit_time=0` 作为 scanner-owned state。descriptor checksum
失败在 PASS_SCAN 只设置 deferred flag（`703-721`），revoke checksum 失败也共用该 flag
（`880-904`）；之后遇到结构可解析的 commit block，`794-878` 仅在 commit time 小于
上一个已接受 transaction 时把损坏解释为 lazy-initialized stale tail 并正常结束恢复；
时间相等或递增意味着同一 journal 内的真实损坏，必须拒绝。commit block 自身的
COMPAT/CSUM_V2/CSUM_V3 checksum 失败也进入同一时间判定。

确定性红测用两个完整 CSUM_V3 transaction：第一个 commit time=10，第二个=9，
分别翻转第二个 descriptor tail、commit checksum 和 revoke tail 的一个 byte。旧 scanner 在三个
分支都返回 incomplete，commit corruption 甚至已进入 replay phase；同一矩阵现返回
`ReplayStatus::Complete`，第一个 payload 回放、stale transaction 的 home block 保持不变。
显式非 stale 矩阵对 descriptor/commit/revoke 三种损坏分别执行 10→10 与 10→11，六组都要求
`ChecksumMismatch`、精确 replay phase 且零 home write，锁定 `commit_time >= last_commit_time`
必须拒绝，没有把校验失败扩大成静默成功。

边界复核又暴露两个旧问题：scanner 只依据 12-byte journal header 检查 block size，
之后却无条件解码 60-byte commit header，32-byte fixture 稳定 panic；另一方面，旧 parser
把纯 header+全零 tail 解成零 tag，因此可能继续接受紧邻的伪 commit。当前 scanner 在 I/O 前
要求至少 60 bytes；parser 将零 tag 尾明确表示为未提交 `EmptyTail` 并 clean-end，而已有
tag 却没有 `LAST_TAG` 则返回 typed corruption。单元用例在空 descriptor 后放置相邻 commit
仍要求 clean-end；原有 Linux-image 集成用例继续要求空 descriptor tail 正常丢弃。

Linux v7.1 `fs/jbd2/commit.c:114-144` 在 commit record checksum 之前写入 coarse realtime seconds/
nanoseconds。旧 Rust writer 一直写 0，使上述 stale 判定在自身产生的 journal 上缺失时间
信号。现在只有非空 running transaction 会读取已注入的 filesystem clock，并在
CRC/FUA publication 前写入 commit header；空 commit 仍不读时钟。定点用例要求
`1_723_456_789s + 123_456_789ns` 原样出现在线格式。另一红测证明旧 writer 会把负秒静默
改成 0、把越界纳秒截断；当前 typed commit timestamp 在 owner 由 running 转 committing 前返回
`InvalidInput`，保留 running queue 且不 abort journal。该改动每个真实 commit 多一次
clock callback，不声称吞吐改善；它与最终 dev/head `sync-cycle` 对照共用性能门槛。

首个实现提交 `897d40a01` 在空 commit 路径先做一次 immutable `system` 查找和
pending 分支，再做 mutable 查找，因此即使不读时钟也污染 clean-sync 热路径。固定 CPU 2、
`powersave` governor、同一 nightly/release/memory backend/4 KiB/20 MiB workload，20 个交错批次、
每端每批 3 次预热和 20 次测量，共800 条原始记录汇总如下。相对 parent
`a91e89cd8`，dirty-sync median/p95 为 +4.737%/-2.793%，clean-sync 为 +9.437%/+6.375%，
unmount 为 +3.223%/+0.652%；clean-sync median 越过 5% 硬门槛。

`aaf882ecf` 将空 running transaction 的早返收敛到单次 mutable owner 查找，只在真实 pending
transaction 上读时钟和进入 commit state machine。同样配置独立重采 800 条，汇总如下：

| sync-cycle metric | parent median | current median | 变化 | parent p95 | current p95 | 变化 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| dirty sync | 11,412 ns | 11,559 ns | +1.288% | 17,723 ns | 15,614 ns | -11.900% |
| clean sync | 225 ns | 221 ns | -1.778% | 463 ns | 346 ns | -25.270% |
| unmount | 7,650.5 ns | 7,661.5 ns | +0.144% | 12,050 ns | 10,589 ns | -12.124% |

六项 median/p95 都通过 5%/10% 门槛。这只证明时间语义未导致该切片的性能
回退，不宣称新功能提升吞吐；最终 PR 仍需对当时最新 dev 重做完整 workload 验收。

### 7.64 xattr inode-body/external placement 检查点

Linux v7.1 `fs/ext4/xattr.c:1629-1853` 先在单个 store 内计算 entry/name/value 的真实
可用空间并原子完成该 entry 的增删改；`2337-2498` 在 `ext4_xattr_set_handle()` 中先查和
尝试 inode body，只有 `-ENOSPC` 才写 external block。目标属性在新 store 成功后，只从旧
store 删除同名 entry，不迁移无关 sibling。`fs/ext4/extents.c:5120-5160` 的
`FIEMAP_FLAG_XATTR` 也先报告 inode-body store，只有 inode body 不存在时才报告 `i_file_acl`。

旧 Rust `persist_xattrs()` 把两处 entry 合成一个 `Vec`：只要整体放不进 inode tail，就把所有
entry 一起编码进单个 external block。确定性红测先写入 48-byte `user.small`，再写入 4000-byte
`user.large`；large 单独能放进 4 KiB external block，但与 small 合放必然溢出，旧实现稳定返回
`NoSpace(op=xattr:encode_value)`。当前 `XattrLayout` 分别拥有 inline/external entry 集合；set 先
构造 inline candidate，只有容量错误才构造 external candidate，`persist_xattrs()` 只发布最终两处
布局。large 写入后 raw inode 只含 small，`i_file_acl` block 只含 large，两个值均可读。

共享 external fixture 进一步固定 refcount=2，并在 shared block 上设置 direct-write 故障；给其中
一个 inode 新增无关 inline 属性后故障点仍未消耗，两个 inode 继续指向同一 block/refcount=2，证明
unchanged external store 没有被复制或重写。inode-table publish 故障则验证新 external allocation、
raw inode、inline sibling 与 free-block accounting 在当前 mount 和重挂载后全部回滚。

1/2/4 KiB Linux image 用例按 `block_size - 80` 构造 large value：该值单独能装入 external
block，但与 Linux/debugfs 预置的 inline sibling 合装必然超过 block。三个几何均完成
inline→external→inline、free-block accounting、FIEMAP inode-body-first、unmount/remount、
`debugfs ea_list` 和 `e2fsck -fn`。本检查点只对齐 `xattr.c:1629-1853,2337-2498`；
EA-inode value、ACL/security/trusted policy 与 external deletion power-cut replay 仍保持
红项。本检查点不声称性能提升；最终 PR 继续按冻结的 dev/head workload 做整体性能验收。

### 7.65 最新 dev 与重构分支整体性能验收

最终验收以最新 `dev` 的 `c8a7962f4` 为基线，在同一 x86_64 主机、同一 nightly、
QEMU 10.1.0、8 vCPU、512 MiB、NVMe snapshot rootfs 上运行双方共有的
`apps/starry/block-io-bench`。每次 guest 运行内部执行 5 轮 4 MiB/4 KiB workload；
重构数据对应 `bb0f0a57a1`，双方各独立启动 3 次 guest。下表先取每次内部 5 轮中位数，
再取 3 次 guest 的中位数。
全部 30 个 correctness phase 均通过 bytewise、checksum 与跨 fd truncate/rewrite 校验。
汇总如下。

| Starry 综合 workload | dev 数据（中位数） | 重构后数据（中位数） | 重构后耗时变化 | 结论 |
| --- | ---: | ---: | ---: | --- |
| buffered write | 1,783,294 us | 586,216 us | -67.13%（3.04x） | 绿 |
| fsync | 497,893 us | 267,779 us | -46.22%（1.86x） | 绿 |
| reopen/read/verify | 168,758 us | 190,927 us | +13.14% | 外层缓存边界，停止在 rsext4 优化 |
| truncate/reverse-write/fsync | 873,498 us | 1,027,712 us | +17.65% | 外层缓存与必要 durability 混合，停止在 rsext4 优化 |
| cross-fd coherence read | 134,987 us | 140,593 us | +4.15% | 绿 |

host `sync-cycle` 同机使用 128 MiB memory device、4 KiB block、journal、20 MiB payload、
3 次预热和 20 次测量。最新 dev 因 API 已变化，仍使用 7.35 冻结的操作序列等价 adapter；
双方执行相同的 `mkfs -> mount -> write -> dirty sync -> clean sync -> unmount`。汇总如下。

| host sync-cycle | dev 数据 median/p95 | 重构后数据 median/p95 | median 变化 | p95 变化 |
| --- | ---: | ---: | ---: | ---: |
| dirty sync | 6,884.5 / 8,758 ns | 7,927 / 8,561 ns | +15.14% | -2.25% |
| clean sync | 2,937 / 3,044 ns | 196.5 / 249 ns | -93.31% | -91.82% |
| unmount | 10,346 / 12,332 ns | 5,310 / 5,783 ns | -48.68% | -53.11% |

dirty-sync median 的额外成本保留了 Linux JBD2 descriptor/payload preflush 与 FUA commit
durability boundary；p95 没有回退，不能通过删减持久化顺序换取更低 median。综合 workload
中的 read I/O 数与 dev 相同，差异来自 ax-fs-ng shared block cache 对 multi-folio direct read
增加的锁与 folio overlay。Linux v7.1 的普通 buffered data cache 由 VFS/mm page cache 持有：
`fs/ext4/file.c:130-148,302-323` 进入 generic file read/write，`fs/ext4/fsync.c:167-187`
先等待 file page cache writeback，再提交 ext4 journal。因此本 PR 不在 rsext4 内重建第二套
inode/page-offset cache；后续若继续收敛上述两项，应在 ax-fs-ng shared cache 与 VFS page cache
边界处理，而不是扩大 rsext4 `DataBlockCache`。该边界的后续工作已登记为
[#2206](https://github.com/rcore-os/tgoskits/issues/2206)。
