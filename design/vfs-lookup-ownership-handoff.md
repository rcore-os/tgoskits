# 文件系统所有权重构交接

## 1. 交接边界

本分支按要求在当前检查点提交并交接，原实施工作停止。文件系统重构计划尚未完成，PR [#2800](https://github.com/rcore-os/tgoskits/pull/2800) 保持草稿；接手者需要继续设计、迁移和系统验证，不能把本次交接视为整体修复或合入批准。

### 1.1 目的与预期收益

路径是名称到文件实体的解析过程，文件句柄则保留已经取得的对象。现有问题来自这些边界混用：rename 后仍沿旧名称关系行走，proc 链接的显示文字被拿来重新查找目标，授权后释放后端锁却继续使用旧授权，以及最后引用在调用者锁内析构。局部补错误码不能约束后续调用是否继续犯同类错误。

重构通过稳定 dentry、独占 `LookupSession`、借用真实守卫的提交能力、对象型链接和领域元数据命令，把身份、权限检查、实际提交及资源释放归到明确的所有者。预期收益是统一普通路径与受限查找的语义，保留 rename/unlink 后句柄的正确身份，减少跨重试授权及锁内析构错误，并让非法顺序更早被接口和测试发现。性能目标是复用现有缓存与锁域、避免全局串行路径查找；尚无完整前后性能数据，不能宣称性能已经提升。

### 1.2 版本与历史

交接沿用 `fix/vfs-lookup-ownership-linux71`，目标分支为 `dev`。原批准计划仍作为接手目标；后续接口与行为约定集中在 [主设计](vfs-lookup-ownership.md)，本文件记录实施状态与缺口，不另立接口契约。

| 对象 | 精确版本或状态 |
| --- | --- |
| 实施开始时的主线 | `94a2d5e51360f483c9ea253da4babb2fe00d3fdb` |
| 最近已局部验收的代码 | `13ad64fcac5c226e981be3f023a3e3f87a3c8ec8` |
| 本次合入的主线 | `a15042772543065e16e48e15e5f472757004d0ec` |
| 本次合并提交 | `8aaa54b4f3d47a8126250f8cd1fbe3ec01764144`，保留上述两方父提交 |
| Linux 语义基准 | v7.1 源码提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，tree `96667ed2997d7b49f3e9a36c4297b1faca4d87e3` |
| 当前检查点 | 含本文件的交接提交；PR 正文记录最终推送的 HEAD |

Linux 依据是固定源码分析，不是 Linux 7.1 实机运行证明。与 #2481 有关的根因修复在本独立分支推进；不带入原 PR 的 eBPF、网络/IPC 业务身份、OOM、starttime、ELF 或 Docker 工具链，不改变磁盘格式。

## 2. 已实现的基础

当前实现包含可复用的底层单元和部分消费者迁移；主设计也包含尚未落地的接口。接手时应逐项对照实现及其证据，不能根据文档中的最终形态推断生产路径已完成迁移。

### 2.1 对象与后端边界

分支已修改稳定 dentry 与弱索引、挂载身份和写入保活、VFS 命名操作的授权与提交结果、Ext4 与 MemoryFs 的真实尝试边界、FAT 规范 inode 与元数据持久化、cgroup 文件属性及后备对象查询边界。Starry 文件对象、proc 权限和调用凭据已有部分连接，尚未完成统一消费者迁移。

`PathHandle` 保留实际挂载使用权，Clone 与同挂载转绑共享已有使用守卫；`PathUnmount` 消费真实独占路径，卸载过程保持使用权连续，其他独立使用者仍在时拒绝卸载。这一局部单元已有红绿及独立审查，尚不代表 FsContext、打开描述和 MM 的全部生产接入。

### 2.2 命名空间基础

`78aeafbf5b0acc2582157cba2ef93e887057b5d4` 的 VFS namespace 单元已独立审查：namespace 实体不可 Clone，创建凭据一次消费，anchor 身份稳定，克隆有完整映射，最后实体退出时退休本树拓扑。原节点退休后不得重新发布连接，已有路径仍保留文件实体与使用权。

传播克隆中的临时引用、挂载安装成功或失败的输入、替换出的连接均有锁外退休出口；覆盖冲突保留原覆盖子树及顺序。Starry `pseudofs::mount_all` 已改为短锁复制上下文后退锁，再准备和安装后端；它仍须随其余消费者迁入 Snapshot。

### 2.3 根切换依赖单元

设计提交 `2655511d9624024ea0ab40eb49fbd564b18352b8` 后的五文件 VFS 单元在 `13ad64fc...` 完成并获局部独立批准。`MountNamespace::pivot_root` 接受调用者实际 Core，在一次 topology 事务中沿真实 mount 父连接核验 old root 与 new mount 到 exact anchor 的归属，再执行移动。

关键实现位于 `fs/axfs-ng-vfs/src/mount/pivot.rs`，接口声明及退休载体位于 `mount/mod.rs`，传播接点在 `mount/propagation.rs`，真实交错测试在 `mount/tests/namespace.rs` 与 `mount/pivot/test_hook.rs`。接口为：

```rust
MountNamespace::pivot_root(
    self: &Arc<Self>,
    old_root: &PathHandle,
    new_root: &PathHandle,
    put_old: &PathHandle,
) -> (VfsResult<()>, MountPivotRetirement)
```

归属遍历使用 `Mountpoint::location()`，不能改成会跳过覆盖层的 `Location::parent()`。shared 拒绝先于 membership 检查，保留 anchor、retired 和 mount-root 条件；拒绝与容量准备发生在首次连接写入前，必要几何扩容在短锁外完成。成败均外带路径、名称观察、移出引用及旧表，调用者退出保护后再释放。raw `Mountpoint::pivot_mount` 与该入口共用私有持锁算法，不能代替生产入口的 namespace 授权。

## 3. 已有验证与限制

证据按精确版本保留。以下运行和审查发生在合并 `dev` 之前；本次交接只完成冲突、差异与文档核对，没有重新运行合并版构建、Clippy、标准库测试或 QEMU。主线依赖和运行时已变化，历史结果不能直接标为当前合并版通过。

### 3.1 最近的确定性红绿

pivot 错误候选 `3e80efdfb09803eee7e8d3cb97a542c3794cde32` 保留真实成员遍历、API、退休载体和其他检查，仅省略根据成员结果拒绝。两次验证均从仓库根使用同一项目入口，工具、配置与临时目录环境头逐字一致：

```sh
cargo xtask test --since 2655511d9624024ea0ab40eb49fbd564b18352b8
```

错误候选的 70 个 VFS 单元中 68 过、2 败：M0 操作 M1 合法树及取门前真实移树后操作，都错误得到 `Ok(())`，预期为 `InvalidInput`；该次 VFS 集成和文档测试因单元失败而未执行。名称父观察的成败退休测试实际通过。

恢复拒绝条件的 `13ad64fc...` 通过 70 个单元、10 个集成、3 个普通 doctest 和 8 个 compile-fail doctest，共 91 项，并有 `ok: axfs-ng-vfs` 终止标记。最终正确版本的格式化、差异检查与 VFS Clippy 的 base、lockdep、host-test 三组合通过，五文件单元独立审查 `findings=[]`。此批准不包含 AX 聚合、Owner 或 Kernel/ROOT 消费者。

### 3.2 整体仍失败的门禁

上述正确候选的标准库选择器实际为 17 个 profile 中 16 过、1 败，整体退出码为 1。`starry-kernel` 因 Overlay 旧接口存在 E0061×10、E0407×5，不能标为整体绿色。此前 Kernel Clippy 的 72 项矩阵只执行首项 `base / aarch64-unknown-none-softfloat` 就因同类 15 处错误失败，余下 71 项未执行。

没有新增直接系统调用、QEMU、四架构或性能通过证据。较早 Ext4 分配及根增长曾有未归因的偶发问题；后续单次通过不构成归因或解决。系统调用兼容性六列表中 pivot 仍为“无法确认”，局部 VFS 测试不能提升这一结论。

### 3.3 可复用的历史证据

namespace 基础的最终 `78aeafbf...` 使用 `cargo xtask test --since 5971181e40e2e54d7ae1879df77a0af3c37ffd92`，VFS 为 68 单元、10 集成、3 普通文档及 8 编译失败用例，共 89 项通过；整体仍因 Kernel Overlay 失败退出 1。该单元也有 VFS 三组合 Clippy 和独立审查。

路径使用权单元以 `7093c874b9ecf4d50a4650d726af2e7d32920799` 为选择基线，错误变异 `fa62e277...` 使 PostFlush 阶段预期 Busy 变成成功，恢复候选 `f66d866b...` 后 VFS 的 59 单元、10 集成、3 普通文档和 6 编译失败用例通过。后续 namespace 测试实际再次覆盖这些路径。后备查询、cgroup 元数据、FAT 规范 inode 与持久化/Uncertain、稳定 dentry、Scope 压力、Ext4 retry 和 MemoryFs 锁域另有历史局部证据；仅在版本和依赖未受影响时复用，不能由这些结果推断整套计划通过。

### 3.4 证据保全

为让接手者不依赖本机会话，核心版本、源码摘要、红绿结果、失败项及 F1–F8 源码调查随 [交接证据](vfs-lookup-ownership-handoff-evidence.json) 一同提交。它保留历史验证的边界，不替代原始日志，也不把源码调查写成运行证明。

原始报告和日志仍保存在当前工作树的 `.anywork/sessions/`，不属于本次清理对象。最近的根验收记录位于 `c3320838e9d4b94cf09199c28802e530b817cf5a91a94da9be6d62a0b20fc852/artifacts/` 下的 `vfs-pivot-accepted-delivery-manifest.json`、`vfs-pivot-red-root-verification.json`、`vfs-pivot-green-root-verification.json`、`pivot-design-accepted-increment.json` 和 `mount-authorization-owner-migration-facts.json`。

原运行账本位于 `01e8ad855dbfff203f326bfb3b97edc70e7483b6784bbdc6020846f2e2fa5498/artifacts/` 的 `vfs-pivot-red-ledger.json` 与 `vfs-pivot-green-ledger.json`；各自记录原始双流位置。绿账本的单元/集成汇总行号存在笔误，根按原流核正为 1756/1771，普通文档为 1779、编译失败为 1792、包终止为 1795；未修改原账本或重跑。完整审查报告保存在 `ce50bfde2a33ff7576fee726c695cccfc4dc86ef07565a1433d960fb88823a1b/artifacts/vfs-pivot-review/review.json`。

## 4. 当前关键缺口

Owner/pivot 的组件及生产连接已在第 7、9 节分步实现；新的挂载授权仍待设计。下面区分当前代码与尚未取得的系统验证，不能用类型迁移推断 syscall 已兼容。

### 4.1 已冻结的上下文迁移

主设计第 3.8 节要求 `FsContextOwner` 不可 Clone，每个 Owner 保存自己的 namespace，State 只共享 root/cwd 与实际 Owner 份额并由短 IRQ-safe 锁保护。`FsContextSnapshot` 持来源 Weak、冻结 Core 和路径，不计入 Owner 份额，也不升级原 State 的所有权。共享 root/cwd 的 Owner 可以属于不同 namespace，registry 每 State 登记一次 Weak。

Kernel 的 `FsBindingSnapshot` 已成对读取 FS/MNT Scope：双空表示退休，单空返回内部错误。`NamespaceChange` 连接 clone/unshare/setns/exit、实际 Core 及不可变 creator，原 `NsProxy` 的 mount namespace 旁路已删除。单 NEWNS、含 USER、多 flag pidfd setns 已分别迁移共享份额、准备与写回；直接 syscall 回归及最终独立审查仍未完成。已有 IPC 文件对象连接仍在范围，网络/IPC 业务身份分配不在范围。

### 4.2 已冻结的有序根切换

主设计第 3.8.8、3.8.13 节的 AX 聚合 `FsContextSnapshot::pivot_root(&self, new_root: &PathHandle, put_old: &PathHandle) -> (VfsResult<()>, FsPivotRetirement)` 已实现，并接入 ROOT 与 Kernel。AX 私有可睡眠序列门覆盖“VFS 移动 → topology 外 exact registry 扫描”，避免两次拓扑提交 1→2、修订扫描 2→1 导致最终 State 指向错误路径。

锁序为 caller writer → AX 序列门 → topology；先退 topology 再扫描 registry/逐 State，Scope、pin 和 State 短锁不能跨可睡眠门。修订按 exact mount+dentry，不按 namespace 过滤。成败都保存退休值并在 writer 外释放。普通 pivot 不替换 Owner/Mnt 适配、不重新发布 NsProxy。ROOT 保留 bind → 同点 pivot → exact 修订 → detach 的顺序；Kernel 的直接 pivot syscall 运行证据仍缺失。

### 4.3 尚未冻结的挂载授权

F1–F8 调查涵盖 newmount、bind、MS_MOVE、传播、remount、detached move_mount、umount2 和 mount_setattr。它只记录固定 Linux 源码与当前实现差距，尚未形成新的第 3.8.14 节、授权 trait、Core 反向连接、opaque creator payload 或匿名 Owner 接口；接手者需先明确这些合同再迁移消费者。

| 入口 | 必须保留的 Linux 区别 |
| --- | --- |
| newmount | 安装目标属于 current namespace，在安装事务内核验 |
| bind | 目标属于 current；来源另有 current、nsfs/pidfs、合格 anonymous 例外 |
| MS_MOVE | attached 来源与 anonymous 根来源分支不同，来源父 shared 等条件不能丢失 |
| propagation | 允许 foreign namespace，但需存在实际 namespace 并具有其 owner userns 权限；不能一律拒绝 foreign |
| remount | 初始 membership 检查在 namespace_sem 外，不能虚称已有同锁复核；文件系统与 mount-local 更新不同 |
| detached move_mount | 来源须合格 anonymous 根；目标 current 或合格 anonymous，目标查找先于来源 |
| umount2 | 有早期检查及实际拆树锁内重验；expire 的 busy 先于首次 EAGAIN；保留已有连接/传播/busy 复核 |
| mount_setattr | 已支持的无传播 fd-empty-path 子集接受 current 或 anonymous 根，不接受任意 foreign/lazy-detached；全零属性有独立参数/查找顺序 |

Kernel 的 `MntNamespace` 已持实际 VFS Core 与不可变 creator，创建入口消费 `CreatedMountNamespace`，nsfd 弱索引也已使用实际 Core。`has_namespace_capability` 提供按目标 user namespace 解释能力的共同判断。`PathHandle` 仍只有 Location/use，anchor 尚无到 Core 的弱回链，这些连接尚未组成 F1–F8 所需的挂载授权能力。

`MountContext` 只有文件系统、选项和 mount 列表，没有 creator 或 anonymous origin。Directory 原始 detached bool 在 attach/lazy-detach 后仍保留；attach 后与 lazy-detach 后均可为 Ordinary，后者 parent=None。Unbound 能区分从未发布，但不是 fsmount 独占来源证明；不能单凭原 fd 标志放行旧树。`open_tree` 当前为 Unsupported/ENOSYS，本计划没有授权顺便扩展新能力。

固定 Linux 寿命事实还包括：namespace 实体持有 user_ns 直到最后 passive 引用释放；普通 FSMOUNT 匿名 namespace 的 owner 是 fsmount 调用时当前 mount namespace 的 owner userns。原 O_PATH file 使用 fc->cred 并标记 FMODE_NEED_UNMOUNT，`dissolve_on_fput` 只在该 mount 仍是匿名 namespace 根时拆树，移出后关闭原 fd 不再管理它。这些事实尚未落实到项目接口。

## 5. 剩余计划

原批准计划继续以统一对象身份、真实授权尝试和受控生命周期为目标。以下是接手工作，并未在本次交接中实施；不能通过恢复旧旁路、补参数或统一返回 Unsupported 来消除迁移缺口。

### 5.1 实现缺口

消费者迁移依赖第 4 节的所有权及挂载授权合同，独立模块仍需按真实依赖完成以下工作。

- 对已迁入的 Owner/Snapshot、FS/MNT 成对绑定、clone/unshare/setns/exit 和 actual Core/creator 执行直接生产回归及独立审查。
- 验证 Kernel/ROOT 的有序 pivot 消费，并完成 F1–F8 授权与匿名树寿命。
- 统一 `LookupSession`/`Continuation`、40 次总链接预算、末组件续走、范围限制、真正非阻塞的 CACHED，以及 typed conflict 按原身份续走。
- lookup 成功与 NotFound 的代际接受、锁外退休和现有解析器/字符串捷径清理。
- proc typed link 及全部消费者、SimpleFS/proc producer、per-open userns、devpts 与 mqueue 接入。
- Overlay 稳定 inode、copyup、outcome、open/data/restore/xattr/cleanup 的完整迁移，消除 Kernel 15 处旧接口错误。
- chmod/chown/time、Native 自动时间与 archive restore 去 raw；AF_UNIX pathname 按原 caller 原子创建，不扩网络身份。
- Native Uncertain 与跨文件系统最后 Owner 的释放边界。
- 修正 `test-unshare-fs` 中“删除 cwd 后 unshare 应 ENOENT”的错误 Linux 前提，保留成功路径及真实失败回滚。

### 5.2 最终验收缺口

验证必须覆盖实际生产消费者和 Linux 可观察行为；局部库测试及类型名称不足以证明系统调用兼容性。

- 受影响格式化、项目 Clippy 和标准库测试；零选择不算通过，整体退出失败要保留。
- 根因系统回归先在一个架构取得真实红绿，再串行执行 x86_64、AArch64、RISC-V、LoongArch 定向 QEMU。
- 真实后端重试、错误副作用、跨调用者保护的最后引用释放、缓存回收后的数据与脏页保留。
- 同环境普通路径查找性能和资源滞留比较、逐系统调用六列表、完整独立审查。

## 6. 恢复工作与环境

本检查点已收束合并及保全本地改动。接手者可以从已推送分支继续，不需要重建会话历史来判断当前停点；但仍需根据接手时的主线和环境补充受影响证据。

### 6.1 本次冲突处理

唯一内容冲突是 `test-suit/arceos/rust/Cargo.toml`。解决保留上游 `049d6929b5` 的套件整合和 `suite.toml`，不恢复被整合删除的 exception-page-fault/futex-errno-order 独立 features，同时保留 `lockdep-baseline` 使用真实 MemoryFs 所需的 `dep:ax-fs-ng` 与 `ax-runtime/fs`。`scripts/axbuild/src/test/std.rs` 自动合并保留本分支 `host-test+fat` profile。

`Cargo.lock` 无冲突，合并结果与本次 `dev` 完全一致，没有手工拼锁或重生成。VFS/AX 源码在此次合入中相对 `13ad64fc...` 无差异；依赖和运行时已更新，因此仍需在接手后重新选择验证。合并前保护的并发技能参考 8 行新增已经逐字恢复，随交接提交。

### 6.2 验证入口与缓存

本节历史检查点使用 nightly-2026-09-04；后续主线已升级为 nightly-2026-11-13，接手时以 `rust-toolchain.toml` 为准。项目继续使用 Rust 2024 和 `python3`。构建、Clippy、标准库测试和系统测试继续使用 `cargo xtask`，不以原生 Cargo 入口替代。合并后 Clippy 实现在 `scripts/axbuild/src/checks/clippy/`，ArceOS 测试在 `scripts/axbuild/src/test/normal/arceos/`；旧路径已经迁移。

新的 ArceOS Rust suite 使用 schema 3 声明批次和构建生成流程，`suite.toml` 与 `build.rs` 是正常入口，不恢复旧 all 或旧独立测试 feature。接手者需先确认选择器命中所需测试；当前只记录示例，不代表交接时已运行：

```sh
cargo xtask arceos test qemu --test-group rust --arch riscv64 --list
cargo xtask arceos test qemu --test-group rust --arch riscv64 --plan
```

本次清理已完成：当前工作树根目录 `target` 在收尾核对时已不存在，未把历史清理量计入本次结果。删除本任务 e2fsprogs-1.47.3 临时源码、构建目录、工具副本和下载归档，移除实占 95,031,296 字节，约 90.6 MiB；文件系统可用空间同期增加 94,830,592 字节，约 90.4 MiB。删除前确认没有进程持用该临时树，构建目录中的 `config.log` 与 `parse-types.log` 另行复制并核验摘要，原日志目录和验证报告保留。

清理前后 HEAD、源码 tree 和干净工作区状态一致；实际记录为本会话 `artifacts/cleanup-result.json`，后续提交只补充此清理结果。源码、提交、设计、报告、原始日志、Cargo 全局缓存、邻居工作树及未知工具未删除。接手时应预期需要重新构建并重新取得临时 e2fsprogs 工具，原运行证据仍按本文件所列版本保留。

## 7. 接续实现

本节记录从交接提交 `b64f42bdbb0ccc41e15df7df6c8f10a359f7097e` 继续实施的结果。新增 AX 所有权组件对应主设计第 3.8.1、3.8.7、3.8.8 和 3.8.13 节；生产消费者迁移仍在后续范围，PR 保持草稿。

### 7.1 路径状态所有者

[`FsContextOwner`](../fs/ax-fs-ng/src/fs_core/owner/mod.rs) 明确保存任务份额，`FsContextSnapshot` 只保留本次选定的路径、真实 VFS Core 和来源 Weak。`share_for_child` 受检增加份额；`fork_owner` 从冻结路径创建独立状态，能够保留已经脱链的 cwd。namespace 属于各 owner 的绑定，共享 State 只保存 root/cwd 和份额。公开入口集中于 `ax_fs_ng::fs_context`，同时重新导出本接口使用的 VFS namespace、克隆映射和路径类型，避免把旧 AX namespace 误认为实际 Core。

[`PreparedFsUpdate`](../fs/ax-fs-ng/src/fs_core/owner/update.rs) 把 cwd、root 和 join 分为不同命令。提交先复核来源分配、namespace 及适用的 `SoleOwner` 条件，再在同一个 IRQ-safe 短锁内移动引用；成功旧值与拒绝输入都交回 `FsUpdateRetirement`。join 保持原共享路径状态，重设 root/cwd，只修改当前 owner 的 namespace；同 Core 的 join 也执行重设。路径内容已经合法变化时，原快照仍可准备仅修改 root 或 cwd 的命令。

[`FsContextSnapshot::pivot_root`](../fs/ax-fs-ng/src/fs_core/owner/pivot.rs) 通过实际 VFS Core 移动拓扑，再按 exact mount+dentry 修订登记的共享 State。全局可睡眠序列门覆盖这两个阶段，registry 修订不按 namespace 过滤。扫描时取得的 State 强引用与移出的路径继续交给 `FsPivotRetirement`，退出调用者 writer 后再释放。普通快照和路径修改不取此序列门。

### 7.2 确定性证明

5 个组件旅程使用真实 MemoryFs、VFS 挂载树及两个宿主线程，分别覆盖共享路径与任务 namespace 分离、冻结快照 fork 和实际克隆映射、成败更新的最后路径退休、两次真实 pivot 的顺序，以及 registry 最后 State 的退休。它们不执行 Starry 系统调用，也不替代真实内核运行时验证。

错误提交 `ca8a095c38` 仅在 VFS 拓扑移动后提前释放序列门。相同的 `pivots_order_real_topology_and_exact_shared_path_revisions` 在全部 5 个功能组合中都因生产序列门未覆盖修订阶段而失败；恢复提交 `55894fad2e` 通过同一旅程。红绿使用同一个项目入口和选择基线：

```sh
cargo xtask test --since b64f42bdbb0ccc41e15df7df6c8f10a359f7097e
```

恢复后的 `ax-fs-ng` 5 个 profile 全部通过，单元数量依次为 268、157、137、238、128；每个 profile 还执行相应集成测试和禁止复制 Owner 的 compile-fail doctest。初版曾因测试并发扫描同一 registry 而误判 State 寿命，已对共享该全局资源的测试使用同一隔离门；这次用例修正不计作产品回归红证据。公开重导出整理后的最终代码检查记录另保存在 [本轮证据](vfs-context-owner-evidence.json)。

### 7.3 生产迁移边界

`FS_CONTEXT`、ROOT、Kernel clone/unshare/setns/exit 和现有 `FsContext` 尚未迁入这些新类型。旧 AX namespace 与新 VFS Core 的生产连接、creator、Scope 中 FS/MNT 成对安装，以及全部调用者保护外退休，仍须在下一实现单元中完成。当前类型测试与组件红绿没有解决这些系统调用路径，也没有取得独立审查批准或性能结论。

本轮重新执行的 Kernel Clippy 复现原有 Overlay 15 处旧接口编译错误，首项失败后 71 项未运行。标准库选择器选中 18 包，恢复后的 17 包通过，唯一失败仍为 `starry-kernel`，整体退出码为 1。未执行 QEMU、四架构系统调用回归或性能比较；第 4 节与第 5 节其余未完成项继续有效。

## 8. 工具链升级接续

本节记录合入 `dev@f55295b7890ee6ec6984222a840d55355cadfd27` 后的适配。受验证的最终源码为 `827e40092cd245875c98b51f9e0a121974802cdf`，精确工具版本、文件摘要、命令、逐包结果和原始日志摘要保存在[工具链证据](vfs-rust-toolchain-evidence.json)。前节使用旧工具链的结果继续作为历史记录，不由本节覆盖或改写。

### 8.1 编译器契约

合并提交 `3e596259d6` 引入主线 #2807 的 `nightly-2026-11-13`。实际 rustc 从 `1.100.0-nightly/a69a63265`、LLVM 23.1.1 升至 `1.102.0-nightly/8eb778d04`、LLVM 23.1.5。版本以 [`rust-toolchain.toml`](../rust-toolchain.toml) 和证据中的 `rustc -Vv` 为准；本轮按两个精确版本的标准库源码核对调用契约。

| 变化 | 对本分支的影响与处理 |
| --- | --- |
| `Allocator` 稳定化后拆分 `allocator_ext`，删除内部 `Unique` | 保留主线对应 feature gates 与 x2apic 0.5.1；未在受影响本地代码发现直接使用 `Unique` 或 `ptr_internals` 的消费者 |
| musl `DirEntry::metadata` 改用目录句柄 | 保留 `DirHandle` 与各 `*at` 接口，按实际目录对象解析相对名；不能恢复 fd 到旧路径文字的缓存 |
| 整数新增固有 `bit`、`set_bit` | 保留主线虚拟中断与 hypervisor 寄存器中的 trait 显式调用，避免把原地修改误改成返回新整数 |
| Clippy 建议栈固定、容器移动与清空 | 保留依赖提前析构的取消测试中的 `Box::pin`；栈固定仅用于不以提前析构取消的调用 |
| 非增量宿主测试的链接边界 | 保留 runtime、axvm 的 exception table 与 TSS 宿主定义，实际使用 `CARGO_INCREMENTAL=0` 检查；不把该项目观察归因为未经核实的 LLVM 保证 |

分配接口变化可对照 [Allocator 稳定化](https://github.com/rust-lang/rust/pull/156882)及[内部指针类型移除](https://github.com/rust-lang/rust/pull/164146)。目录项行为直接比较[旧版实现](https://github.com/rust-lang/rust/blob/a69a63265cfd9e006d43137f98301b8d274ad4c9/library/std/src/sys/fs/unix.rs#L1019-L1063)和[新版实现](https://github.com/rust-lang/rust/blob/8eb778d04094d3af4b26580cab327e0413eb1e02/library/std/src/sys/fs/unix.rs#L1050-L1076)，不从更早的目录 API 拉取请求推断本次 musl 消费迁移的时间。

### 8.2 冲突与真实消费者

两个内容冲突位于 `fs/ax-fs-ng/src/fs/fat/file.rs` 和 `os/arceos/api/arceos_posix_api/src/imp/fs.rs`。前者保留本分支 `FatInode` 的规范持有者及同一 filesystem guard，把主线的 metadata、write、append、truncate flush 接到实际文件句柄。后者保留 `sys_open` 转入 `sys_openat` 的句柄式路径，由 `fops::open_at` 显式接收当前原生调用凭据。

实际 `fs-basic` 运行还暴露关闭后的 FAT 脏缓存阻止删除。`prepare_fat_unlink_cache` 在后端 gate 外写回同一 FAT 中仅由 registry 与本次写回快照持有的缓存，再沿既有 prune 路径释放。并发重新打开仍可使真实 FAT 尝试拒绝操作；准备阶段不授予删除权限。`DirNodeOps::prepare_unlink` 改为返回 `VfsResult`，把写回错误传回外层，未丢弃脏页或放松活跃句柄保护。

Cargo 新诊断暴露的依赖按实际功能边界启用；无消费者的 `rd-net/futures` 与 `starry-signal/event-listener` 删除。`Cargo.lock` 在合并时没有冲突，后续由 `cargo update --workspace --offline` 自动更新这两项依赖边，没有手工拼接。Git 测试明确初始化夹具分支；Ext4 增长成功夹具明确关闭当前增长实现不支持的 mkfs 默认功能，保留容量、内容及中断恢复断言。

### 8.3 确定性回归

既有 `fat_unlink_preserves_active_identity_and_releases_closed_cache_owners` 增强为关闭前仍有未写回数据。`f9c0162731` 中三个 FAT profile 都在预期 `Applied` 处稳定得到 `Unchanged`；修复后同一旅程全部通过，活跃句柄拒绝、原身份与内容保留、关闭后的回收及真实删除仍一起验证。

既有 `fs-basic` 增强为保留 `DirEntry`，关闭迭代器并重命名父目录后读取元数据。临时错误实现把目录 fd 绑定到 cwd，同一测试稳定于 `metadata()` 返回 `ENOENT`，最外层退出 1；逐字恢复生产源码后退出 0。错误变异未推送。元数据断言后显式释放保留项，使后续清理不额外要求 FAT 支持删除仍打开的目录。

该成员继续复用原 suite 和批次，没有新增默认启动。定向验收在 x86_64、AArch64、RISC-V、LoongArch 各实际选择一次 `fs-basic`，均取得逐项 `status=pass`、套件最终成功标记和外层零退出。这些是 ArceOS 标准库适配证据，不替代 Starry 直接系统调用验证。

### 8.4 验证终态与剩余边界

9 包 134 项定向 Clippy 通过；后续测试改动另通过套件 43 项及 AX/Rsext4 9 项检查。最终非增量标准库入口实际选择 21 包，20 包通过，唯一失败为 `starry-kernel` 原有 Overlay 的 E0061 十处与 E0407 五处，整体退出码为 1。AX 五个 profile、VFS、Rsext4、axbuild、axvm 与宿主虚拟化装配均实际完成，不能把局部通过写成 PR 全绿。

本机 e2fsprogs 1.46.5 不识别已有增长夹具的 feature 参数。本轮从 kernel.org 获取并核对 SHA256 后构建 1.47.3，只以命令局部 PATH 选用本任务二进制，没有替换宿主工具或修改 Git 全局配置。标准库入口为：

```sh
PATH=<isolated-e2fsprogs-1.47.3>/bin:$PATH CARGO_INCREMENTAL=0 cargo xtask test --since origin/dev
```

宿主 `ax-hal` 的条件平台依赖告警，以及 x86 soft-float 下工具链和第三方库的 target-feature future-incompatibility 告警仍有记录，未加 `allow` 属性。生产 Owner/Scope/creator、Overlay、统一查找与 proc 消费迁移、Starry 系统调用回归、性能和独立最终审查仍未完成，第 4、5 节继续有效。

## 9. 生产所有权迁移

本节记录工具链适配后的生产接入，受验证源码为 `cb65d894c1dd45ba0dc77fef36ccdbde36d8f3c7`。源码、原始日志摘要及逐组合结果保存在[生产连接证据](vfs-production-owner-evidence.json)。本节推进第 4、5 节的 Owner 和路径使用权工作，完整 PR 继续保持草稿。

### 9.1 任务路径与命名空间

AX 的 `ROOT_FS_CONTEXT` 和 `FS_CONTEXT` 保存 `FsContextOwner`，读取返回冻结的 `FsContextSnapshot`。`FsContext` 只作为绑定实际 Core 的不可变路径视图，旧 namespace 包装、可变上下文和 registry 旁路已删除。ROOT 根切换借用聚合 pivot，Native 的目录切换通过准备命令及短 Scope 提交；Axvisor 的归档视图使用冻结快照和实际克隆映射。

Kernel 的 [`FsBindingSnapshot`](../os/StarryOS/kernel/src/task/fs_binding.rs) 在同一进程 writer 下取得 FS/MNT，单空组合被拒绝。`MntNamespace::from_created` 消费实际 Core 的创建凭据并保存 creator，nsfd 保持同一 Core 身份。`NamespaceChange` 把 clone、unshare、setns、cwd/root 更新和 pivot 的准备、短 Scope 发布及 writer 外退休连在一起；退出在转入 zombie 前退休 FS/MNT。`NsProxy` 不再另存可独立替换的 mount namespace。

这些连接已消除生产代码中的旧所有者通道，但没有取得 Starry 直接 syscall 运行证明。`setns` 的 mount/user 分支使用目标 user namespace 能力判断，多 flag pidfd 路径按原调用者身份准备；其余 namespace 的既有安装策略仍待逐入口对照。F1–F8、匿名挂载来源及 Core 反向查询也没有因这次迁移自动实现。

### 9.2 打开路径与映射

`FileBackend::Direct`、`CachedFile`、打开的 `File`/目录、`DirHandle` 和 `ReadDir` 保留实际 `PathHandle`，写入及执行互斥租约也保住所选路径。目录的打开结果直接转移拥有型路径，缓存锁、描述表锁和后端 gate 内不重新取得 mount use。缓存及目录读取状态先于最后路径守卫退休。

Kernel 的设备映射把原文件路径放入实际映射保留对象，缓存设备映射同时保留原设备路径及缓存 backing。关闭 fd 后仍活着的映射因此继续承担所选挂载的使用权；此处没有改变映射、页缓存或设备 I/O 算法。

`opened_paths_keep_mount_uses_through_dup_close_and_lazy_detach` 使用真实 MemoryFs 和挂载树，覆盖 direct、buffered、O_PATH、目录、dirfd 及迭代器。持有或复制时普通卸载返回 Busy，最后关闭后可规划卸载；规划后新打开仍使提交失败，lazy detach 后原对象继续可用。错误提交 `ed9d882da4` 的五个 AX profile 均在 direct 路径未保住挂载处失败；修复后相同断言全部通过。测试夹具的挂载根同时修正为被测实际节点，避免把不同文件系统硬包在同一 mount 中。

### 9.3 当前验证结果

源码 `cb65d894c1` 的 AX/runtime Clippy 34 项全部通过。标准库入口选择 18 包，17 包通过；AX 五个 profile 的单元数为 269、158、138、239、129，各自同时通过相应集成和文档测试。唯一失败仍为 Kernel Overlay 的 E0061 十处与 E0407 五处，整个标准库入口退出 1。

本轮按相同源码串行运行四架构 `fs-basic`，x86_64、AArch64、RISC-V 和 LoongArch 均取得逐项 `status=pass`、最终套件标记及外层零退出。Axvisor AArch64 的 memory-root `axtest` 实际运行 125 项，结果 `pass=125 fail=0 skip=0`，输出 `AXTEST_SUITE_OK`。原生运行证据覆盖了本轮路径接入，不提升 Starry 系统调用兼容性结论。

Kernel Clippy 的 72 项计划在前四项因同一 Overlay 错误失败，其余 68 项未执行。完整 Overlay 迁移、统一查找、proc/metadata 消费、直接 syscall 红绿与四架构回归、性能及资源比较、最终独立审查仍未完成。验证期间 `dev` 又推进至 `1b3ad931870097e2e2a295d7da20508f3f2102e5`，该提交不属于本节源码及结果，后续合入必须另记冲突和受影响验证。

### 9.4 主线权限修复同步

合并提交 `be5915967bfeb4d1e98de6b62f5e4751e434eae0` 随后合入 `dev@1b3ad93187` 的 #2808。四处内容冲突为 namespace 模块、user namespace、clone 和 unshare/setns：保留本分支的成对绑定及 writer 外退休，使用准备时取得的 creator euid 创建子 user namespace，保留主线允许 level-32 父创建 level-33 子、下一次返回 `ENOSPC` 的边界。`prlimit64` 消费共同的 `has_namespace_capability`，避免为同一不可变祖先关系保留第二套遍历；初始 user namespace 的 hard-limit 提升门禁仍独立存在。

其余主线权限、信号、IPC、文件传输修复和系统回归完整保留。此合并没有改变工具链或锁文件；AX、ArceOS 及 Axvisor 源码也与本节运行版本相同。合并后 Kernel 定向 Clippy 在首项仍因 Overlay 的同一 15 处错误失败，余下 71 项未执行；标准库增量选择器准确选中 Kernel 一包，同样失败。宿主测试另报告上游 seccomp helper 中未使用的 `alloc::vec` 导入，尚未把这些失败记为通过。

能力继承的源码对照固定于 Linux v7.1 的 [`cap_capable_helper`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/security/commoncap.c#L68-L106)。本次同步只证明冲突处理和当前类型检查的边界，直接 syscall 验收仍须在 Overlay 迁移完成后执行；对应日志摘要附在同一生产证据的 `followup_mainline_merge` 中。
