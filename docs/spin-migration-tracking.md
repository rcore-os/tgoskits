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
- 项目代码不再直接使用 `spin::Mutex`。
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

状态：进行中

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

状态：待办

目标：

- 新增一个编译前检查，行为类似 `sync-lint`。
- 在真正 build/test 前阻止外部 `spin` 被重新引入。
- 将该检查纳入 GitHub workflow。

建议检查项：

- `Cargo.toml` 不允许新增 crates.io 版 `spin` 依赖。
- `Cargo.lock` 不允许出现 registry 来源的 `spin`。
- 允许的 `spin` 来源只能是 `components/spin` 或明确登记的迁移兼容目录。
- 检查输出应包含文件、依赖名、来源和修复建议。

验收标准：

- 本地可通过 `cargo xtask <lint-name>` 或类似入口单独运行。
- CI 在 build/test 前执行该 lint。
- 人为添加 registry 版 `spin` 时，lint 能稳定失败。

### 3. 清理 `spin::Mutex`

状态：待办

目标：

- 逐个分析项目中 `spin::Mutex` / `spin::MutexGuard` 的使用点。
- 按上下文语义替换为项目内合适锁类型。
- 删除项目代码对 `spin::Mutex` 的依赖。
- 删除或禁用 vendored `spin` 中 `Mutex` 的对外使用面。

替换原则：

- 可能睡眠、阻塞、等待条件变量或等待队列的路径，优先考虑 `axsync::Mutex`。
- IRQ、抢占敏感或必须非睡眠的路径，优先考虑 `ax_kspin` 系列锁。
- 跨锁嵌套路径必须用 lockdep 验证锁顺序。
- 持锁期间可能调用 `might_sleep()` 的路径，需要纳入 might_sleep 检查。

验收标准：

- 项目代码中不再出现业务使用的 `spin::Mutex` / `spin::MutexGuard`。
- lockdep 测试未发现新增锁顺序问题。
- StarryOS 和 ArceOS 相关测试在启用 lockdep 后通过或只剩已登记的非本阶段问题。

### 4. 引入安全的项目内读写锁并替换 `spin::RwLock`

状态：待办

目标：

- 明确定义项目内读写锁的语义。
- 用项目内读写锁替换 `spin::RwLock`。
- 将新读写锁纳入 lockdep 和 might_sleep 管理。

待确认设计：

- 是否需要非睡眠型 `SpinRwLock`。
- 是否需要可睡眠型 `RwLock`。
- 读锁、写锁、可升级读锁是否要作为不同 lockdep class 或同一 class 的不同 acquire mode。
- 读读共享、读写互斥、写写互斥如何在 lockdep 中表达。
- 是否需要 writer fairness，避免长期读者导致写者饥饿。

初步建议：

- 优先引入语义明确的非睡眠型读写锁，用于替代现有 `spin::RwLock`。
- 不把现有 `spin::RwLock` 直接替换成可睡眠读写锁，除非逐点确认调用路径允许睡眠。
- 新锁至少需要支持 lockdep acquire/release 记录和 might_sleep 的非睡眠锁检查。

验收标准：

- 项目代码中不再出现业务使用的 `spin::RwLock`。
- 新读写锁的读写互斥、递归、升级、降级等语义有明确测试覆盖。
- 启用 lockdep 后，StarryOS 和 ArceOS 测试不出现新的读写锁顺序问题。
- 持有非睡眠读写锁期间调用睡眠路径时，might_sleep 能报告问题。

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

- 项目没有可直接替代 `spin::RwLock` 的成熟自研读写锁。
- `spin::Mutex` 和 `spin::RwLock` 都是非睡眠型自旋锁语义。
- `spin::Mutex` 不会自动关闭中断，也不会自动禁止抢占。
- 单纯把 `spin::Mutex` 替换成 `axsync::Mutex` 可能改变上下文语义。
- 单纯把 `spin::RwLock` 替换成可睡眠读写锁风险较高，需要逐点确认。
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
cargo xtask <spin-lint-name>
FEATURES=lockdep cargo xtask arceos test qemu -g rust --arch riscv64 --no-symbolize --keep-qemu-log
FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -c qemu-smp1/system
FEATURES=lockdep cargo xtask starry test qemu --arch riscv64 -c qemu-smp4/system
```

其中 `<spin-lint-name>` 需要在第 2 阶段确定实际命令名。

## 跟踪表

| 阶段 | 状态 | 主要产物 | 验证 |
| --- | --- | --- | --- |
| 收口外部 `spin` 依赖 | 进行中 | `components/spin*`、workspace patch | `cargo tree`、`Cargo.lock` 检查 |
| 增加防回退 lint | 待办 | 编译前 lint、CI workflow | 人为引入外部 `spin` 后 lint 失败 |
| 清理 `spin::Mutex` | 待办 | 项目内锁替换、删除 Mutex 使用面 | clippy、lockdep、StarryOS/ArceOS 测试 |
| 替换 `spin::RwLock` | 待办 | 项目内读写锁、lockdep/might_sleep 接入 | 单元测试、lockdep、might_sleep 测试 |
| 完全移除 `spin` | 待办 | 删除 vendored crate 和 patch | `cargo tree`、全量 CI |

