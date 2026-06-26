# spin crate 迁移与清理跟踪

## 背景

项目历史上直接或间接使用了外部 `spin` crate。`spin` 提供的 `Mutex`、`RwLock`
等同步原语是纯自旋语义：不会进入等待队列，不会主动让出 CPU，也不会自动关闭中断或禁止抢占。
这类锁如果和项目内的 `ax_kspin`、`axsync::Mutex` 等锁混用，容易形成锁语义不清、锁顺序不稳定、
睡眠上下文误用、甚至 ABBA 死锁等问题。

本计划的目标是把外部 `spin` 依赖收口到仓库内，逐步替换项目中对 `spin` 同步原语的直接使用，
最终完全移除 `spin`。

## 总目标

- 项目不再从 crates.io 引入 `spin`。
- `spin` 的临时 vendored 版本只作为迁移缓冲区存在。
- 项目业务代码不再直接使用 `spin::Mutex`。
- 项目代码不再直接使用 `spin::RwLock`。
- 引入语义明确、可纳入 lockdep / might_sleep 检查的项目内读写锁。
- 最终删除 `components/spin` 及相关兼容代码。

## 非目标

- 不把所有锁机械替换成同一种锁。
- 不把 `spin::Mutex` 直接等价视为 `axsync::Mutex`。
- 不在未确认上下文语义前，把非睡眠锁替换成可睡眠锁。
- 不用 `allow` 或局部绕过检查来隐藏锁语义问题。

## 阶段计划

### 1. 收口外部 `spin` 依赖

状态：已完成

目标：

- 将外部 `spin` crate vendored 到 `components/spin`。
- 如果依赖树中存在旧版本 `spin`，用 `components/spin-*` 临时兼容。
- 根 `Cargo.toml` 通过 workspace dependency 和 `[patch.crates-io]` 指向本仓库路径。
- `Cargo.lock` 中不再出现来自 crates.io registry 的 `spin`。

验收标准：

- `cargo tree` 中的 `spin` 均解析到 `components/spin*`。
- `Cargo.lock` 中 `spin` 条目没有 registry `source` 和 `checksum`。
- 全项目没有直接依赖外部 registry 版 `spin`。

### 2. 增加防回退 lint

状态：已完成

目标：

- 新增一个编译前检查，行为类似 `sync-lint`。（已完成：`cargo xtask spin-lint`）
- 在真正 build/test 前阻止外部 `spin` 被重新引入。（已完成）
- 将该检查纳入 GitHub workflow。（已完成：CI `static_checks` 在 `test_checks` 前执行）

建议检查项：

- `Cargo.toml` 不允许新增 crates.io 版 `spin` 依赖。
- `Cargo.lock` 不允许出现 registry 来源的 `spin`。
- 允许的 `spin` 来源只能是 `components/spin` 或明确登记的迁移兼容目录。
- 检查输出应包含文件、依赖名、来源和修复建议。

验收标准：

- 本地可通过 `cargo xtask spin-lint` 单独运行。
- CI 在 build/test 前执行该 lint。
- 人为添加 registry 版 `spin` 时，lint 能稳定失败。

### 3. 清理 `spin::Mutex`

状态：已完成（业务使用）

目标：

- 逐个分析项目中 `spin::Mutex` / `spin::MutexGuard` 的使用点。
- 按上下文语义替换为项目内合适锁类型。
- 删除项目代码对 `spin::Mutex` 的依赖。
- 删除或禁用 vendored `spin` 中 `Mutex` 的对外使用面。

当前进展：

- `components/axfs-ng-vfs` 已将 VFS 内部 `Mutex` / `MutexGuard` 别名从
  `spin::Mutex` 切换为 `ax_kspin::SpinNoPreempt`，保持非睡眠自旋语义，
  同时可通过 `axfs-ng-vfs/lockdep` 接入 `ax-kspin` lockdep。
- `ax-fs-ng/lockdep` 和 `ax-feat/lockdep` 已向下传播 `axfs-ng-vfs/lockdep`，
  ArceOS lockdep 测试套件也显式启用该 feature。
