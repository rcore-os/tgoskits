# 文件系统所有权重构接续记录

## 1. 交接边界

本文件保留原交接的版本证据，并持续记录后续实施。文件系统重构计划尚未完成，
PR [#2800](https://github.com/rcore-os/tgoskits/pull/2800) 保持草稿；后续源码、验证
和缺口分别记录，局部验收不构成整体合入批准。

### 1.1 目的与预期收益

路径是名称到文件实体的解析过程，文件句柄则保留已经取得的对象。现有问题来自这些边界混用：rename 后仍沿旧名称关系行走，proc 链接的显示文字被拿来重新查找目标，授权后释放后端锁却继续使用旧授权，以及最后引用在调用者锁内析构。局部补错误码不能约束后续调用是否继续犯同类错误。

重构通过稳定 dentry、独占 `LookupSession`、借用真实守卫的提交能力、对象型链接和领域元数据命令，把身份、权限检查、实际提交及资源释放归到明确的所有者。预期收益是统一普通路径与受限查找的语义，保留 rename/unlink 后句柄的正确身份，减少跨重试授权及锁内析构错误，并让非法顺序更早被接口和测试发现。性能目标是复用现有缓存与锁域、避免全局串行路径查找；尚无完整前后性能数据，不能宣称性能已经提升。

### 1.2 版本与历史

接续沿用 `fix/vfs-lookup-ownership-linux71`，目标分支为 `dev`。原批准计划仍是
实施目标；接口与行为约定集中在 [主设计](vfs-lookup-ownership.md)，本文件记录
实施状态与缺口。下面保留原交接阶段的版本，后续工具链和生产连接见第 8、9 节。

| 对象 | 精确版本或状态 |
| --- | --- |
| 实施开始时的主线 | `94a2d5e51360f483c9ea253da4babb2fe00d3fdb` |
| 最近已局部验收的代码 | `13ad64fcac5c226e981be3f023a3e3f87a3c8ec8` |
| 本次合入的主线 | `a15042772543065e16e48e15e5f472757004d0ec` |
| 本次合并提交 | `8aaa54b4f3d47a8126250f8cd1fbe3ec01764144`，保留上述两方父提交 |
| Linux 语义基准 | v7.1 源码提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，tree `96667ed2997d7b49f3e9a36c4297b1faca4d87e3` |
| 原交接检查点 | 含本文件的原交接提交；后续推送 HEAD 由 PR 正文及生产证据记录 |

Linux 依据是固定源码分析，不是 Linux 7.1 实机运行证明。与 #2481 有关的根因修复在本独立分支推进；不带入原 PR 的 eBPF、网络/IPC 业务身份、OOM、starttime、ELF 或 Docker 工具链，不改变磁盘格式。

### 1.3 当前生产进展

写打开阶段源码 `aa14e2ca77` 接通原写准备下的物化、原生数据端点和 backing inode
执行互斥。18 包标准库及 VFS、AX、Kernel 的 81 项 Clippy 全部通过，四架构真实
ownership 子用例均为最外层 1/1 通过；前三个架构打印 103 pass、0 fail，LoongArch
成功 stdout 沿用 capture-failures 策略。当前阶段与错误候选见第 9.21 节，前一阶段
的 90 项 Clippy、statx 及 namespace 证据保留在第 9.19、9.20 节。
`df0536aae1` 随后修复已删除 cwd 的 getcwd 判定，Kernel 增量标准库、72 项
Clippy 以及 x86_64/LoongArch 的完整 unshare 子用例通过，见第 9.22 节。

组件查询阶段 `e828831525` 区分候选发布冲突、父命名冲突与真实后端错误，普通
FsContext helper 只对前两种结果合作重查。18 包标准库通过，初始四包的 43 项
Clippy 及最终 AX 的 6 项检查通过。缓存退休与夹具修复见第 9.23、9.24 节；
普通解析与打开的统一会话及总链接预算已在 `88bed9cdf7` 接通，见第 9.25 节；
checked/trace/parent 其余消费者及 scoped/CACHED 仍待迁移。

动态只读描述、旧数据 hook 的其余消费者、FAT 合并 holder 分类、统一查找与 proc/SimpleFs
完整迁移、挂载授权、逐系统调用表、性能及独立审查继续按原计划实施。原交接的
编译失败和局部批准只描述其明确版本，不能据此判断当前源码或完整计划已经完成。

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

证据按精确版本保留。以下运行和审查发生在原交接合并 `dev` 之前；原交接阶段
只核对冲突、差异与文档，没有重跑当时的合并版构建、Clippy、标准库或 QEMU。
后续验证另见第 8、9 节，历史结果不直接标为当前源码通过。

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
- Overlay 的稳定 inode、root、固定 backing、依赖及 mounter 已接入；继续完成 copyup、outcome、open/data/restore/xattr/cleanup 及命令准备的实际消费，消除剩余 5 个旧目录方法。
- chmod/chown/time 已提交意图命令，SimpleFs 已使用真实 metadata 锁；继续完成全部后端及 fd 类、Native 自动时间及其余内部 raw 入口，并取得直接 syscall 证明；initramfs 已迁入显式初始化恢复。AF_UNIX pathname 按原 caller 原子创建，不扩网络身份。
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

### 9.5 打开描述的后备写权利

源码 `fed029e6c98310375909d888c6f5fbb02a3af68e` 进一步让 `WriteAccess` 持有完整 `PreparedWrite`。可见 mount 和声明的写入 backing 都保持真实写租约，只读依赖保留原角色；dup 或映射继续延长同一个打开描述的权利。字段退休顺序为 inode 访问登记、完整准备、所选路径使用权，执行租约也在移除登记后才释放路径。

既有 `readonly_reconfiguration_preserves_writers_until_description_and_mapping_release` 增强为两层声明依赖，并使用真实 VFS lease、描述符复制和 remount 实现。只读 lower 不阻止打开，仍存活的写描述或映射则必须阻止中间层和真实 upper 切为只读；后备拒绝还须释放之前取得的权利。`09b1002e72` 的五个 AX profile 均稳定得到错误的 remount 成功，修复后同一旅程全部通过，没有新增测试启动或放宽断言。

该源码的 AX/runtime Clippy 34 项、x86_64 `fs-basic` 和 Axvisor AArch64 125 项均通过。标准库仍选中 18 包、通过 17 包，唯一失败为既有 Kernel Overlay 15 处接口错误。对应结果记录在生产证据的 `prepared_write_access`；分层数据/open、restore/xattr 和跨全部 gate 的 copy-up 准备协调仍待实现，这一步没有接通完整 Overlay。

### 9.6 元数据的实际所有者

`638f4416cb` 到 `1e3732044c` 将 SimpleFs 的字段修改放入真实 metadata 锁，并让
`syscall/fs/metadata.rs` 的 chmod/chown/time 直接提交原始意图。Chown 的省略字段
保留为 None，NOW 在 inode 尝试内取时钟，普通 fd 的 O_PATH 拒绝与 *at 的空路径
区分处理。`file/path.rs::descriptor_path` 从保留的打开描述选择原生 PathHandle，
proc 与元数据入口共用该投影；不把显示文字当作授权路径。全部 fd 类型、typed link
流水线及 raw 自动维护接口仍未迁完，SimpleFs/proc 的根引用环也仍待处理。

`8786f2c86b` 增强已有 utimensat 旅程，通过原始 syscall 验证两项 OMIT 早返回、
NULL 的 fd 分支、O_PATH 与空路径、flags 和时间参数的错误顺序及拒绝后时间保留。
测试保留原 11 项、原分组及启动，使用独立 mkdtemp 目录。相同源码在宿主
Linux 6.8.0-149-generic x86_64/GCC 11.4 运行 11 项通过；语义源码仍固定于
Linux v7.1 的 [`fs/utimes.c`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/utimes.c)。
这不是 Linux 7.1 或 Starry 的运行结果。负 epoch 时间的既有 Duration 表示限制
也没有因本轮迁移解决，直接系统调用兼容性仍为无法确认。

### 9.7 分层实体及后备身份

`67bedf0655` 将 Overlay 拆为 `pseudofs/overlay/mod.rs` 与 `inode.rs`。wrapper
保留稳定逻辑实体，upper 切换不重新按旧文件名查找，metadata 的 inode/device
投影到本实例。弱索引使用实际 backing owner、实体身份或保活 inode，以及
index=off 的 lower 硬链接别名身份；root 实体不反向持有 FS，root dentry 为弱引用。
当前目录关系来自 VFS 的同步名称快照，旧 path/parent_path 字符串已删除。

挂载入口在解析 backing 前保存成对 FsCredentialsSnapshot，并检查路径搜索权限。
转换后的 VFS 凭据由该快照统一借出，能力仍限定在已支持的初始 user namespace。
内部单名读取使用固定 backing 查找；upper/work 必须是同一 exact mount 的互不包含
目录。读写依赖及不超过两层的深度不可变，普通 poll/flags 不执行 copy-up。
完整 owner gate、原生暂存及复合结果尚未接通；这些源码连接不构成完整 Overlay 验收。

### 9.8 跨尝试准备的命令所有者

`c52ef18f20` 与 `e3ecb0ebbb` 将 NamespaceCommandScope 接入所有原生命名和
元数据便捷入口。每轮 NamespaceOperationScope 借用它，结束全部 pressure、
admission、retire 及 finish 后才允许 prepare；下一轮重新观察及授权。准备项按
实际 receiver 与实体身份分开，落败候选交同一 retire，终止时显式 cleanup，原始
错误与 Applied/Uncertain 继续保留。没有新增全局任务登记或 Drop 磁盘回滚。

既有 `completion_pressure_preserves_typed_results_and_mismatched_owners` 扩充同一
递归旅程：实际 Scope/请求/准备图负责派发，计划探针只记录 prepare/cleanup/析构，
验证同局部键的跨 FS 隔离、全部完成后的准备、重新进入、失败清理及最后引用。
临时错误源码 `91125ca99e` 移除准备候选的外层退休，同一用例稳定记录 admission
退出前析构，VFS 为 69 过、1 败。该变异未推送。恢复 `e3ecb0ebbb` 后 VFS 70 项
单元及相应集成/文档通过；完整标准库仍为 18 包中 17 过，唯一失败为 Kernel 的
5 个 Overlay 旧目录方法。探针使用非阻塞记录并断言记录完整，失败不再引发额外析构恐慌。

源码 `c52ef18f20` 的 VFS/AX/Rsext4 12 项 Clippy 通过，探针调整后 VFS 3 项再通过。
恢复源码的 x86_64 fs-basic 与 Axvisor AArch64 125 项均通过。其余三架构的最近
fs-basic 证据仍属于 cb65，不能提升为本轮结果。源码 `05cacb742b` 的 Kernel Clippy
首项与标准库单包检查均只剩 5 个 E0407；其余 71 项 Clippy 仍未执行。主线引入的
seccomp 测试冗余导入已移除，没有增加 allow。详细日志摘要记录在生产证据的
`metadata_and_overlay_foundation` 与 `namespace_command_preparation`。

### 9.9 初始化属性的受控恢复

源码 `2648e697c3` 让 initramfs 的 root 和条目属性使用
`TrustedInitialization::restore_metadata`，删除这两处 raw 更新。VFS 私有的
MetadataIntent 区分普通用户命令与显式初始化来源，继续借用真实写准备和
Command/Scope，并在实际 inode 尝试中更新字段。初始化保持请求中的所有者、
SUID/SGID、符号链接权限、设备号及时间，仍接受只读和后端能力拒绝；用户系统调用
没有新增选择初始化来源的参数。Memory/SimpleFs 的共享 apply 和 Ext4 原生字段
投影支持设备号，FAT 保留其实际能力边界。此能力不能用于 Overlay 的运行期 root 回退。

现有归档、设备及 Ext4 元数据完成旅程增强初始化判定，没有新增测试入口或启动。
错误源码 `669fc32475` 把普通 chown 的清位策略套到初始化字段，五个 AX profile 的
同一归档旅程都将预期 06755 得到 0755，Ext4 元数据旅程也将 06750 得到 0750。
恢复源码后相同旅程通过，并验证符号链接 mtime、设备号及只读失败后的原字段。
错误变异只存在本地，没有推送。

该源码的 9 项 VFS/AX Clippy、x86_64 fs-basic 及 Axvisor AArch64 125 项均通过。
标准库入口依旧为 18 包中 17 包通过，唯一失败仍为 Kernel 的 5 个旧 Overlay 方法。
Overlay 的封闭暂存、运行期恢复来源、最终 caller/mounter 属性传递及其余内部 raw
消费者仍待迁移，不能据此宣称 Starry 可运行或系统调用已兼容。证据位于
`initialization_metadata_restoration`，后续继续从第 4、5 节和主设计推进。

### 9.10 恢复时的时间字段参与

原初始化接入还继承了用户命令隐含的 ctime 更新。`ac5c4edbfc` 在既有确定性时钟
旅程中追加一次只参与 mtime 的恢复，同一源码在五个 AX profile 均将应保留的
ctime 10005s 改成 10006s。`865d2a7db3` 将授权字段的 ctime 改为 Option，普通
用户命令仍在实际尝试中取时钟并传 Some；初始化保持 None，并且不取时钟。
Memory/SimpleFs、Ext4 和 Cgroup 的消费者同时适配，不从低精度旧快照重造 ctime。

修复后同一时钟、归档、设备和元数据旅程通过；VFS/AX 9 项 Clippy、x86_64
fs-basic 与 Axvisor AArch64 125 项通过。标准库仍选中 18 包、通过 17 包，Kernel
定向 Clippy 首项和标准库仍只失败于 5 个旧 Overlay 方法，其余 71 项 Clippy 没有
执行。主线已再次核实仍为 1b3，工具链未变。字段参与的红绿和最终源码检查保存在
`initialization_metadata_restoration.ctime_omission_followup`，此前 9.9 的初次证据保留原版本。


### 9.11 分层属性意图

`918401f144` 增加实际授权借用的 `forward_to`。每层按自身 mounter 重新授权，
保留外层过滤的 Chmod、首次 Chown 的 kill 意图以及 Keep/Now/Set；最终原生 owner
重新取 NOW。只允许原 exact-mount 写权对应的声明下降边，原 retire 和完成登记继续
共用。Overlay 私有适配仍负责选择其固定 upper，不能以可变祖先链代替实体来源。

同一两层组件旅程在 `ab6c7bffdb` 重放原始 chmod 时稳定得到 02644，而非 0644，
VFS 为 70 过、1 败；恢复后 71 项及相应集成、文档通过。VFS 三组合 Clippy 通过，
完整标准库为 18 包中 17 过，Kernel 仍为 5 个旧 Overlay 方法。组件探针不是实际
Overlay 或系统调用证明。证据为 `layered_metadata_forwarding`。

### 9.12 原生暂存所有权

`d9be25a23a` 接入 `StagingNode` 与消费式封存：只从实际排他创建取得新对象，
属性来自实际 source，恢复仍检查保存的 mounter，seal 检查完整长度并结束恢复能力。
原生 rename 核对两个槽位，observed-unlink 核对对象后才清理，实际移出与完成错误
分别保留。暂存共享原不可变准备图的寿命，没有再次取得写租约。原生数据复制使用
既有 canonical cache，不使用 root OpenOptions；所有端点在恢复/封存前结束。

`65043f0377` 为新增的公开 API 集成进程安装现有 OS 接口所需的宿主页提供者，
首次缺该能力的 NoMemory 是夹具问题。真正错误变异 `987cea61da` 跳过原生 rename
来源身份，同一 MemoryFs 旅程在五个 AX profile 均错误安装替代对象，返回 Applied/Ok；
预期为 Unchanged/BadState。恢复后同一旅程全部通过，并证明内容、06755/属主/时间、
mounter 拒绝、未复制内容拒绝封存、安装/清理身份、放弃清理及最后准备寿命。

最终源码 65043 的 VFS/AX 9 项 Clippy、x86_64 fs-basic 与 Axvisor AArch64 125 项
均通过。标准库仍为 18 包中 17 过，唯一失败为 Kernel 的 5 个旧 Overlay 方法。
MemoryFs 与 Ext4 使用既有活跃脱链机制；FAT 保留 holder 保护，暂存前明确拒绝。
Ext4 尚未取得工厂本身的专属运行旅程。复合数据/open、xattr、祖先完整暂存和逻辑
效果汇总尚待接入 Overlay，不能把这一原生组件结果写成完整 copy-up 或 ABI 通过。
新增宿主页提供者含明确所有权/布局证明的少量 unsafe，仍需最终独立审查。精确源码和
日志摘要见 `native_copy_up_staging`；后续仍按第 4、5 节继续。


### 9.13 原生数据端点

`b064cb685d` 将 `FileIoRequest` 接到已有原生暂存复制，沿每层实际保存的 mounter
选择 canonical cache 的真实 owner，并保留全部中间 PathHandle。写端点要求原
PreparedWrite 的 exact-mount 权利及声明写边；读边不能补写权。Overlay 的 hook
只选择已有 upper，物化仍须先由命令完成。

错误变异 `9aaafb59d4` 允许读边用于写端点，在同一 VFS 组件旅程中绕过预期的
CrossesDevices；正确源码通过 71 项及集成、文档测试。该源码的 9 项 Clippy、
x86_64 fs-basic 与 Axvisor AArch64 125 项通过；标准库为 18 包中 17 过，Kernel
标准库及 Clippy 首项仍只失败于 5 个旧 Overlay 方法。普通描述的动态端点、打开前
物化、全部 native writer 与映射仍待接入；接受过的 fd 权限不能被每次 I/O 重新
授权。证据为 `native_file_io_selection`。

### 9.14 后备读取的完成责任

`51e1a68dc0` 将 Metadata/Lookup/Xattr 的读取完成登记到同一 Scope，调用者只取得
Ready/Retry 快照。Ext4 保留真实 staging 或核心 journal-progress 原因，全部 gate
结束后才执行压力，所有 admission 退出及 retire 结束后才释放 finish 所有者。
`522b8c9c1a` 的 PreparedRead 借用实际 receiver，在 callback 前按只读角色准备
全部依赖，继续使用同一完成流程；普通 WouldBlock 不触发重试。

错误变异 `6878e1eeb5` 丢掉 Ext4 读取压力；既有 staging 旅程在两个启用 Ext4 的
AX profile 中都未发生真实设备刷新，稳定失败。恢复后同一 Metadata/Lookup/Xattr
旅程通过，并验证压力阶段不持 state/namespace gate、admission 仍活动，以及下一轮
读取成功。原 admission 图旅程另验证只读挂载、零写租约、exact mount 和完整退出后
重试。增强夹具曾因保留返回路径而使最终释放断言失败，`b3e4668cce` 在该断言前
显式结束返回路径；这不是产品红证据，也没有放宽原断言。

最终源码 `b3e4668cce` 的五个 AX profile、VFS 71 项及集成、文档通过，标准库仍为
18 包中 17 过，唯一失败是 Kernel 的 5 个旧 Overlay 方法。9 项 VFS/AX Clippy、
最后 3 项 VFS Clippy、x86_64 fs-basic 和 Axvisor AArch64 125 项通过。Kernel
Clippy 首项因同样 5 个方法失败，其余 71 项未执行。普通 Overlay owner、目录批次、
统一 lookup 和完整 ABI 验收仍待继续；证据为 `typed_backing_read_completion`。


### 9.15 分层目录观察

`a0f4eba6ab` 将 Overlay 普通 metadata、单组件 lookup 与目录读取接入 PreparedRead
及同一 LayerRead；嵌套读取沿原 receiver-bound 请求下沉。reader gate 内消费原生
自有批次，所有 gate/admission 结束后才向普通目录 sink 交付名称，删除旧回调中
lookup 的重入路径。索引使用已读 metadata，胜出及落败候选进入共同 retire。
命名与元数据 writer 的共同边界尚未完成，不能据此宣称 Overlay 并发实现完成。

DirectoryBatch 每批最多 128 条。MemoryFs 使用既有读取，FAT 共用实际 locked
读取并保留临时引用，Ext4 使用原游标和 DirectoryReader，保留实际 staging/journal
progress。既有 MemoryFs 挂载旅程覆盖跨批完整名称、无重复和 exact mount；既有
Ext4 staging 旅程同时检查目录 Retry 与后续原生快照，没有新增测试入口或启动。
批次不代表原生内部扫描及 Overlay 合并名称表具有固定空间或时间成本。

同一真实 FAT 旅程在 `a0f4eba6ab` 的三个 FAT profile 均因 guard 退休引用而返回
Unchanged，预期 Applied。`7e78506f56` 对确知的 guard 克隆登记同 FS gate 计数，
只从 unlink 实际 strong count 中排除这些引用及本次 Weak upgrade；退休先归还
计数、退出 gate，再释放实际 inode。修复后相同旅程全部通过，保留真实活跃句柄
拒绝、内容、名称身份、取消及最后删除。更高层任意包装器仍不被一律视为私有引用，
FAT 活跃脱链和暂存能力未放开。新增计数沿用已有 FsRef/Send/Sync 的同 owner 证明，
仍纳入最终独立审查。

### 9.16 目录游标循环

`ce961c3b56` 在既有提供者旅程中把非法游标改为 1、2、1；原校验只比较相邻值，
VFS 为 70 过 1 败。`87ff1178b7` 拒绝回到输入游标或同批任何已有完整游标；
Overlay 另记录跨批 offset/continuation/change-attribute，拒绝循环而不假定 cookie
整数递增。最终 VFS 71 项及集成、文档通过；跨批 Overlay 的实际运行仍待 Kernel
旧方法迁移后验收。

最终源码 `87ff1178b7` 标准库选中 18 包、通过 17 包，唯一失败是 Kernel 的 5 个
旧 Overlay 方法。原生源码 `7e78506f56` 的 VFS/AX 9 项 Clippy、x86_64 fs-basic
与 Axvisor AArch64 125 项通过；最终 VFS 3 项 Clippy 通过，Kernel Clippy 首项仍
因同样 5 个方法失败，其余 71 项未执行。两次 QEMU 后仅改游标契约与校验，没有
再次修改所覆盖的原生 FAT 实现；不把这些运行记为实际 Starry Overlay 证明。

游标红轮另外出现一次 AX empty/EOF 测试的全局页计数 2 对 0。相对前一源码只有
VFS cfg(test) 夹具变化，分配来源尚未取得确定证据；最终同入口没有复现，但这项
观察没有被写成已修复。精确版本、全部日志及其局限见 `owned_directory_observation`。
后续仍先完成五个旧命名操作及 writer，再继续第 4、5 节和完整 ABI 验收。

### 9.17 原生匿名暂存

源码 `2b8e4b20f1` 的 `SealedStaging::detach` 从实际 observed-unlink 核对新建目标，
在同一 Context 中脱链并保留原生孤儿。seal 在 gate/admission 外准备私有父目录，
这一模板只用于同一工厂 detach 的未应用重试；普通 PreparedUnlink 仍不可 Clone。
`StagingNode::copy_xattrs` 在恢复所有者之前复制原生 source 提供的属性。既有暂存
旅程移到 `fs/ax-fs-ng/tests/staging.rs`，独占页提供者，保留内容、时间、属主、清理、
拒绝与写准备寿命，并覆盖孤儿缓存读写、零链接和安装/脱链的替代对象拒绝。

错误变异 `fedd310618` 只移除 detach 身份核对，五个 AX profile 均在同一断言稳定
返回 Applied/Ok，预期 Unchanged/BadState；错误变异未推送。正确实现的五个
profile 全部通过，VFS/AX 9 项 Clippy 通过；标准库仍是 18 包中 17 过，Kernel
五个旧方法失败。早期私有方法可见性、非 vfs 配置调用 vfs-only 方法及切片传参
错误是夹具编译问题，没有记作行为红证据。精确版本与日志见生产证据
`anonymous_staging_and_xattrs`，仍缺 Ext4 工厂及实际 Overlay 的运行证明。

### 9.18 元数据复制连接

`45b68934f7` 将 Overlay 两种节点的 `change_metadata` 接入实例 owner gate、
当前 inode 的固定 authorizer 和保留意图的下沉。缺少 upper 时按当前别名发现祖先，
以真实逻辑身份登记 `CopyPreparation`；准备阶段创建私有 `0700` 工作目录并清除
继承 SGID，再执行实际缓存复制、xattr、恢复及 seal。下一轮重新授权，在隐藏目标
应用意图后安装祖先或脱链匿名对象；已知 Applied 更新真实 backing，后续错误
保留效果并禁止整轮 Retry。最终属性从实际 owner 读取，显式清理退出全部 Scope 后
运行，并核对实际对象身份、保存第一项错误及报告残留。

本提交的标准库入口选中 18 包，17 包通过，五个 AX 暂存 profile 再次通过；VFS
3 项 Clippy 通过。Kernel Clippy 首项只报告原五个 E0407，其余 71 项未执行，
不能据此宣称整个 Kernel 或新 Overlay 路径已经验证。命名 writer、打开和普通数据
路径仍待接入共同边界，嵌套复制源在隐藏创建之前拒绝，完整挂载陷阱、工作目录
碰撞/恢复及竞态、部分安装和直接 syscall 证明尚未完成。

同时读取 `018e350d65` 的 CI run `43930453445` 失败日志，编译失败仍来自同五个
方法。AArch64 `task-stack-guard-page` 另有一次页异常缺少 guard 诊断而失败；当前
工具链下本地同一原用例为 1/1 通过，显示正确 guard 范围，未改源码或成功正则。
原因尚未确认，这次本地通过没有记为修复。精确源码、命令、日志摘要及限制见
`overlay_metadata_copy_up`，后续仍需处理全部旧方法与完整目标验收。

### 9.19 合并命名空间与系统调用运行

`4020af975f` 用 `NamespaceAttempt` 替换五个旧命名方法，create、symlink、link、
unlink、rename 经实例 gate 和固定 authorizer 下沉；`EntityPreparation` 合并复制
与同命令删除引用准备。该源码的 Kernel 72 项 Clippy 全部通过；`d4dde74ca5` 修正
测试中已删除的影子 ID 引用后，18 个标准库包全部通过，实际身份断言仍保留。
这些检查解决了旧构建阻塞，后续仍以直接系统旅程核对行为。

Linux 原生运行否决了第 9.18 节当时采用的普通已删除 lower 匿名安装假设。固定
v7.1 `copy_up.c` 的普通别名路径仍安装原父目录中的名称；`index=off` 时 whiteout
导致 EEXIST。相同 C 输入在宿主 Linux 6.8 中得到 fchmod=-1/errno=17、nlink=1、
mode=0600。`c8bd7acd09` 据此修正预期，内容、旧名不重建和清理判定均保留；
原生工厂 detach 能力没有删除，也没有被当作普通 Overlay 的兼容证明。

真实 Starry 启动先在 `/dev/log` 的旧 O_CREAT 路径被 EPERM 阻塞。修正预建 inode
绑定后，同一 ownership 旅程稳定得到 73 pass、6 fail，暴露静态设备 existing-open、
pathname socket 的创建身份/拒绝状态和已删除 lower 的错误。首次修复后的 83 pass、
3 fail 又揭示 fsuid 切换未清除 FS 特权，以及合并链接被当作普通文件读取。
这些是实际客户机失败；磁盘 ENOSPC、网络隔离准备失败和夹具编译错误均另行记录。

生产源码 `0ade408bf75c272c0dfec4a071532cf404819fcd` 原子保存 bind 的 FS/MNT、
凭据与 umask，创建实际 inode 后直接绑定其 BindSlot；只在当前线程发布 fsuid/fsgid，
并按 CAP_FS_SET 规则降权与恢复。`SimpleDir` 的 existing-open 走固定 authorizer，
缺失目标仍拒绝修改。`ReadLink/LinkText` 连接原生与嵌套读取完成，Ext4 保留真实
staging Retry、压力和错误；Overlay 在源准备成功后拒绝普通已脱链 lower 的首次复制。

该源码的 18 包标准库及 VFS/AX/ax-net/Kernel 共 90 项 Clippy 全部通过。
x86_64、AArch64 的 `cargo xtask starry test qemu --arch <arch> -c
qemu/system/test-vfs-ownership` 均为 92 pass、0 fail，最外层 1/1 通过；回归使用
管道与 pthread_join，在调用者恢复身份前观察真实兄弟线程，未增加独立启动。
Linux 的 Overlay 与线程/socket 对照分别为 35/35、17/17 通过。其余架构和后续
错误候选的结果见生产证据 `overlay_namespace_runtime`；运行中的任务不记作通过。

本地测试使用任务私有的用户/挂载命名空间，镜像与其他架构 release 的新输出放在
RAM 层，并复用原路径下的构建缓存；宿主挂载和共享缓存保持原状。普通 Overlay
open/data writer、FAT 合并观察 holder、嵌套 xattr、工作目录恢复、安装竞态与部分
失败仍待完成。统一查找、proc/SimpleFs 完整所有权、F1–F8、完整系统调用表、性能、
最终独立审查和精确 head 的完整 CI 继续按原计划推进，PR 保持草稿。

### 9.20 隔离判定与统计入口

错误候选 `253a6c3209dbbf140bf70224e9944f689249d024` 只绕过 command nonce，
并把当前线程的凭据发布替换为真实进程发布；其余 backing 角色、mount 和原生
检查仍保留。VFS 同一完成旅程稳定为 70 pass、1 fail，外部 command 未被拒绝，
标准库最外层退出 1。线程错误源码 `184df01a5a` 在客户机为 91 pass、1 fail，
唯一失败是管道控制的兄弟线程身份观察；该错误修改没有推送。

LoongArch 不提供 fstat/newfstatat，最初系统用例在 C 编译阶段失败，没有进入
客户机。测试改用四架构共有的直接 statx 及 libc 自带声明，保持 uid/gid/mode/nlink
判定一致；缺少 linux/stat.h 的夹具错误也没有计为行为红证据。正确测试源码
`0f2a1a3dd23e559c5c51091c5bc77add7d846bc1` 的 Rust 文件与 `0ade408bf7` 逐字相同。
恢复后 18 包标准库再次全部通过，statx 版本的 x86_64 为 92 pass、0 fail，最外层
1/1 通过；其余架构的最终状态与原始日志摘要由生产证据记录。源码仍包含最新
`dev` 的 `1b3ad93187`，原工作树 stash 保留。

`0f2a1a3dd2` 的 statx 版本随后在 x86_64、AArch64、RISC-V、LoongArch 均取得
所选 ownership 子用例的 1/1 成功终态。前三个架构均输出 92 pass、0 fail；
LoongArch 沿用 --capture-failures，成功时不回放程序 stdout，日志确认该程序的
BEGIN/PASSED、total=1 passed=1 failed=0 和最外层退出 0，未据此宣称日志打印了
内部 92 项计数。所有错误候选均已离开生产分支，Rust 字节一致性已核对；精确日志、
摘要与支持边界保存在生产证据，最终 head 的完整 CI 仍需继续完成。

### 9.21 普通写打开与原生缓存

`c4b08ac1f3` 让既存普通写打开借用描述原来的 `PreparedWrite`，通过
`FileOpenRequest` 在真实 owner gate 中每轮重新观察和授权。Overlay 的
`OpenAttempt` 复用 `CopyForest` 准备内容和祖先，结束本轮 Scope 后准备，下一轮
重新授权后安装，最后由 AX 发布描述。原实现已能返回 writable fd，但 upper
尚不存在；增强后的同一 ownership 旅程在该断言稳定失败，最外层退出 1。

`27144a3027` 的 `LayeredFile` 保存接受的原生端点和 canonical cache。普通写描述
随后直接调用该 cache 或 direct backend，逻辑 fd 身份仍由原始 PathHandle 保留。
`WriteAccess::bind_backing` 在原图内绑定中间层和原生 inode 访问租约，原
`Arc<WriteAccess>` 随映射保留它们；关闭 fd 后仍阻止 backing 执行和 merged
卸载。只读截断在发布前释放临时 writer。mmap、CoW、loop 通过 `cached_file`
和 `data_path` 取得已经选定的实际端点。

`aa14e2ca77` 增加确定性的重新授权证明：复制准备阶段由另一个线程撤销真实
owner metadata 的写权限，下一轮必须在发布前拒绝。错误候选
`da02a30e8cd63b45988d8743221900257d4f487f` 分别绕过该重新授权和原生端点选择，
VFS 旅程得到错误的 `Ok(())`，原生 I/O 旅程得到 0 字节写入而非 8 字节，标准库
最外层因这两个包失败退出 1，其余 16 包通过。恢复后 18 包全部通过，VFS 新旅程
进入实际测试入口，全部 72 项通过；受影响三包的 81 项 Clippy 也全部通过。

四架构通过同一 ownership 子用例，x86_64、AArch64、RISC-V 均打印 103 pass、
0 fail，LoongArch 的 BEGIN/PASSED、total=1 passed=1 failed=0 与最外层退出 0
确认实际执行。新增判定覆盖首次写打开、拒绝不安装 upper、跨页写入、共享映射、
关闭 fd 后继续映射写入、mount 使用权及 lower 内容不变。宿主 Linux 对应 Overlay
旅程为 46 pass、0 fail。x86_64 内核来自 `27144a3027`，后续 Rust 差异只在
`file_open.rs` 末尾追加 cfg(test) 模块，生产前缀与其余 Rust 文件一致；其他架构
使用 `aa14e2ca77`。错误候选未进入生产分支，完整 PR 范围和草稿状态保持不变。

`13b4214ab2` 随后在同一旅程补充私有映射先读原生缓存、写时复制、与共享写入
隔离和显式退休，覆盖 CoW 消费者的实际接入。该版本的 Rust 与下一节 getcwd
修复逐字相同；x86_64 为 107 pass、0 fail、最外层 1/1，Linux Overlay 为
50 pass、0 fail。其他架构已有上述 103 项版本的实际通过，新增私有映射判定
本地只在 x86_64 验证，后续以新 head CI 核实。

### 9.22 命名空间复制与已删除目录

前一 head 的完整 CI 暴露 `test-unshare-fs` 错误期待已删除 cwd 使 unshare 返回
ENOENT。同一原始程序在宿主 Linux 也于该断言失败。固定 Linux v7.1 的
[`ksys_unshare`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/fork.c#L3001-L3125)
保留 fs_struct 的实际路径，准备后再提交；
[`unshare_nsproxy_namespaces`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/nsproxy.c#L197-L226)
根据实际 user namespace 检查有效 CAP_SYS_ADMIN，没有该权限时返回 EPERM。

`e6bd54b7f1` 增强同一系统旅程：先保留 permitted 集并撤销有效 CAP_SYS_ADMIN，
以真实 EPERM 验证 fd 表、FS 上下文、UTS 和 mount namespace 均未发布；再恢复
权限，验证已删除 cwd 允许成功复制，调用者切换 cwd、关闭 fd 后，真实兄弟线程
仍持有原 fd 表和已删除 cwd。条件变量控制观察顺序，清理关闭兄弟线程残留 fd。
宿主 Linux 6.8.0-149-generic 的完整程序通过，没有增加独立用例或默认启动。

这一增强在 Starry x86_64 暴露实际 getcwd 错误：unshare 已正确成功，但路径生成
使用保留的旧名字，未返回 ENOENT。`df0536aae1` 在 `sys_getcwd` 调用
`path_from` 前检查实际 cwd 的 `NameState::Unlinked`，包含 cwd 恰好等于进程 root
的情形。该边界与固定 Linux 的
[`getcwd`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/fs/d_path.c#L389-L423)
一致；描述与 proc 显示名仍由其原有接口处理。

修复前同一 x86_64 子用例在 nameless cwd 断言失败，最外层为 0/1、退出 1；
修复后 x86_64、LoongArch 均为 1/1、退出 0，完整程序继续验证 pending PID
namespace、clone(CLONE_FS) 隔离及无效 flags。Kernel 增量标准库选中并通过一包，
四目标共 72 项 Clippy 全部通过；VFS 与 AX Rust 源码保持上一节通过时的内容。
其他 getcwd 并发路径、完整 syscall 表及最终 head 完整 CI 仍按原计划核实。

### 9.23 组件查询与合作重查

`DirNode::lookup_component` 返回 `LookupStep`，将 Found、通过负观察的 Missing、
Revalidate 和真实 Failed 分开。缓存 generation 或 provider 版本失效属于候选发布
冲突；目录 namespace generation、writer 及 `NameObservation` 失效要求父命名
重验证，并优先保留较强分类。普通无 identity 的动态结果也必须经过接受检查。
后端 WouldBlock 始终保留为 Failed，不由错误码猜测是否重试。固定 backing 的
单次查询保留原边界，不采集可能与外层 publication 冲突的全局命名观察。

`Location::lookup_component` 保留分类及覆盖挂载转换；普通 FsContext helper
在候选退休后重新检查原目录搜索权限，只在接受结果后追加搜索记录。
`BlockRuntimeOps::yield_now` 是必需接口，原生适配先拒绝 atomic context，再检查
当前线程并保留调度失败；宿主适配执行实际 thread yield，writeback 和 prefetch
的测试实现同步迁移。`with_fs` 使用冻结 binding，后端调用及让出发生在任务状态
短锁之外。原生启动顺序已核实为调度器建立、解除启动期抢占限制、安装 FS runtime。

错误候选 `988ff8a97f` 关闭发布校验，同一受控交错使 VFS 四项回归失败，
标准库最外层退出 1，其余 17 包通过。恢复后 `e828831525` 的 18 包全部通过。
`560c8565f4` 的 VFS、AX、ax-block、runtime 共 43 项 Clippy 通过；后续缓存改动
在最终源码上补跑 AX 全部 6 项检查并通过。证据保留各自源码和覆盖边界。

本阶段尚未构造 LookupSession/Continuation，没有完成 scoped 父路径约束重验证、
总链接预算、真正非阻塞的 CACHED 或末组件副作用接入。现有 open 的父路径和
末链接仍分开解析，不能将本节视为完整查找迁移。

### 9.24 退休准备与验证资源

命名操作原先复用压力回收的尽力扫描，索引锁繁忙时会跳过已关闭缓存。
`prune_retiring_with_contention` 现在先在后端 gate 外取得 inode index 排他权，
再扫描 registry，冻结 Weak 升级并按当前 owner、持久化及脏页状态选择缓存。
临时容器在锁外预留，registry 增长时先退出两把锁再扩容；移出的 owner 交给
原命名命令的 MutationRetire，在外层保护结束后析构。普通压力回收保留非阻塞
try-lock，不借用命名操作的可等待策略。

原缓存所有权旅程增加受控索引争用：确认 try-lock 已失败后，另一线程才释放
索引。错误候选 `2c7e6170d1` 在该分支直接返回，稳定因已关闭缓存仍被持有而失败，
标准库最外层退出 1；正确实现及既有 Ext4 两次 unlink 回收断言通过。该旅程
继续检查重新打开、脏页与外层退休寿命，没有放宽原判定。

远端 `5e04fc35fb` 的 std 失败还暴露共享页面提供者的计数污染。临时分配栈确认
写打开旅程后半段及 FAT 生命周期旅程在独占夹具之外分配页面。现在两个完整
旅程都拥有 `with_test_page_provider` 范围，EOF 零分配断言保持原值。使用路径
查找的宿主集成夹具显式安装 runtime adapter。临时诊断及错误候选未进入生产
分支；最终标准库检查包含 AX 全部五个 feature profile。

`52fc53e925` 的 x86_64 ownership 程序为 107 pass、0 fail，最外层 1/1、退出 0。
此后 Rust 差异仅为测试夹具及模块说明，实际内核仍按 `52fc53e925` 记录。
`5e04fc35fb` 的 PR 检查汇总已终态：37 项成功、6 项失败、1 项取消及 5 项跳过；
失败包含 std、四架构 Starry suite 及 Axvisor AArch64 的 Arm 下载 HTTP 503。
后续 head 仍须逐项取得要求检查的终态，完整系统调用表及其余计划保持原范围。


### 9.25 普通路径会话与末组件

`88bed9cdf7` 将普通 `resolve`、`resolve_no_follow` 与文件打开接入同一个不可复制的
`LookupSession/Continuation`。会话保留冻结 root/cwd、当前及已接受的 `PathHandle`、
未消费组件、原调用凭据和私有总链接预算。父路径与末链接共用 40 次上限，链接目标
的尾斜杠和原后缀继续由同一会话处理。末组件创建交给真实目录的打开命令，不能把
查询缺失当作创建授权；普通最终名称带尾斜杠且请求创建时按 Linux 返回 EISDIR。

`LinkTarget::Object` 以及 proc 的对象快照持有实际 `PathHandle`。exe、cwd/root、
fd 和 namespace 目标在原所有者仍存活时取得使用权，目标消费不再先降级为裸
`Location`。这还没有实现保存的 `LinkCaller`：proc 当前的授权调用身份仍待统一迁移。

同一公开 AX 集成测试在错误源码 `b9d63887d0` 的五个 profile 全部失败，旧实现把
40 个父链接再加一个末链接错误接受为 `Ok(3)`，预期 ELOOP。该候选仅在原生产源码
上增加与正确提交逐字相同的测试文件。恢复 `88bed9cdf7` 后，18 包 std、VFS/AX
9 项和 Kernel 四目标 72 项 Clippy 均通过；错误候选没有推送到生产分支。

x86_64 真实内核的 ownership 程序保持 107 pass、0 fail、最外层 1/1 通过。
完整 path-lookup 程序的链接预算和创建尾斜杠判定通过，但仍有 17 项 scoped
openat2 断言以 EOPNOTSUPP 失败，最外层退出 1。同一完整 C 程序在 Linux
6.8.0-149-generic 通过；该运行不能替代固定 Linux v7.1 源码的语义核对。
两次 QEMU 内核 SHA-256 都是
`2696de51c463244d194cbb9526827f31327c01745caa9d442d4f370a42905b97`。

checked/trace/parent 旧循环、scoped 根与挂载约束、父命名及拓扑重验证、保存的
调用身份和真正非阻塞的 CACHED 仍在原交付范围。远端旧 head `e22f9f1da7` 的
完整 CI 为 38 成功、6 失败、5 跳过且无运行中项目，不能记作本节源码的 CI。


### 9.26 Arm 客户机准备资源

远端 Axvisor AArch64 在 `vcpu_runtime` 的 host_prepare 阶段连续遇到 Arm 原下载
地址 HTTP 503。`b3774ce7bc` 保留 GNU Arm 10.3-2021.07，改从 Arm 官方存储
端点下载同一归档，固定 SHA-256 并在校验后发布缓存；原 CLI 与 CROSS_COMPILE
覆盖仍有效。归档 MD5 与官方相邻清单相同，准备脚本和 README 同步说明校验。

独占冷缓存的项目 ktest 入口实际完成 venv、固定 Zephyr 检出、工具链下载、
客户机编译和 Axvisor QEMU 运行。Fair 与 FIFO 两个策略各确认 300 次定向投递，
非目标 vCPU 返回为 0；AXTEST 为 1 pass、0 fail、0 skip，最外层退出 0。
该修复随后在 `ab86e4910c` 的远端 AArch64 任务通过，日志确认实际下载及
vcpu_runtime 运行。NUC 本轮也通过；此前启动失败的原因没有确认，不能归因于本项修复。


### 9.27 软浮点依赖与内核夹具

固定 rustc 的 [x86 裸机目标](https://github.com/rust-lang/rust/blob/8eb778d04094d3af4b26580cab327e0413eb1e02/compiler/rustc_target/src/spec/targets/x86_64_unknown_none.rs)
与仓库 JSON 逐字段相同，保留 soft-float ABI 和禁用 SIMD 的契约。
[Rust #160302](https://github.com/rust-lang/rust/pull/160302) 对软浮点目标显式开启
SSE/AVX 的接口产生 future-incompatibility 警告；这也包含只声明而未使用的函数。
`cpufeatures` 0.3.1 在 freestanding 目标上已返回无 SIMD 支持，旧 RNG 实际产物
也走标量路径，不能据警告宣称已经复现用户 FP 状态破坏。

`dfe068a933` 在共享 `CargoBuildTarget` 中为 x86 裸机选择
`chacha20_backend="soft"`，构建与 Clippy 使用同一参数。真实依赖编译指纹确认
该 cfg 已传给 ChaCha，新的报告不再包含 chacha20；core、memchr、bitmaps 的
无条件 intrinsic 声明警告仍有记录，没有添加 allow 或开启内核 SIMD。
实际 RNG fill_bytes 的反汇编没有 xmm、ymm 或 zmm 寄存器操作。

首次 Starry 内核 axtest 构建因旧夹具的五处错误退出，尚未执行测试。
`d58de43286` 迁移 mount 创建凭据、proc 冻结上下文借用，并把两个误用普通
`#[test]` 的 namespace 身份用例注册到 axtest。修复后的 x86_64 完整内核测试
为 263 pass、0 fail、0 skip，最外层退出 0；随机设备熵混合、两个 namespace
身份和 mountinfo 判定都有独立的实际通过标记。

axbuild 标准库一包与其 Clippy 一项通过。首次 Kernel Clippy 日志逐项打印 72 项
通过，但外层进程返回 143，未作为最终验收；随后发现共享参数只作用于当前 crate，
需按下一节修正并重新执行。远端 `ab86e4910c` 的完整 CI 为 40 成功、4 失败、
5 跳过，失败均为 Starry suite，尚未达到最终交付条件。


### 9.28 静态检查参数的依赖边界

原 Clippy 适配把 `CargoBuildTarget.rustflags` 放在 `--` 后，目标策略只作用于当前
crate，依赖仍按另一套参数编译。`33df20205c` 复用已有 Cargo rustflags 来源合并，
把目标策略传到依赖；每项检查专用的 source cfg 与 warnings 门禁继续留在 `--` 后。
这也使 AArch64 的复制代价参数在构建和 Clippy 中具有相同的依赖作用域。

同一协议回归在原适配器 `bfa4e20a34` 上为 510 pass、1 fail，因目标策略没有进入
依赖编译作用域而失败，最外层退出 1。恢复正确适配后 axbuild 标准库一包通过，
最外层退出 0；两次使用逐字相同的测试文件。实际旧依赖编译指纹也缺少标量 cfg。
最终 `33df20205c` 的 axbuild/Kernel 共 73 项 Clippy 全部通过，独立退出状态文件
确认最外层为 0。实际 x86 debug 依赖指纹从没有标量 cfg 变为包含该 cfg，
当前编译报告不再含 chacha20。标准库回归、实际依赖参数和最外层终态分别记录，
没有把先前退出 143 的日志作为本次验收。


### 9.29 保存的调用者与路径视图

`934d5759b9` 把链接解析身份接到 `FsView` 和 `LookupSession`。`FsView` 持有
冻结的 `FsContextSnapshot`，借用外层保存的 `LinkCaller`，并只从该调用者派生
`MutationCredentials`。它保留实际 root/cwd/dirfd 使用权，不提供任务状态更新出口。
原 `FsContext` 只保留路径选择，打开与路径变更入口不再另收一份凭据。

普通、checked、trace 及普通 parent 入口共用 `LookupSession`；旧的文本链接解析
循环已删除。会话把同一调用者传给 `LinkOps::target`，直接消费 `Text` 或 `Object`，
保留既有的 40 次预算。所有 `LinkOps` 实现、DirEntry/Location 显示入口及原有调用点
已迁移为显式身份参数。启动、归档、迁移和 Axvisor 使用明确的 native 身份；
backing 复制继续使用保存的 mounter 投影，native 身份不能代替 proc 授权。

Starry 的 `ProcLinkCaller` 保存原 `FsCredentialsSnapshot` 与 ProcessData 分配身份，
`ProcLink` 回调不再抓取当前任务。目标的凭据、dumpable 与 MM owner 仍在每次授权时
读取。`FsCredentialsSnapshot::for_real_id_access` 保留配对的 user namespace Arc，
使 access 的 DAC 与 proc READ_FSCREDS 使用同一投影；pidfd 的 READ_REALCREDS 保持独立。
exec 的脚本解释器及 PT_INTERP 查找也沿用入口保存的调用者。

现有内核授权旅程增加了受控凭据变更：旧操作使用旧身份，新操作使用新身份，
real-ID 操作使用派生身份。私有错误候选 `f8632295f8` 在派生能力后保留错误的
fsuid/fsgid，真实 x86_64 QEMU 在同一旅程的 real-ID 授权断言失败，最外层退出 1。
恢复投影后，完整内核测试为 263 pass、0 fail、0 skip，独立状态文件确认退出 0。
两次测试源码 SHA-256 相同；正向 ELF、负向 Git tree 与日志均已保留。

`d75084e2ce` 的 VFS/AX 9 项 Clippy 及 18 个受影响软件包的标准库测试通过。
`5e0d063b0b` 补齐设备创建夹具的能力视图；标准库测试使用已准备的 e2fsprogs
1.47.3，避免系统旧工具不识别既有格式化选项。`8221696e4c` 修正借用表达，
让无需链接解析的 cgroup inode 控制继续使用直接凭据快照；该源码的 Kernel/runtime
100 项 Clippy 和 x86_64 ownership 程序均通过，后者为 107 pass、0 fail、外层 1/1。
通用 Clippy 明确跳过 Axvisor，其专用命令也没有 Clippy 子命令；专用配置检查另见
第 9.30 节，没有将工具跳过记为通过。

本轮完整路径查找 C 用例有 28 项断言通过，17 项 openat2 约束仍以 EOPNOTSUPP
失败，最外层退出 1。scoped 规则、真正非阻塞的 CACHED、特殊末组件、仍返回
Location 的出口、跨操作数的原始视图、完整 proc/SimpleFs 对象及其余原交付范围
继续保留。远端 `2e5a149b8d` 的 CI 已全部结束：35 成功、5 失败、4 取消、5 跳过。
新增 ArceOS AArch64 失败缺少栈保护页诊断行；与此前成功日志的故障地址、PC 和
栈指针相同，原因仍需验证。当前拉取请求保持 draft，尚未满足最终交付条件。


### 9.30 对象打开与实际消费者

`eb1a9befc8` 删除按 `/proc/.../ns/...` 文字截获打开的 `try_open_nsfd`。
namespace 链接沿 `LookupSession` 取得实际对象，再由已有 `add_to_fd` 分派为 NsFd；
`O_PATH | O_NOFOLLOW` 先保留链接本身。原截获在真实 proc-link-access 程序中把
`/proc/2/ns/mnt` 的 nofollow 结果错误变成 namespace fd，fstat 类型断言失败，
外层退出 1。删除后同一完整 C 程序外层 1/1 通过，未改测试或放宽断言。
该源码的四架构 Kernel 72 项 Clippy 全部通过。

`d75084e2ce` 补齐 native 调用者的公开导出及 Axvisor memory-root 夹具。
归档视图使用 `fs_view`，避免遮蔽内核地址转换的 `view()`。AArch64 memory-root
实际内核测试为 125 pass、0 fail、0 skip，Web UI diskless 用例外层 1/1 通过；
两者验证真实调用方已采用新的 `FsView`，没有用静态编译代替启动结果。

通用 Clippy 明确要求 Axvisor 专用目标，而 Axvisor 命令没有 Clippy action。
依据 `AGENTS.md` 第 2.1 节的特殊配置例外，检查任务实现后，复用上述实际 Web UI
构建日志中的目标 JSON、生成配置、feature、profile 和环境，仅切换为 Clippy，
同时检查 library 与 binary 并拒绝警告。`982c87b3ba` 和 `54d80a8531` 修复实际
发现的冗余借用、条件嵌套、立即 push，以及启动选择器迁移后无生产调用的旧包装。
`StartupSelector` 与 `selected_vm_ids` 删除，现有缺省选择测试直接验证生产使用的
`startup_selection`，同时检查 start/prepare；旧 getter 回读测试并入该旅程。
最终该专用配置 Clippy 外层退出 0，随后 AArch64 内核批次为 124 pass、0 fail、
0 skip，外层退出 0。它沿用原批次、一次启动，没有增加测试启动。

合并 `dev@93ad9be84f` 的 `70e2bb4fb4` 没有 Rust 或锁文件冲突；新增内容是容器
前置依赖，固定 rustc 和目标契约不变。容器脚本的两个单元测试通过。完整路径用例
仍有 17 项 scoped openat2 断言失败，非阻塞 CACHED、拥有型出口与第 4、5 节其余
计划继续实施。上述本地结果分别绑定实际源码；远端最终 head 的 CI 另行核实。


### 9.31 受限路径策略

`ca468249a1` 让普通、受限和打开继续使用同一 `LookupSession`，删除只支持
`BENEATH|NO_SYMLINKS` 的独立解析器。`LookupScope` 区分普通根、BENEATH 和
IN_ROOT；正交限制在真实组件、挂载转换与链接目标上执行。初始绝对路径按入口规则
选起点，绝对链接沿原会话跳转；对象链接先取得动态目标和授权结果，再检查
NO_MAGICLINKS、scoped 与 NO_XDEV，不解析其显示文字。

`MountObservation` 在 capture 与 validate 时 try-acquire 实际 topology gate，
再读取完成版本；仅比较写后版本会漏掉在途发布。`PathObservation` 组合已有名称
写者与版本，组件转交、向上遍历和最终祖先检查均使用原观察。创建把观察交给实际
`CreateIntent`，槽位尝试在同一 naming 短锁内验证旧观察并开始自身发布，再复核
mount。native 与 composite 使用相同命令边界，失效候选交给原退休载体。

本节源码的 VFS/AX 9 项、Kernel 四目标 72 项 Clippy 全部通过，18 包标准库通过。
原完整 path-lookup C 程序保持 SHA-256 不变，x86_64 从 28 pass、17 fail 变成
全部 45 项断言通过，xtask 1/1、外层 0；内核 SHA-256 为
`e1e59b42eef4244cb3475b53a44ff7b666fa01a87cf8f26ea928ce3692b17ec4`。
这同时保留总链接预算、rename 后父身份、挂载穿越及 nofollow 的原判定。

新增组件旅程受控执行真实 MemoryFs rename，在同一操作跟随 `..` 前移动父目录，
观察 EAGAIN；另把失效观察直接送入真实创建槽位，验证不创建 escaped 条目。
挂载旅程持有真实写 gate 时核对 capture/validate 拒绝，完成真实安装后旧观察仍失效。
私有错误候选 `6858290c64` 同时删除 mount gate 排除和槽位原观察检查，同一测试
在 AX 五个 profile 及 VFS 分别失败，标准库外层退出 1；其余 16 包通过。
该候选没有推送；恢复生产源码后，同一 18 包标准库再次全部通过，外层退出 0。

`openat2` 的 BENEATH/IN_ROOT 互斥与 CACHED 变更类 flag 校验按固定 Linux
`build_open_flags` 排序；普通 cache-only 请求仍未实现，明确不把这一分支计为
CACHED 完成。特殊末组件、拥有型结果、整体 proc/SimpleFs、其余第 4、5 节范围
继续保留。本次仅有 x86_64 scoped 系统运行证据，其他目标的静态检查不能代替运行。


### 9.32 进程自指链接与持有描述读取

`c5ae587338` 将 proc/self 的目录特例替换为普通文本链接，并提供 thread-self。
`ProcLinkCaller` 保存原线程 PidIdentity，SelfLink 配方只保留固定 PID view 和种类，
通过另一个线程持有的链接 fd 读取时使用本次读取者的编号。目标不可见时 ENOENT，
NO_MAGICLINKS 允许这类普通链接；完整 SimpleFs/raw 属性迁移仍另行实施。

既有 proc-link-access 程序并入自指链接旅程，保持一次默认启动。错误源码
`ca468249a1` 先因 self 仍是目录而失败，外层 1；`c5ae587338` 的类型修复通过后，
同一程序进一步发现 `readlinkat(fd, "")` 把链接 fd 当目录并返回 ENOTDIR，外层 1。
`ac8e8bdd61` 保留原打开描述完成空路径读取，并由 `FsView::read_link` 在普通
路径显示回调结束前保留实际路径使用权；空路径非链接和长度采用固定 Linux 规则。
最终 x86_64 的同一完整程序外层 1/1 通过，观察了主线程、兄弟线程的目标编号及
原有 allow → EACCES → allow、nofollow 类型和对象身份判定；三次程序摘要相同。

相同程序在 Linux 6.8.0-149-generic 的私有用户、PID 和挂载命名空间通过。直接用
宿主普通用户运行时，自指断言通过，但既有 nondumpable fd 目录的 nofollow 断言因
DAC 失败；成功对照使用与 Starry 用例相同的命名空间内 root，并显式移除
CAP_SYS_PTRACE。此环境区别已记录，没有把普通用户运行的失败隐藏为通过。
最终内核 SHA-256 为
`259792fae363004ff4e03a7f62fa3e1a0a96cad9423ade42cf043eec6dbfbcea`。

最终 `ac8e8bdd61` 的 AX/Kernel 78 项 Clippy 全部通过，外层 0；同一完整 proc
程序也在 AArch64 为 1/1 通过，内核 SHA-256 为
`5c51183fa2312ba064bf2d024dd869c82a7e9ee9bc3d32de09aaa06f5df99642`。
x86_64 的原完整路径程序再次为 45 pass、0 fail、外层 0，与本节 proc 程序使用
同一内核。中间 Clippy 因完整操作尚需修复而终止，没有作为通过。

[系统调用兼容性记录](vfs-syscall-compatibility.md) 已按本轮入口、flag 和目标记录
20 行六列对照；其余完整 PR 范围继续补齐。精确源码与独立状态文件记录在生产证据，
PR 仍是草稿，完整 CACHED、拥有型出口、其余 proc/SimpleFs、F1-F8 及原交付范围不变。


### 9.33 查找结果的使用权

旧 `finish_lookup_with_trace` 把已取得的 PathHandle 降为 Location，公共 resolve
返回时 guard 已释放。公开功能旅程 `resolved_paths_hold_the_selected_mount_until_the_consumer_retires`
保留真实 MemoryFs 挂载中的查找结果，再执行普通卸载；错误源码 `21eaf18bd9`
在 AX 五个 profile 都得到 `Ok(())`，预期 ResourceBusy，标准库外层 1。
测试随后释放结果并要求正常卸载成功，区分丢失使用权和永不释放的泄漏。

`802004f30c` 将普通结果、nofollow、checked、trace、parent 及搜索权限边界改为
拥有型 PathHandle，命名创建结果复用父路径使用权。目录迭代直接转交路径；
Starry 的 ResolveAtResult、元数据/xattr、cwd/root、exec、挂载与匿名对象消费者
保留原句柄，`.location()` 只作为拥有者仍存活时的后端借用。普通卸载消费目标，
转入既有 PathUnmount，避免把自身租约当 busy，也不释放后重新取得。

Overlay lower/upper/work 的输入及文件系统寿命保留实际 backing 路径，内部原生
关系和 inode 索引先于这些租约退休。Axvisor 的资源校验与 Web UI 消费者读取拥有型
结果。`272141b52a` 迁移公开创建夹具的借用表达；同一回归文件 SHA-256 保持不变，
最终 AX 五个 profile 全部通过，完整标准库 18 包、AX/runtime/Kernel 的 106 项
Clippy 全部通过，外层均为 0。

x86_64 的 ownership 程序为 107 pass、0 fail，路径程序为 45 pass、0 fail，
完整 proc-link-access 为 1/1；三次定向 xtask 的外层均为 0。实现源码
`802004f30c` 与夹具提交 `272141b52a` 生成的实际内核相同，SHA-256 为
`788f7d79e76ea3086a9218ec398798a7d92ecf1fd555e058f462faf80b0c499a`。
Axvisor 同一专用配置 Clippy 再次通过，AArch64 内核批次 124 pass、0 fail、
0 skip，Web UI diskless 为 1/1，全部外层 0。

随后以同一构建的 ELF 运行完整 x86_64 system，541 个程序中 534 通过、7 失败，
xtask 与外层均为 1。失败为 dir-mutation-permissions、proc-pid-exe-readlink、
cgroup-basic、ltp-pilot-min、cgroup-ns、devpts-newinstance、unix-bind-chown。
已观察到特殊末组件错误顺序、执行文件退出后卸载仍 busy，以及 cgroup/devpts
实例语义缺口；完整批次不记为通过。AArch64 GICv2 SMP4 的原 stack-guard 用例
本地 1/1 通过并出现要求的诊断，但未复现旧 CI 的诊断丢失，未声称已修复。

完整 CACHED、特殊末组件、跨操作数同一原始视图、其余 proc/SimpleFs 及原交付
范围继续处理。生产证据的 `owned_lookup_results` 保留精确源码、各层终态和摘要；
系统调用表新增拥有型根、挂载与执行消费者，不由类型迁移推断完整兼容。


### 9.34 类型化父解析

`190c9a2d5f` 以 `ParentLookup` 保留原父目录使用权、FinalComponent、尾斜杠、
搜索记录及 dirfd 边界，删除物理父名称重建与旧 tuple 出口。父解析在末尾点和
点点之前停止，普通 lookup/open 在同一 Continuation 中消费它们；命名操作按
Linux 的 create、unlink、rmdir 和 rename 规则分类。删除操作不因尾斜杠跟随
最终链接。公开创建、initramfs、原生 rename_at 和 Kernel 全部迁移到新出口。

Kernel 的 linkat 和 renameat2 入口只保存一次 FsView，由 `view_at` 选各自 dirfd，
两端解析与提交使用原 root/cwd/namespace/caller。`df88f5e52a` 与 `cf78f01c61`
补齐 String 导入和 dirfd 选择入口的重新导出；失败编译与中断轮次不计为通过。
当前源码已共用原 FS 视图，但跨线程改变 CLONE_FS 的直接受控证明仍未取得。

错误源码 `272141b52a` 的完整目录权限程序会把 dirfd 点目标重建成物理父名称，
在不可搜索的外层父目录返回 EACCES，预期 EEXIST。相同 C 文件保持摘要不变，
`cf78f01c61` 在 x86_64、AArch64、RISC-V 和 LoongArch64 均为 xtask 1/1，
最外层 0。该版 18 包标准库和 AX/Kernel 78 项 Clippy 通过；x86 的 ownership、
path 完整程序保持 107 pass、0 fail 和 45 pass、0 fail。

核对固定 Linux 的 do_renameat2 后，原完整改名旅程增加带目录尾斜杠的混合类型
交换拒绝及身份保留判据。错误候选 `9b3d54bbe2` 在两个 VFS profile 返回
NotADirectory，预期保留后端的 OperationNotSupported；标准库最外层 1。
`5111b45358` 将目录要求应用到实际交换操作数，同一文件摘要不变，最终 18 包
标准库与 AX 六项 Clippy 全部通过，最外层 0。Linux 6.8 的直接 renameat2 探针
确认这类输入合法并交换实际对象；MemoryFs 当前仍不提供 EXCHANGE/WHITEOUT，
没有用拒绝路径宣称完整交换兼容，也不把前版四架构 ELF 记成该修正的运行结果。

生产证据的 `typed_parent_lookup` 绑定各源码、四架构 ELF、C/Rust 文件与日志
摘要；系统调用表扩为 42 行。完整 CACHED、实际命名提交的剩余拓扑重验、
proc/SimpleFs、伪文件系统实例、执行缓存退休及原验收范围继续实施。


### 9.35 测试内核选择与临时目录

`a0a3b8b5fc` 修复 Starry 测试工具接受不完整 fixed-elf 请求的问题。发现模式原先
忽略该参数，仍构建当前源码；现在 fixed_elf 与 arch、target、test_case、list
互斥，并进入需要完整 build/config/rootfs 的外部运行模式。真实根 CLI 旅程在
`4537950076` 为 509 pass、1 fail，修后 510 pass、0 fail；同一无效真实命令从
错误退出 0 改为退出 2。保留完整外部模式的接受判据。

此前 owned-lookup 完整 system 实际重新构建未变化的 `272141b52a`，其 ELF 与
前一轮摘要相同，但 fixed-elf 并未强制复用。临时目录负例实际源码为
`5111b45358`，ELF 为 `4a7b0954ac8602be7c338ffc214d2cca73da76fdfe6df5a0b12ce27161d18a57`，
不再记为 cf78f01c61 的固定内核运行。工具已拒绝这种混用，证据中的字段与解释
同步纠正；这两次程序的实际结果保留。

`04d7860504` 用 `mount_temporary` 在启动装配 `/tmp`、`/dev/shm` 时通过显式
初始化权限将实际根设为 01777。普通 MemoryFs 的默认权限不变，运行期创建仍
经过原授权与 owner 提交。原 unix-bind-chown 完整 C 程序未修改，旧版因普通
用户无法写入 0755 的 /tmp 失败，新版 x86_64 为 1/1、最外层 0，并核对实际
socket 的类型及 uid/gid。Linux 6.8 在私有 01777 tmpfs 上同一程序 7 pass、
0 fail；其单身份映射不证明 Starry 的 root 到 1000 降权步骤。

该源码的两包标准库与 axbuild/Kernel 73 项 Clippy 全部通过，最外层 0；x86_64
完整 system 为 541 个程序中 536 通过、5 失败，最外层 1。尚存 executable
卸载、cgroup-basic、ltp-pilot-min、cgroup-ns 和 devpts-newinstance，继续分别核对
实现和测试。该临时目录修正还没有其余三架构的运行证据，不能据此声明完整批次
或所有目标通过。精确源码、修前修后日志和摘要保存于生产证据的
`temporary_directories_and_kernel_selection`。


### 9.36 执行映像与映射退休

原 executable 旅程增加存活时普通卸载必须 EBUSY 的判据，源码 `5082dc693f`
仍在子进程退出后失败。`8a9f6c485c` 将 ELF 缓存改为拥有型条目：锁只保护 LRU
查找与发布，文件读取、映射及最后引用销毁在锁外；普通卸载前退休目标
FilesystemId 的缓存。该版仍失败，静态检查轮次中止，不记通过。

临时诊断显示挂载在退出后还保留两份实际使用权，旧 MM 的后台回收晚于失败的
卸载。`271ba070e1` 让退出发布 zombie、以及 exec 退出 process publication 后，
同步无用户且无内核 pin 的旧 MM 内容退休。缓存失效与退出使用同一等待条件，
只由 Freed 证明完成；仍有用户或内核访问的 MM 继续异步持有资源，失败的 backend
保留修复所有者。ProcessData 兜底 Drop 不等待。临时日志代码已全部移除。

同一 C 文件在 x86_64、AArch64、RISC-V 和 LoongArch64 均为 xtask 1/1、最外层
0，覆盖存活 busy、退出后普通卸载、实际 proc executable 身份及 unlinked 对象。
相同程序在 Linux 6.8 的私有 PID、用户和挂载命名空间通过。该版一包标准库、
Kernel 四目标 72 项 Clippy 及 x86_64 真实内核测试均通过；各自精确汇总、ELF、
源码和日志摘要记录于 `executable_cache_and_mapping_retirement`。

完整 system 尚未在此版本重跑，不能把前版 536/541 的五项失败直接改写为四项。
另已在远端 b8a1ecdf0b 的 LoongArch64 完整批次观察到 readonly remount 的 EBUSY，
需在后续完整批次继续确认。ELF 头缓存的写后失效、同条件性能以及原独立审查与
验收范围继续保留。

### 9.37 文件系统实例与命名删除

`628a87d496` 先按固定 Linux 与实际 Linux 对照修正既有旅程：普通重复挂载共享
Filesystem device，不同 devpts 默认挂载独立；cgroup 根的点点指向实际挂载点父
目录，旧 openat2 的一律不支持断言改由既有完整路径旅程承担。该版标准库仅 VFS
身份断言失败，x86 完整 system 为 539/541。另一个保留的标准库准备失败使用了
宿主旧 mkfs，不能混入采用任务内 e2fsprogs 1.47.3 的有效结果。

`2d98a3119d` 加入跨 cgroup namespace 的实际 device 比较，错误实现稳定失败。
`fd998f7796` 用唯一层级 Filesystem 和经过 backend owner 验证的 MountRoot 保留
实际共享状态，namespace 只选择根目录；`42a4f79ba5` 删除 devpts 全局默认实例与
Legacy/NewInstance 选择层，每次 mount 创建独立 producer。前一候选把 cgroup
namespace 当成 superblock 的划分依据，经固定 Linux 的 cgroup_do_get_tree 核对
后弃用，没有进入分支。

首次 x86 完整批次已到 540/541，仅 cgroup 删除权限失败；静态检查同时发现移除
PTY 全局实例时误删了 SG2002 ION 的 OnceLock 导入，`ca2b9eefb5` 补回相同 feature
条件。诊断确认普通用户 rmdir 控制文件时过早 ENOTDIR。原权限程序加入通用删除
顺序后，在 `3dd3c3e138` 的普通 unlink 与 rmdir 上分别错误返回 EISDIR、ENOTDIR；
尾斜杠的两个判据已经通过。`156c1943e5` 把普通目标类型判定移入同次后端授权，
该版 18 包标准库及 81 项 Clippy 通过，LoongArch 完整批次为 536/538。

两个剩余失败分别是 readonly remount 与 LTP rmdir02。后者暴露移走外部预检后，
后端仍依赖无发布者的旧 DirNode mountpoint 字段；`283b85a46b` 改查当前 Location
的实际 namespace 挂载索引并删除旧字段及访问器。带临时日志的完整批次恢复 LTP
拒绝，只剩 remount，537/538；该诊断运行不作为干净生产源码的通过记录。

写入租约诊断定位到运行器的 `/tmp/starry-system-test-output.*`。Linux 用同一
运行器和未修改的完整 remount 程序也复现 EBUSY，证明该输出本身是活动写入者。
`77d42086c9` 改为匿名 memfd 捕获，保留失败输出、LTP 成功证据及非零失败传播；
创建 namespace 前重置内容和偏移。Linux 相同程序由失败变为通过，三个实际 exec
程序另外证明两个成功与一个预期失败的回放及汇总。最初 Linux 准备因锁定的继承
procfs 失败，重跑使用新的外层 PID/proc namespace，保留该准备失败的独立记录。

两处临时 Rust 日志文件都已按备份与 Git 字节核对恢复。干净 `77d42086c9` 的
18 包标准库、VFS/AX/Kernel 81 项 Clippy 和 LoongArch 完整 538/538 全部通过，
最外层均为 0，ELF 摘要为
`3e9e349310efcbb37beda7f487f07904766465da77157e91923e262de2655677`。
权限、实例、根 pin 和完整程序的源码、原始日志与断言一致性记录于生产证据的
`filesystem_instances_and_unlink_authorization`。其余三架构的本版完整结果尚未取得，
继续核对精确 head 的 CI，不能把旧 ELF 记录为本版通过。

Linux devpts 完整程序因宿主单 GID 映射使用 gid=0，Starry 仍验证 gid=5；Linux
unlink 顺序对照通过移除 capability 形成父写入拒绝，不等同于 Starry 完整的
root 到 uid/gid 1000 转换。pilot 的 Linux 候选仍有原 mmap 未知保护位预期差异，
不将 openat2 断言迁移描述为整个 Linux 程序通过。系统调用表扩展为 63 行，保留
fs_context 授权顺序、PTY 锁状态、statx 与其余未证实入口的实际缺口。同条件性能、
非阻塞 CACHED、剩余消费者与拓扑重验及独立审查继续受原验收范围约束。

### 9.38 预装依赖的主线合入

发布前 dev 推进至 `9bc2ee1411`。`06b39a18bf` 无内容冲突合入该提交，保留主线
在预装容器中只校验 host 工具与 Rust target、在裸机环境沿用安装行为的策略。
已逐项核对新的 rustup::ensure_target 与两个调用点、CI 的容器与 Node 预检，
没有恢复分散的 rustup 安装分支。固定工具链、锁文件、VFS 与 Kernel 源码相对
`77d42086c9` 完全未变，原运行结果继续保留其实际源码标签。

合并提交的 axbuild 标准库一包共 512 个主测试通过，新 rustup 的两项真实进程
夹具均执行；一项 axbuild Clippy、三项 host 依赖检查与定向格式检查通过，
最外层均为 0。标准库日志中的缺失 value.h 诊断来自既有 C 编译失效夹具的预期
拒绝，不是被忽略的构建失败。结果记录于实例证据中的 followup_mainline_merge。
最终 CI 仍以发布 head 的实际任务终态核对。

### 9.39 执行链的统一视图

`85b526ad02` 增强原 exec family 程序，分别检查 `/proc/self/fd` 与 `/dev/fd`
成功执行和遮住 procfs 后的 ENOENT。同一完整 C 在 Linux 私有命名空间为
82 pass、0 fail；旧 Starry 路径前缀分流使两个遮盖判据失败，80 pass、2 fail，
最外层退出 1。`09ccf6cb2c` 删除该执行捷径，两个 syscall 和启动候选将一次选择
的 FsView 传至主映像、shebang 与 PT_INTERP；执行权限投影取自同一 view。
execveat 的操作数 dirfd 不替换解释器所使用的原 root/cwd。

原生装载测试用真实 MemoryFs、独立视图与未发布地址空间，要求在给定视图发现
解释器并因权限拒绝，再写入脚本确认执行访问退休。临时把 shebang 查找恢复为旧
隐式全局根时，同一测试错误返回 Vfs(NotFound)，内核测试与最外层均失败。诊断
变动已与提交及备份逐字核对恢复；测试模块摘要在红绿两轮相同。

干净 `09ccf6cb2c` 的一包标准库、Kernel 四目标 72 项 Clippy、x86 内核
264 pass/0 fail/0 skip 和完整 system 541/541 均通过。首轮 Clippy 与 x86
内核编译共享 target 目录，AArch64 报告隔离期间混入 x86 bitmaps 的 SSE 告警，
被严格门禁拒绝；编译结束后串行重跑通过，未放宽包清单、报告匹配或警告。
该准备失败保留在独立日志，不能算成功检查或源码修复。

执行链证据记录于 `executable_lookup_view`，包括 Linux 对照、两种红证据、
实际 source、ELF 及日志摘要。直接 CLONE_FS 并发根切换、AT_EXECVE_CHECK、
ELF 头缓存写后失效及同条件性能仍未闭合；openat 的两个 fd 前缀捷径也仍待迁移。

上一发布 `b6755f03ec` 已取得四架构 Starry CI 的实际日志：x86 完整 541/541，
RISC-V 与 LoongArch 完整各 538/538，AArch64 分组 513、23、1、1 项全部通过，
内核测试分别为 263、262、263、273 项且零失败。该轮总计 43 项成功、5 项跳过、
1 项 Axvisor OrangePi UVC/NPU/FT232 硬件任务失败。失败在 Starry robot-flow
启动内核前等待 U-Boot shell 900 秒超时，同任务随后 UVC 成功采集 5 帧，不能
把后续成功当作 robot-flow 或未执行 Axvisor 分支通过。ArceOS AArch64 的实际
栈保护页用例已输出 guard page hit 并通过，原诊断缺失未再出现。五项跳过属于
同一 head 的重复 PR workflow，实际矩阵在 push run 执行；四架构日志与终态记录
于实例证据的 followup_current_ci。
该轮没有执行链的新测试与实现，不改写为 09ccf6cb2c 的四架构证据。

随后发布 `96c81261c4` 的执行链 CI 已完整结束，44 项成功，5 项为重复 PR
workflow 的预期跳过，push run 为 44090328458。四架构完整 system 全部零失败，
原生装载测试均实际执行，内核汇总为 x86 264、AArch64 274、RISC-V 263、
LoongArch 264 项。上一轮超时的 robot-flow 本轮通过，UVC 采集 5 帧；没有修改
板卡恢复或启动超时设置。各原始日志和终态记录在执行链证据的
followup_terminal_ci，不作为后续描述路径打开改动的运行证据。

### 9.40 描述路径的统一打开

`5f61ab5504` 扩充原 procfd 程序，覆盖普通文件与匿名管道的挂载遮盖、保留 proc
目录、访问模式、打开标志、描述状态与通道寿命；旧捷径实际失败。`18ef129e76`
再将 O_TRUNC 不得改变排队内容纳入同一旅程，完整候选在 Linux 私有用户、PID、
挂载命名空间通过，Starry 旧实现仍失败。相对目录和普通别名上的 O_TRUNC 还暴露
通用后端错误调用管道 set_len，返回 EINVAL。

`332b0512d2` 删除两个 fd 前缀入口和旧 Pipe::reopen，普通及受限打开在一次
`with_fs` 视图内完成 fd 分派，终端路径借用保存的视图。相同 C 确认遮盖、flags 与
跨访问模式已恢复，剩余失败为截断、无 writer 时 HUP 抑制，以及无 reader 时写端
重开 ENXIO。`e7d17cb3fc` 按实际 `ObjectNode<Arc<Shared>>` 选择匿名行为，保留
命名 FIFO 的伙伴协议，截断只作用于普通文件。`OpenedPipeFile`/`backing_file` 同时
描述命名与匿名对象的实际打开路径，保留原 mount use，不重建为 pipefs 根。

首次构建因遗漏五处函数指针形式的 `named_file` 调用而报 E0599。`d788254e65`
已迁移 resolve_fd、close 通知、锁范围、挂载 fd 识别与 fstatfs 消费者，删除旧名，
没有兼容别名。该构建失败独立保存，不用作语义红证据。最终 C 与 18ef129e76、
中间分派版及 Linux 候选逐字相同。

干净 `d788254e65` 的 18 包标准库、AX/Kernel 四目标 78 项 Clippy、x86 完整
system 541/541 及格式检查全部通过。Clippy 报告会话在 x86 编译结束后才开始，
没有重现异架构报告混入。ELF 摘要为
`5535ae74f03a087eb2587e927f6cffbe975484a6c2acfe0a7e46b7e68734d8ad`，
源码、日志及各阶段退出状态记录于 `descriptor_open_lookup_view`。

系统调用对照扩展至 80 行，分别保留 readv/writev、ppoll、select 和 epoll 的
直接证据缺口。创建通知仍使用存在性观察，挂载表 per-open producer、PTY 实例、
原匿名 fstatfs 与其他 raw 消费者、非阻塞 CACHED、受控共享 FS、性能及独立审查
仍按原范围完成；本版其他三架构运行由匹配发布提交的 CI 核实。

### 9.41 原生文件系统统计

`a18eedcafa` 在原 statfs 旅程加入原生 pipe/socket/namespace 与 memfd 的 fd、
proc 链接和关闭原 fd 后的副本查询。初轮在 musl 编译阶段因 f_type 符号类型不同
而失败，尚未运行，不能作为语义红证据。`b5b8baf659` 统一使用 uint64_t 比较，
并加入伪文件系统页几何信息和可区分 upper/lower 容量的 Overlay 判据。同一完整
C 在 Linux 私有命名空间 67/0，旧 Starry 真正运行后为 40 过、17 败，外层退出 1。
Linux 单 lower 的准备候选被拒绝，最终使用两个 lower；该准备失败独立保留。

`a960991962` 的 fstatfs 复用实际 `descriptor_path`，保留描述与 PathHandle 至
用户输出完成。SimpleFs 删除 512 字节、100 个块的占位统计，按固定 Linux
simple_statfs 使用页大小与零容量计数。Overlay 构造前保存实际层的最大名称上限，
每次从 upper 或首个 lower 取得当前容量并保留 Overlay 类型，逻辑挂载计算 flags，
错误直接传播；删除旧 `dummy_stat_fs`，不保留固定数据后备路径。

干净 `a960991962` 的同一完整 C 为 67/0、xtask 1/1，最外层 0；Kernel 一包
标准库、四目标 72 项 Clippy 和格式检查通过。ELF 摘要为
`97b97ddb854f3d8e966ccc55f8468681cea7c9a9e78b1f405579239da1707125`。
源码、Linux 基准、各次准备失败和真实红绿日志记录于 `native_filesystem_statistics`。
原 fsid 表达、未实现 UUID 选项、无原生路径能力的匿名描述及其他三架构运行仍
保留实际范围；不将 d788254e65 的完整 541/541 改写为本次源码的完整批次结果。

上一发布 `822838d283` 的 CI 另有待处理失败：axbuild 的 rustup 查询夹具返回
未说明原因的失败、ArceOS AArch64 缺失栈保护页诊断、LoongArch DRM 的禁用期间
vblank 序号判据失败，以及板卡预构建下载 linux-headers 的 I/O 错误。原始任务日志
已保存；本阶段本地通过不将这些任务改写为成功，继续按实际调用链处理。

### 9.42 宿主目标准备夹具

`822838d283` 的标准库任务在 rustup 安装目标查询夹具失败，旧断言没有输出实际
错误。`68ce4b0ddd` 增强原测试，保持脚本写句柄至子进程结束，并打印错误上下文；
同一查询稳定得到 ETXTBSY，512 项中 511 过、1 败，最外层退出 1。这个受控条件
证明直接执行临时脚本的脆弱点，但不能确定旧 CI 的唯一原因。

`dd987ab09a` 让私有准备函数接收已构造的 Command，生产仍直接调用 rustup；
测试明确使用 /bin/sh 读取脚本，移除脚本执行权限依赖。查询与安装的既有完整
测试都保持打开写句柄，继续验证精确 argv、完整目标匹配和非零状态传播。标准库
512/0、axbuild Clippy 1/1、格式化通过。原始日志、受控红绿源码和摘要记录于
`rustup_fixture_execution`；其他 CI 失败继续按各自责任边界处理。

### 9.43 栈保护页终止记录

`eafb3fa498` 将保护页原因直接放入主 panic 记录。原先的独立紧急写入可能返回
零字节，检测函数忽略结果后继续通用缺页 panic。相同 AArch64 用例与相同的
panic 前输出故障注入，在 3dfc6f4a42 失败、eafb3fa498 通过；注入只控制
panic 前 write_fmt 的零字节返回，实际主记录仍走真实紧急输出。

`d0a5c3a632` 补齐本地 IRQ 保护。缺页慢路径可以恢复父 IRQ，trap 本身不能
作为排除迁移的证明。实际 guard 命中后的临时契约检查确认旧代码 IRQ 仍开启，
9a755ee187 失败、d0a5c3a632 通过。检查范围内没有休眠、分配或任务锁。
Starry mm/access.rs 与 ArceOS bootstrap 都迁移新入口，文档同步删除旧名。
Starry 漏迁移导致的 E0425、旧 mkfs.ext4 与漏迁移并存的首次标准库失败，以及
为补充 IRQ 保护而中断的 Clippy 分别保留，不记作完整通过或 DRM 语义红证据。

临时注入恢复后，9b8e29fd84 的原栈保护页用例在四架构均 1/1；同一 Rust
实现与核心测试的 19 包标准库全过，核心 118 项，a8cc3944a1 的 runtime、
Kernel、kdrm-core 四目标 104 项 Clippy 与格式检查通过。精确源码、注入、
原文件摘要和退出状态记录于 stack_guard_terminal_diagnostics。boot、独立
IRQ、overflow 与 double-fault 栈仍不由动态 RuntimeStack guard 覆盖。

### 9.44 重新启用的显示序号

固定 Linux 在 modeset 增加一个序号，并对无硬件计数器的活动时间按最近整数
补算。9b8e29fd84 增强原引用生命周期测试，使用每个夹具独立的驱动时间戳，
经真实核心 off/on/get 验证关闭 100 个周期后，活动 3/4 周期得到总增量 2
与对应时间戳。没有新增核心用例、发现项、批次或默认持续集成启动。

8d412dbfe7 将系统测试原先向下取整的判据提取为原样函数。同一宿主 C 回放
包含实际系统测试源码，重放已由真实核心断言验证的样本，旧判据稳定拒绝合法
增量 2，外层 1。a8cc3944a1 按 Linux 最近整数取整后，同一回放外层 0，
并继续拒绝把关闭 100 个周期计入的增量 102，以及序号回退。
这是测试判据的受控红绿，不能写成 DRM 内核实现的修前失败。

9a755ee187 的旧判据正常 LoongArch 运行也通过，不能当作原 CI 的确定性
复现。a8cc3944a1 的原完整 DRM ioctl 旅程 1/1，外层 0；仍使用既有
capture-failures，不由隐藏的成功输出猜测逐断言数量。ELF 摘要为
0f263dcbb4e3b3a0764e2fd86efe9d0f8316727c77efe75c03cf7d8c014de122。
系统调用表扩展至 87 行，源码、共享验证和独立证据层见 vblank_resume_observation。

3dfc6f4a42 的远端标准库 512/0、LoongArch 完整 system 538/538 与 UVC
采集 5 帧均已从原始日志确认，但 ArceOS AArch64 再次缺失保护页诊断。
原 APK 下载 I/O 错误没有对应源码、镜像或超时修改，不能把后次成功归因于
本轮实现；后续发布仍须核实自身精确 head 的全部要求检查终态。

3dfc6f4a42 的完整 CI 已终态为 43 成功、1 失败、5 个既有重复工作流跳过。
四架构 Starry 完整 system 与 native suite 全部实际通过；x86 541/541，
RISC-V 和 LoongArch 各 538/538，AArch64 四组 513/23/1/1。原 statfs
和 procfd 程序在每个目标均实际执行，统计实现和 C 与 a960991962 逐字相同。
原始任务和证据索引为 ci_3dfc6f4a42_terminal，不能把该轮 ArceOS 失败改为成功。

### 9.45 实体与包装的退休

`dd31444ae5` 将 SimpleFs 分成 core、实体、配方与活跃 owner；后续提交清理
剩余 namespace 转换和初始化错误包装，并保留 UVC 原有的串行初始化。
`SimpleFsOwner` 的具体 Arc 私有，`as_filesystem` 克隆构造时唯一的通用句柄，
因此设备号与只读状态不因根重建而拆分。所有普通、可选设备与 Cgroup 根使用
同一构造协议；Cgroup 属性仍由原组件节点拥有。

完整生命周期旅程含两次真实挂载根物化、原属性保留及最后外部路径释放。
第一次增强运行漏清理合法正缓存而失败，原始 2303b0bb3d 同样不能隔离环。
补齐真实 `clear_cached_entries` 后，受控恢复原强根持有仍使外层退出 1，
失败为根包装未退休；弱根实现通过相同断言，kernel suite 265/265、外层 0。
受控源码逐字恢复，ELF 摘要为
9d29e7d70bd3f15ee08639a490f6ad998e21b91c31dab2827e503d5bd27f8054。

`5526028044` 的 mqueue 旅程实际观察 mqd/path inode 不一致及两个方向的
chmod 未共享；devpts 旅程观察重复 lookup 丢失 ptmx 权限，均外层 1。
同一 C 在宿主 Linux 或隔离 Linux 容器通过，devpts 保留 gid=5 断言。
首次 mqueue 编译缺少 `_GNU_SOURCE`、宿主旧 unshare 不支持映射选项及
容器默认 mount 策略拒绝均单独保留，不计为产品红绿。

修后完整 x86 system 中两个程序通过，Kernel 标准库测试与四目标
72/72 Clippy 通过，格式检查包含 include-backed Rust 文件。
证据键为 `simplefs_entity_lifetime`，按其中各次 source、文件摘要和实际范围
解释结果。mqueue 路径与 mqd 共享实体，但普通路径 fd 的全部 mq 操作、
动态 proc 字段策略、userns 与完整只读行为仍需相应契约；不据本次结果外推。

### 9.46 栈保护持续集成终态

`4f16858e51` 的完整持续集成终态为 44 成功、0 失败、5 个既有重复 PR
包装跳过。AArch64 原栈保护用例实际输出完整地址诊断并通过，Rust suite
9/9；旧 3dfc 的失败记录保留。该终态只证明 4f16858e51，后续 SimpleFs
提交仍须核实自己的完整 CI。原始任务与日志见 `ci_4f16858e51_terminal`。

### 9.47 队列的打开描述

普通路径打开按实际 SimpleFile 的 QueueStatus 生产者选择 MqDescriptor，保留
原 File 和挂载使用权。O_PATH 仍走自身的对象路径；元数据 fd 投影优先使用
原 PathHandle，只读 bind alias 的 fchmod 返回 EROFS。队列、原生 inode、
描述偏移和标志分别由原所有者维护，不新增名字索引或属性副本。

f7ff4bd7a1 的增强状态程序在旧普通分派下稳定失败，2bcfc68ab9 通过。
第二轮在 568402cfbc 增强定位读取、只写描述错误优先级、原始写拒绝和 seek
溢出，旧实现在该轮程序失败，最终 10ed803b4a 通过。每轮红绿和 Linux 对照
均使用各自相同 C。每个完整向量段按传统 read 操作处理，
后续段失败保留已完成字节；顺序 readv 的游标锁覆盖所有段。Linux 6.8 相同 C
也通过。首次遗漏 signal.h 的编译错误单独保存，不计产品红证据。

最终 72/72 Clippy、Kernel 标准库和格式检查通过。88cee91658 的完整 x86
system 为 541/541，后续逐段策略收敛后的 10ed803b4a 另行运行相同 mqueue
程序通过，未把前者写成后者的完整 system 结果。新测试未增加默认用例或启动。
根证据键为 mqueue_path_descriptions，兼容表共 149 行；单段内部部分复制、
受控并发、名字/删除状态、IPC 域和其他描述消费者继续保留缺口。

### 9.48 实体基础的持续集成终态

a69cd19120 的终态为 44 成功、0 失败、5 个重复 PR 包装跳过。四架构真实
日志都含原 mqueue/devpts 增强程序成功和 AXTEST_SUITE_OK；x86 system
541/541，RISC-V 与 LoongArch 各 538/538，AArch64 为 513/23/1/1 四组。
该终态只覆盖实体基础，不代表后续队列描述提交的 CI。日志及快照见
ci_a69cd19120_terminal。刷新 dev 后仍为 9bc2ee1411，已是当前分支祖先。

### 9.49 可执行头的当前读取

2b31db7844 移除仅按 Location 身份复用解析头的全局 LRU。主映像和 PT_INTERP
通过本次 ElfImage 从已有 CachedFile 重新读取，活动 ExecutableFile 与映射保留
原路径。卸载和 memtrack 的专用清理入口、uluru 依赖一并移除，没有空兼容包装。
主设计第 6.31 节记录替代方案、所有权、失败和回滚范围。

同一增强 C 在 Linux 通过，088637d6cd 在 ELF magic 已改坏后仍进入用户态，
导致原旅程失败；2b31db7844 在完整 x86 system 541/541 中通过。断言还要求失败
释放写互斥、原字节恢复后同一 inode 再次执行。默认用例与启动数均未增加。
最终源码另通过 Native 265/265、Kernel 标准库和四目标 Clippy 72/72；格式检查
包含三个 include-backed Rust 文件。全部 ELF 与日志按实际源码和摘要保存。

解释器的坏头仍存在 EINVAL/ELIBBAD 差异，execveat 的写后损坏恢复没有独立
直接旅程；兼容表分别保留部分正确和无法确认。移除解析缓存也不替代数据页对
直接、映射和后端写入的一致性证明。新头部实现还需要自己的最终 head CI。

两版相同客机与输入交替各启动三次，每次预热 8 次、保留 128 个样本。
fork/exec/退出/wait 合并中位数为旧版 21.937100 ms、修复版 21.738987 ms；
p95 分别为 33.279167 ms、22.225831 ms。宿主共享且未隔离负载，不据此宣称
性能提升。相同 Error 日志配置下 GNU size 节区合计减少 5,432 字节，bss 相同；
不是堆峰值或驻留内存测量。完整程序、配置、样本和输入/ELF 摘要已提交到
`elf_header_freshness`。原索引仅成功测得 sizeof=608，未测头对象总堆大小。
兼容表现为 151 行，新增主映像和 execveat 写后重执行分别说明证据。

### 9.50 队列描述的持续集成终态

8ea0ea4a0a 的终态为 44 成功、0 失败、5 个既有重复 PR 包装跳过。四架构真实
日志均含增强 mqueue 程序成功和 AXTEST_SUITE_OK；x86 完整 system 541/541，
RISC-V 与 LoongArch 各 538/538，AArch64 为 513/23/1/1 四组。该结果补齐上一
阶段最终队列描述的完整矩阵，不能冒充后续 ELF 头修复的持续集成证据。
原始快照和日志为 `ci_8ea0ea4a0a_terminal`；主线刷新仍为 dev@9bc2ee1411。

### 9.51 解释器格式与短读

cb2e029332 增强原 test-execve，复制真实 shell 和动态链接器并重定向私有
PT_INTERP。相同 C 在 Linux 为 104/0，旧 Starry 为 102/2，两个原始入口
均错误返回 EINVAL(22) 而非 ELIBBAD(80)。3e6f7499f9 增加专用
InvalidElfInterpreter 到 ABI 的转换，相同程序通过 104/0。

83960a291e 在同一旅程增加短头和完整内容恢复，相同增强 C 在 Linux 为
112/0，前一格式修复为 110/2，两个入口均返回 ELIBBAD 而非 EIO(5)。
c463de79d5 在解析前检查本次读取量，原生 ELF 头大小使用现有 xmas-elf
线格式类型，主映像继续允许短脚本。相同增强程序在客机为 112/0；失败后
主映像和解释器均可重新取得 writer，恢复同一解释器 inode 后实际 shell 退出 42。

最终 Kernel 标准库和四目标 Clippy 72/72 通过，格式检查包含两个
include-backed Rust 文件。61e1b04c82 只修正测试自身的 x86_64 execveat
旧头文件后备编号 358 为 322。强制后备路径在 Linux 为 98/14 到 112/0，
普通头文件下预处理逐字相同；修正后的最终 C 再次通过真实客机 112/0。
这项测试程序失败与前两轮产品错误分别记录，没有增加默认用例或启动。

生产证据键为 elf_interpreter_failures。Kernel 验证源码 c463de79d5 与
最终系统源码 61e1b04c82 分别保留，两个 Rust 文件逐字相同。以前 2b31 的
完整 541/541、Native 265/265 和耗时对照不冒充本轮结果。兼容表为 153 行；
全部 ELF 结构、体系结构、PT_INTERP 的 NUL 填充与其他优先级仍有独立边界。

### 9.52 当前头读取的持续集成终态

06400fc668 的终态为 44 成功、0 失败、5 个既有重复 PR 包装跳过。四架构
均实际运行原写后重执行程序并通过 Native suite；完整 system 数量按各自日志
记录。LoongArch 使用 --capture-failures，成功的自有程序输出按配置不重放，
真实程序退出和未过滤的构建入口均已核对，不以缺少成功 printf 判作跳过。
该终态覆盖当前头读取阶段，不能替代后续解释器错误修复的 CI。
原始快照、日志和输出策略为 ci_06400fc668_terminal。

### 9.53 缓存路径与挂载引用

46138dd272 的 cache-only O_PATH 路径从同一次非阻塞捕获中取得凭据、root/cwd、
fd 表及限制，通过封闭 CachedLookup 查询实际缓存，最后向原 fd 表尝试提交。
结果持有原 PathHandle；同挂载不重新获取 use，也不保存一份供 fstat 使用的
属性副本。设计第 7.3 节说明捕获最后可失败步骤和锁外退休，6.33 节记录范围。

同一增强 C 在 Linux 通过，6a4a9f23f1 的旧实现失败三项，46138dd272 在真实
x86 QEMU 1/1 通过。inode 身份、CLOEXEC、unlink 后 nlink、唯一缓存 fd 的
卸载 busy 和关闭后卸载均有实际判据。18 包标准库及三包 81/81 项 Clippy
通过；没有增加默认用例或启动。只证明 MemoryFs/tmpfs 搜索目录的 O_PATH，
普通打开、链接、原生后端和受控任务发布竞争继续列为未完成。

原始夹具失败也保留：Linux tmpfs 负项应为 EAGAIN，不能按有持久负项的文件
系统要求 ENOENT；组件观察须隔离其他测试的全局 writer；失效须通过另一别名
真实创建推进后端版本，不能把创建后已经为 1 的版本再次写成 1。最终断言仍
要求确定进入验证阶段、没有普通回退并拒绝已失效的负项。

### 9.54 解释器错误的持续集成终态

已发布的 6a651d64f5 刷新后为 44 成功、0 失败、5 个重复 PR 包装跳过。
四架构均实际运行 test-execve 与 Native suite；x86 为 system 541/541、
Native 265/265，RISC-V 为 538/538、264/264，LoongArch 为 538/538、265/265，
AArch64 为 system 513/23/1/1 四组与 Native 275/275，均没有失败或跳过的用例。
LoongArch 成功输出按既有配置不重放，程序退出和实际编译入口仍已核对。
原始快照及四份完整任务日志为 `ci_6a651d64f5_terminal`。该提交早于 CACHED
实现，不能作为后续缓存分支的持续集成结果。

### 9.55 缓存链接的预算与错误

c854aa5f7f 接入 MemoryFs 的不可变缓存链接文本和拥有型路径帧，所有帧与普通
查找共用一份链接预算，原后缀在展开后继续解析。缓存 BENEATH 根逃逸和
NO_XDEV 向上跨挂载返回 EAGAIN；普通模式和绝对 BENEATH 链接保留各自 EXDEV。
设计第 7.4 节及实现记录第 6.34 节说明真实数据所有者和语义边界。

同一最终 C 在 Linux 通过，5983630d73 的旧实现失败 10 项，c854aa5f7f
真实 x86 QEMU 1/1。长链经普通查询预热后，40 次成功、41 次 ELOOP；原缓存
fd 的 inode、CLOEXEC、unlink 后 nlink 和唯一挂载引用判据继续成立。现有
MemoryFs 链接集成测试同时核对原后缀与共享预算。18 包标准库、两个修改包的
9/9 项 Clippy 和格式检查通过，无 Kernel Rust 改动，无默认用例或启动增加。

首次 Linux 夹具错把缓存 dotdot 当作普通 EXDEV，并未预热长链所需的名称与
atime，已保存准确源码和失败输出。修正后保留严格预算断言，产品红绿另行
记录为 `cached_link_text`。链接 atime/挂载时间策略、动态 producer、普通缓存
打开、原生缓存元数据及受控发布竞争仍在范围，不能据此宣称完整 CACHED。

### 9.56 定时器边沿与请求起点

bcfae703e1 在实际 timer timestamp producer 保留已发布边沿，防止初次启用
采样晚于尚未前推的计时器边沿时，无符号负差产生巨量计数。周期由原锁保护，
截止时间由 timer 队列保护，原 Linux 帧数取整算法不变。866ca1cdc2 和修后
提交的 Rust 测试模块逐字相同，真实核心夹具的旧计数 3186109819 与所需 0
稳定不符；修后保留原时间，再验证后续边沿只计入一次。

系统程序通过原始 ioctl 构造相对零查询和立即事件，不依赖绝对序号较小。
相同 C 请求回放在旧请求及判据上失败 6 项，修后接受低、高和 32 位回绕起点，
并拒绝伪造目标回复。等待子进程通过原同步管道传回完整回复，父进程与真正
禁用事件核对冻结序号和截断时间。该回放属于测试判据验证，不是真实 Linux
DRM 运行。最初错误允许禁用查询的模型及对应 LoongArch 346/1 失败另存，
最终模型按 Linux 拒绝禁用后的 CRTC_GET_SEQUENCE。

最终三包标准库、核心四目标 Clippy 4/4、格式检查和原 LoongArch DRM 程序
1/1 通过，ELF SHA-256 为
243b30bfe33c9ac175e47fa85e69b16e55f426bcf34f560ad06553499b2ff9cc。
只新增一项组件测试，系统程序、默认发现和默认启动数不增加。普通旧源码
QEMU 运行也通过，不能把它当作 CI 的确定性复现。记录为 vblank_timer_epoch，
系统调用表为 174 行，并修正 card0 实际使用 Card0File 的调用链。
fa5a8830aa 的 Axvisor LoongArch 客机启动超时尚待处理，PR 继续保持草稿。

### 9.57 当前编译器的完整终态

02baa6365f 的 CI 为 44 成功、0 失败、5 个既有重复 PR 包装跳过。实际 push
任务执行四架构路径及 DRM 程序，完整 system 为 x86 541、RISC-V/LoongArch
各 538 全过，AArch64 的 513/23/1/1 四组全过。Native 为 x86 265、
AArch64 275、RISC-V 264、LoongArch 265，均无失败或跳过。

该 head 的 Axvisor LoongArch CI 和同镜像本地运行均为 1/1，完成宿主读写
及两个 Linux 客机的只读挂载。没有 Axvisor 代码改动，也没有复现或归因
先前 fa5a8830aa 的超时；本地 bridge 网络下载失败在启动 QEMU 前独立保留。
本地实际 ELF SHA-256 为
4c387426b839ea0bfb2fdd77ca8dff95ccf1716f3a09111b6c60797154f856e1。
`ci_02baa6365f_terminal` 保存终态、四架构日志与该本地观察；此终态早于
后续普通缓存只读打开，不把后续实现或完整计划记为完成。

### 9.58 缓存只读打开的实际数据

536b9c90af 从原 inode 的 userdata 尝试锁克隆已有 FileUserData::Strong，
只接受实际 backing 为 None 且未退休的缓存。最终 cached 属性检查当前
DAC，路径、页数据及挂载 use 仍由原所有者保留；每次打开的游标和预读状态
独立，dup 共享打开描述。没有普通打开、缓存创建或节点 flags 回调。

同一最终 C 在 Linux 通过、4c2ab40a5e 旧 Starry 失败四项，修后实际 x86
QEMU 1/1。文件和目录普通只读、原载荷、dup/独立游标、pread 不改游标、
EBADF、非阻塞、CLOEXEC、目录枚举及原唯一 O_PATH 卸载判据均通过。
最终 nofollow、O_DIRECTORY 和输入尾斜杠分别核对，避免读取未跟随链接的
目标文本来改变最终类型要求。实际 ELF SHA-256 为
af6332243ec71de393d8aa22cfa1c6e70da8880ecb3e09b2fa90fd5ff0d7f951。

组件使用真实 MemoryFs 检查冷缓存拒绝及热缓存的当前 DAC。初始 uid 和 vfs
条件夹具错误保留为 cached-readonly-std-invalid-fixture，不记为产品红绿。
最终 18 包标准库、三包 Clippy 81/81 和格式检查通过，六列表为 185 行，记录键为
cached_readonly_descriptions。没有新增系统程序或默认启动；其他架构的本轮
运行、写入和端点、额外策略、原生缓存、链接 atime 及受控任务/fd 发布仍需
继续。先前 02baa6365f 的完整 CI 不替代本阶段，PR 仍保持草稿。

### 9.59 状态与描述符标志的分类

4732aeade7 在原生 64 位路径打开设置原始 O_LARGEFILE，并在 F_GETFL 区分
普通描述与 O_PATH。普通投影去掉创建、独占、截断、NOCTTY、CLOEXEC；
O_PATH 只保留自己的目录与 nofollow 状态。原参数继续供设备关闭等回调使用，
fd 表中的 FD_CLOEXEC 没有被裁剪，也没有新增状态副本。

相同最终 C 在 Linux 通过，f4a9c2cb24 的旧 Starry 失败四项，修后真实 x86
QEMU 1/1。原始状态分别为普通文件 32770、普通 O_PATH 2097152、缓存只读
34816、缓存 O_PATH 2097152，与 Linux 相同。两类普通描述的 F_GETFD 和
原缓存 CLOEXEC 检查继续通过，载荷、共享位置及独占卸载引用也保留原判据。
实际 ELF SHA-256 为
a50d1eb24c07e1a2096dfced5c0e50e1fa123295597aed5f2bddb3ed47005a47。

最终 Kernel 一包标准库、四目标 Clippy 72/72、格式和定向检查通过。AX 的
前置提交 c9b35a2ee1 只澄清缓存类型错误的注释，非文档字节与基线相同，
其六个 Clippy 组合再次通过。记录为 open_description_status，六列表 193 行；
默认系统程序、启动数和 Rust 单元测试没有增加。此前 readonly 的 18 包
记录仍绑定 536b9c90af。初始状态通过不证明 F_SETFL 完整切换、所有端点及
其他目标本轮状态读取，完整 VFS 计划和独立审查仍未完成。

### 9.60 描述符额度与回滚

14ad08a41a 在捕获保护结束后，从该原表尝试预留最低数字 fd，随后才执行
BENEATH 绝对拒绝、dirfd 获取和缓存查找。复用 FileDescriptorReservation
保持原表 Arc 与 Reserved 槽；最终 try_install 在原写锁内消费预留，失败沿
既有 RAII 归还号码，描述及最后引用在锁外退休。普通 FIFO 共用数字上限
检查，PreparedFileDescriptor 的 entry-count 上限没有混入。

相同最终 C 在 Linux 通过，6799b62256 旧 Starry 的满额度冷名字、坏 dirfd
和绝对 BENEATH 分别错为 EAGAIN、EBADF、EXDEV，修后实际 x86 QEMU 1/1，
三项均为 EMFILE。输入 EFAULT/ENOENT、变更 flags 的早期 EAGAIN、恢复额度
后的最低空闲号回收及原标志/内容/挂载引用旅程全部通过。实际 ELF SHA-256 为
7cde0919b09a3ee51544f5383b5eda59357890d03b4102ead018c7c1c89b371d。

Kernel 一包标准库、四目标 Clippy 72/72、格式及三处 include-backed 检查通过，
六列表为 199 行，记录键为 cached_fd_admission。首次 Linux 未预热链接的六项
EAGAIN 作为夹具失败保留；最终私有 tmpfs 设为 MS_NOATIME 并普通预热名字，
不依赖粗粒度 atime/ctime，也不宣称 Starry 时间策略已经实现。没有新增系统
程序、启动或 Rust 单元；受控 binding/fd 发布竞争及其完整错误优先级仍待补。
0c1743f054 的远端 CI 另有 axbuild UDP 生命周期测试失败、Axvisor AArch64
HTTP stop 超时与取消的 workspace Clippy，不能由本地 fd 结果写成 CI 全过。

### 9.61 宿主退出与请求预算

618c16e8af 合入主线 d130219c0e，保持 nightly-2026-11-13。0c1743f054 远端
终态为 41 成功、2 失败、1 取消、5 个既有包装跳过；实际失败日志和快照保留，
取消的 workspace Clippy 不算通过。

UDP 工作线程原本已被 join。诊断器让合法竞争者在端口释放后抢先绑定，
旧测试稳定以 AddrInUse 失败，整包为 511/1；没有确定原 CI 丢失的 errno。
23890c167c 通过线程结果交回原 UdpSocket，finish/Drop 在调用者侧退休。
既有测试保留所有数据断言，在仍持有套接字时验证工作线程退出后的独占与
实际接收，整包在诊断器下为 512/0。测试发生重写，该记录用于测试夹具
错误证明，不是相同 Rust 测试的产品泄漏红绿。

f20c4d17ed 为成功和幂等的 pause/resume/stop 补齐已有 120 秒完成预算，
并记录 VM 请求实际耗时。相同诊断脚本执行四处真实 stop 表达式，对本地
0.2 秒响应使用 0.02/2 秒请求与完成预算，旧 0/4、新 4/4；没有借此证明
虚拟机内部进展。原始 QEMU 用例本地也通过，新合并版完整 HTTP 控制面
1/1，停止、计数器归零、重启、删除及重建断言保留。四次 stop 耗时分别为
0.019、0.007、0.009、0.014 秒，没有复现原 CI 的 15 秒超时或修改 Axvisor
生命周期实现。实际 ELF SHA-256 为
ec7787ba4d877b2cca0af335d97e9fc3a1227859bb31cf9a8d955cdda81fd372。

合并版四包标准库全部通过，包含 axbuild 512 个单元；axbuild Clippy 1/1、
格式检查通过。记录键为 ci_host_fixture_lifecycle，既有 68 项生产记录保持
原值，六列表仍为 199 行。没有新增默认启动；最终发布 head 的 CI 和 VFS
完整计划仍须继续完成。

### 9.62 描述状态的共享与隔离

5451b16b07 的 F_GETFL 从实时 nonblocking/append 状态重建两位，去掉原打开
参数中会使 O_NONBLOCK 粘住的副本。Directory 在原 open_flags 位置保存
AtomicU32，初始化及两种 setter 共享这个状态字，保留路径和访问位。既有
dup/fork 的 Arc 继续共享状态，独立打开重新构造；没有增加第二份状态表。

同一最终 C 在 Linux 通过，e6a73d6358 的实际旧 Starry 稳定失败 11 项，修后
真实 x86 QEMU 1/1。普通/缓存 File 及 Directory 通过两位的全部状态组合，
经 dup 设置后从原描述和副本观察，同时核对另一独立打开及 FD_CLOEXEC
不变，最后恢复初始状态。载荷、共享位置、pread、目录枚举及独占挂载引用
继续沿原判据通过。实际 ELF SHA-256 为
2f7118a7e5fcc7f017305136e8dc883ab436105be0bd4c68e5733d470a3fe3f1。

Kernel 一包标准库、四目标 Clippy 72/72、格式和两处 include-backed 检查通过。
记录键为 description_status_transitions，既有 69 项生产记录保持原值，六列
表为 212 行；没有新增 Rust 单元、系统程序或默认启动。原参数生产者及其
实时 getter 已逐类核对，设备包装的直接运行未借用通用 File 结果冒充通过。
F_SETFL 多位并发提交、O_PATH 命令门禁、FASYNC、O_DIRECT/O_NOATIME 以及
完整 VFS 计划与最终发布 head 的 CI 仍按各自边界继续完成。
