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

接手者首先需要区分已冻结但未实现的 Owner/pivot 设计，与尚待裁定的新挂载授权设计。不能在迁移时用猜测补齐实际 Core、creator 或匿名来源证明。

### 4.1 已冻结的上下文迁移

主设计第 3.8 节要求 `FsContextOwner` 不可 Clone，每个 Owner 保存自己的 namespace，State 只共享 root/cwd 与实际 Owner 份额并由短 IRQ-safe 锁保护。`FsContextSnapshot` 持来源 Weak、冻结 Core 和路径，不计入 Owner 份额，也不升级原 State 的所有权。共享 root/cwd 的 Owner 可以属于不同 namespace，registry 每 State 登记一次 Weak。

Kernel 必须成对读取 FS/MNT Scope：双空表示退休，单空是内部错误，不能 fallback 到 ROOT。clone/unshare/setns/exit、Native 和 Kernel 应同序接入，清除旧 live-State 通道。单 NEWNS、含 USER、多 flag pidfd setns 要分别保持原共享份额、权限与写回规则；已有 IPC 文件对象连接仍在范围，网络/IPC 业务身份分配不在范围。

### 4.2 已冻结的有序根切换

主设计第 3.8.8、3.8.13 节冻结 AX 聚合 `FsContextSnapshot::pivot_root(&self, new_root: &PathHandle, put_old: &PathHandle) -> (VfsResult<()>, FsPivotRetirement)`，尚未实施。AX 私有可睡眠序列门覆盖“VFS 移动 → topology 外 exact registry 扫描”，避免两次拓扑提交 1→2、修订扫描 2→1 导致最终 State 指向错误路径。

锁序为 caller writer → AX 序列门 → topology；先退 topology 再扫描 registry/逐 State，Scope、pin 和 State 短锁不能跨可睡眠门。修订按 exact mount+dentry，不按 namespace 过滤。成败都保存退休值并在 writer 外释放。普通 pivot 不替换 Owner/Mnt 适配、不重新发布 NsProxy。Kernel 与 ROOT 都必须迁入聚合入口，保留 bind → 同点 pivot → exact 修订 → detach 的顺序；当前生产消费者尚未迁入。

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

当前 Kernel 旧 `MntNamespace` 只有诊断 ID，没有实际 VFS Core 或不可变 creator；两套同名 namespace 仍未接通。`PathHandle` 只有 Location/use，anchor 无到 Core 的弱回链；nsfd 缓存是旧 AX namespace Weak 到 inode，不能反查 mount 的 namespace。现有能力 helper 固定于 ptrace，没有可直接复用的目标 userns `CAP_SYS_ADMIN` 判断。

`MountContext` 只有文件系统、选项和 mount 列表，没有 creator 或 anonymous origin。Directory 原始 detached bool 在 attach/lazy-detach 后仍保留；attach 后与 lazy-detach 后均可为 Ordinary，后者 parent=None。Unbound 能区分从未发布，但不是 fsmount 独占来源证明；不能单凭原 fd 标志放行旧树。`open_tree` 当前为 Unsupported/ENOSYS，本计划没有授权顺便扩展新能力。

固定 Linux 寿命事实还包括：namespace 实体持有 user_ns 直到最后 passive 引用释放；普通 FSMOUNT 匿名 namespace 的 owner 是 fsmount 调用时当前 mount namespace 的 owner userns。原 O_PATH file 使用 fc->cred 并标记 FMODE_NEED_UNMOUNT，`dissolve_on_fput` 只在该 mount 仍是匿名 namespace 根时拆树，移出后关闭原 fd 不再管理它。这些事实尚未落实到项目接口。

## 5. 剩余计划

原批准计划继续以统一对象身份、真实授权尝试和受控生命周期为目标。以下是接手工作，并未在本次交接中实施；不能通过恢复旧旁路、补参数或统一返回 Unsupported 来消除迁移缺口。

### 5.1 实现缺口

消费者迁移依赖第 4 节的所有权及挂载授权合同，独立模块仍需按真实依赖完成以下工作。

- Owner/Snapshot、AX、Kernel、Native 同序接入，FS/MNT 成对绑定，以及 clone/unshare/setns/exit 迁移和实际 Core/creator 连接。
- AX 有序 pivot 聚合、Kernel/ROOT 消费者及 F1–F8 授权与匿名树寿命。
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

项目使用固定 nightly-2026-09-04、Rust 2024 和 `python3`。构建、Clippy、标准库测试和系统测试继续使用 `cargo xtask`，不以原生 Cargo 入口替代。合并后 Clippy 实现在 `scripts/axbuild/src/checks/clippy/`，ArceOS 测试在 `scripts/axbuild/src/test/normal/arceos/`；旧路径已经迁移。

新的 ArceOS Rust suite 使用 schema 3 声明批次和构建生成流程，`suite.toml` 与 `build.rs` 是正常入口，不恢复旧 all 或旧独立测试 feature。接手者需先确认选择器命中所需测试；当前只记录示例，不代表交接时已运行：

```sh
cargo xtask arceos test qemu --test-group rust --arch riscv64 --list
cargo xtask arceos test qemu --test-group rust --arch riscv64 --plan
```

本次停止后清理当前工作树编译缓存及明确归属、停用且可重建的临时构建产物，实际结果写入 PR 和交接收尾记录。源码、提交、设计、报告、原始日志、Cargo 全局缓存、邻居工作树及未知工具不在清理范围。接手时应预期需要重新构建，原运行证据仍按本文件所列版本保留。