- `buddy-slab-allocator`、`ramdisk`、`rdif-serial`、`arm-scmi-rs`、
  `realtek-rtl8125`、`aic8800`、`rdrive`、`ax-driver`、`crab-usb`、
  `ax-fs-ng`、`ax-std` 和 StarryOS `wext` 中的业务 `spin::Mutex` 用法
  已替换为项目内锁。
- vendored `components/spin` 已移除 `spin::Mutex`、`spin::mutex`、
  mutex 相关 feature 和依赖该实现的 `spin::Barrier` 暴露面；新增
  `spin::Mutex` 使用会直接编译失败。
- `components/kspin/src/base.rs` 只保留 `spin::Mutex` 的历史来源说明文档引用。
- 剩余 `spin::RwLock`、`spin::Once`、`spin::LazyLock` 使用属于后续阶段，
  不包含在本阶段和本 PR 的实现范围内。

替换原则：

- 可能睡眠、阻塞、等待条件变量或等待队列的路径，优先考虑 `axsync::Mutex`。
- IRQ、抢占敏感或必须非睡眠的路径，优先考虑 `ax_kspin` 系列锁。
- 跨锁嵌套路径必须用 lockdep 验证锁顺序。
- 持锁期间可能调用 `might_sleep()` 的路径，需要纳入 might_sleep 检查。

验收标准：

- 项目业务代码中不再出现 `spin::Mutex` / `spin::MutexGuard`。
- lockdep 测试未发现新增锁顺序问题。
- StarryOS 和 ArceOS 相关测试在启用 lockdep 后通过或只剩已登记的非本阶段问题。

### 4. 引入安全的项目内读写锁并替换 `spin::RwLock`

状态：已完成（自旋型替换）

目标：

- 明确定义项目内读写锁的语义。
- 先调查当前 `spin::RwLock` 使用点是否只需要非睡眠自旋读写锁，还是同时需要可睡眠读写锁。
- 用项目内读写锁替换 `spin::RwLock`。
- 将新读写锁纳入 lockdep 和 might_sleep 管理。

前置调研：

- 调研结论：当前业务侧 `spin::RwLock` 使用点主要是短临界区表/状态保护、静态初始化、
  `try_read` 快速失败和 xHCI 寄存器/IRQ 相关路径，没有发现必须先引入睡眠型读写锁的使用点。
  因此本阶段先落地非睡眠 `SpinRwLock`；可睡眠读写锁暂不引入，留到后续出现明确长临界区或
  可睡眠上下文需求时单独设计。
- 逐点盘点当前 `spin::RwLock` 使用点，至少覆盖 StarryOS 文件表/任务表/文件锁/AIO、
  `ax-net` 路由和 socket 状态、`axtask` 任务注册表、`axfs-ng` 注册器和缓存、
  `rdrive` OSAL、`arceos_posix_api` fd/pthread 表以及 `usb-host` xHCI 寄存器访问。
- 对每个使用点记录上下文：是否可能在 IRQ/禁抢占路径调用、持锁区是否可能睡眠或阻塞、
  临界区长短、是否有跨锁嵌套、是否需要 `const fn new` 静态初始化、是否依赖 `try_*`
  或升级/降级语义。
- 根据调研结果决定是否需要同时引入两类锁：
  - 非睡眠型 `SpinRwLock`：用于保持现有 `spin::RwLock` 的短临界区纯自旋语义。
  - 可睡眠型 `RwLock`/`RwMutex`：只在明确确认调用路径允许睡眠、且自旋等待风险更高时引入。
- 不把“当前类型名是 `spin::RwLock`”直接等同于“必须继续自旋”，也不把它直接等同于
  “应该改成睡眠锁”；每个替换点必须有上下文判断依据。

待确认设计：

- 是否需要非睡眠型 `SpinRwLock`。（已确认需要，已实现）
- 是否需要可睡眠型 `RwLock`。（当前未发现必须需求，暂不实现）
- 读锁、写锁、可升级读锁是否要作为不同 lockdep class 或同一 class 的不同 acquire mode。
  （第一版仅覆盖现有 read/write/try API，未暴露可升级读锁）
- 读读共享、读写互斥、写写互斥如何在 lockdep 中表达。
  （第一版先作为非睡眠持锁状态接入现有 lockdep；读模式精细表达留作后续增强）
- 是否需要 writer fairness，避免长期读者导致写者饥饿。
  （第一版保持接近现有 `spin::RwLock` 的非公平语义）

实现参考与约束：

- 已读 `/tmp/rwlock.md`；继续以 `~/gitStudy/asterinas` 中的 Asterinas 源码作为实现参考，
  重点阅读 `ostd/src/sync/rwlock.rs`、`ostd/src/sync/rwmutex.rs` 和
  `ostd/src/sync/wait.rs`。
- 可参考 Asterinas 的紧凑 `AtomicUsize` 状态机：
  writer 位、upgradeable reader 位、being-upgraded 位、reader 计数和 reader 溢出哨兵。
- 自旋型实现优先接入现有 `ax_kspin` guard/lockdep 体系，避免引入一套平行的原子上下文模型。
  默认 `SpinRwLock` 保持历史 `spin::RwLock` 的 raw 自旋语义，不自动关闭 IRQ 或禁止抢占；
  需要禁抢占/IRQ 语义时使用显式的 `SpinNoIrqRwLock` 等别名。
- 睡眠型实现如果确实需要，应复用项目现有任务等待/唤醒机制；不要为替换少数调用点引入
  大范围调度或等待队列重构。
- 初始 API 面尽量小，优先覆盖现有 `spin::RwLock` 实际使用到的 `new`、`read`、`write`、
  `try_read`、`try_write`、`get_mut` 等能力；升级读锁、降级、公平性增强等只在现有使用点
  或明确测试需求要求时加入。
- 改动应分层推进：先新增锁和单元测试，再小批量替换调用点，最后禁用 vendored
  `spin::RwLock` 暴露面，避免一次性大范围改动造成回归定位困难。

当前进展：

- `components/kspin` 新增 `BaseSpinRwLock`，并导出 `SpinRwLock`、`SpinNoIrqRwLock`、
  `SpinRawRwLock` 及对应 read/write guard。
- 第一版 `SpinRwLock` 支持 `const fn new`、`read`、`write`、`try_read`、`try_write`、
  `get_mut`、`into_inner`、`Default`、`From`、`Debug`、`reader_count`、`writer_count`，
  并为 StarryOS active scope 的历史用法保留 unsafe `force_read_decrement` /
  `force_write_unlock` 兼容接口。
- `SpinRwLock` 已接入现有 `ax_kspin` guard 和 lockdep tracing/acquire-release 路径；
  默认别名保持 raw 自旋语义，不改变 IRQ/抢占状态，以避免改变现有 `spin::RwLock`
  调用点语义。写锁进入 task held-lock 栈；读锁先保留 trace-only 记录，避免 StarryOS
  active scope 这类长期 leaked read guard 污染 lockdep 栈。
- 已将业务侧 `spin::RwLock` 替换为 `ax_kspin::SpinRwLock`，覆盖 `ax-net`、StarryOS kernel、
  `ax-fs-ng`、`ax-task`、`ax-posix-api`、`rdrive` 和 `crab-usb`。
- `axfs-ng` 的 task/IRQ ops 注册器已改为复制 `&'static dyn ...` 后释放读锁，再调用回调，
  避免读锁跨 `task_wait` / IRQ 注册回调等潜在阻塞路径。
- `crab-usb` 已移除不再需要的 `spin` 依赖；其它仍使用 `spin::Once` / `spin::LazyLock`
  的 crate 依赖保留到后续阶段清理。

初步建议：

- 本阶段已按调研结论先引入语义明确的非睡眠型读写锁来替代 `spin::RwLock`。
- 不把现有 `spin::RwLock` 直接替换成可睡眠读写锁；可睡眠读写锁应作为调研确认后的独立
  小步变更引入。
- 新锁至少需要支持 lockdep acquire/release 记录；默认 raw 语义不主动触发 might_sleep 的
  IRQ/抢占检查，后续若需要强制持锁睡眠检查，需要在 lockdep/atomic-context 模型里进一步表达。

验收标准：

- 已形成 `spin::RwLock` 使用点分类结论，明确哪些需要自旋读写锁、哪些需要或暂不需要
  睡眠读写锁。
- 项目代码中不再出现业务使用的 `spin::RwLock`。
- 新读写锁的读写互斥、`try_read`/`try_write`、并发读写和 `force_read_decrement`
  兼容语义有明确测试覆盖；升级/降级 API 第一版未暴露，因为业务侧未使用。
- 启用 lockdep 后，StarryOS 和 ArceOS 测试不出现新的读写锁顺序问题。
- 持有非睡眠读写锁期间调用睡眠路径时，might_sleep 能报告问题。
  （默认 raw `SpinRwLock` 暂不主动改变 IRQ/抢占状态；需要后续增强 held-lock 到
  might_sleep 的联动）

### 5. 清理 `spin` 其它内容并完全移除

状态：待办

目标：

- 清理剩余 `spin` API 使用。
- 删除 `components/spin` 和临时兼容目录。
- 删除根 `Cargo.toml` 中的 `spin` workspace dependency 和 `[patch.crates-io]` 项。
- 删除防回退 lint 中的临时兼容白名单，只保留禁止外部 `spin` 的规则。

验收标准：

- `cargo tree` 中不再出现任何 `spin` crate。
- `Cargo.lock` 中不再有 `spin` package。
- `rg "spin::|\\bspin\\b"` 只剩文档、历史说明或明确允许的 lint 文本。
- CI 全量通过。

## 当前已知事实

- 项目已新增可替代 `spin::RwLock` 的非睡眠 `ax_kspin::SpinRwLock`。
- `spin::Mutex` 和 `spin::RwLock` 都是非睡眠型自旋锁语义。
- `spin::Mutex` 不会自动关闭中断，也不会自动禁止抢占。
- 单纯把 `spin::Mutex` 替换成 `axsync::Mutex` 可能改变上下文语义。
- 单纯把 `spin::RwLock` 替换成可睡眠读写锁风险较高，需要逐点确认；当前阶段未引入睡眠读写锁。
- lockdep 可以用于发现锁顺序问题，但需要锁类型主动接入。
- might_sleep 可以用于发现持有非睡眠锁时进入睡眠路径的问题，但需要知道当前任务持有哪些非睡眠锁。

## 需要跟踪的风险

- 间接依赖再次引入 registry 版 `spin`。
- 替换 `spin::Mutex` 时误把非睡眠路径改成可睡眠路径。
- 替换 `spin::RwLock` 时引入读写锁升级或公平性语义变化。
- 新读写锁未正确接入 lockdep，导致锁顺序问题不可见。
- might_sleep 未覆盖新非睡眠锁，导致持锁睡眠问题无法及时暴露。
- StarryOS 与 ArceOS 对同一锁类型的上下文假设不一致。

## 建议验证命令

按阶段选择相关命令执行：

```bash
cargo fmt
cargo xtask clippy --package <changed-crate>
cargo xtask sync-lint
cargo xtask spin-lint
FEATURES=lockdep cargo xtask arceos test qemu -g rust --arch riscv64 --no-symbolize --keep-qemu-log
FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -c qemu-smp4/system
```

## 跟踪表

| 阶段 | 状态 | 主要产物 | 验证 |
| --- | --- | --- | --- |
| 收口外部 `spin` 依赖 | 已完成 | `components/spin*`、workspace patch | `cargo tree`、`Cargo.lock` 检查 |
| 增加防回退 lint | 已完成 | `cargo xtask spin-lint`、CI `static_checks` | `cargo xtask spin-lint`、人为引入外部 `spin` 后 lint 失败 |
| 清理 `spin::Mutex` | 已完成（业务使用） | 项目内锁替换、删除 Mutex 使用面 | clippy、lockdep、StarryOS/ArceOS 测试 |
| 替换 `spin::RwLock` | 待办 | 项目内读写锁、lockdep/might_sleep 接入 | 单元测试、lockdep、might_sleep 测试 |
| 完全移除 `spin` | 待办 | 删除 vendored crate 和 patch | `cargo tree`、全量 CI |
