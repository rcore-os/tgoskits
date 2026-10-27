# axbuild 构建参数追加契约

## 1. 配置所有权

`BuildInfo` 是 ArceOS、StarryOS 与 Axvisor 共享的构建输入。固定 CPU 客户机需要选择代码生成参数，但不能为此复制或遮蔽 axbuild 自动生成的栈保护、调试与链接参数。

### 1.1 追加参数

Build Config 根级 `rustflags: Vec<String>` 缺省为空，每项是一个完整 rustc 参数。参数保持输入顺序，不做 shell 展开，不按空白拆分数组元素。空列表不改变旧配置的构建结果；系统包装配置继续通过共享 `BuildInfo` 消费该字段。

`BuildInfo` 的 std 与 no_std 构建入口先完成目标、工具链与链接配置，再使用已有 `append_cargo_rustflags()` 向当前有效来源追加。该 helper 继续拥有来源选择、完整序列去重和参数边界，不引入第二套合并规则。参数是否适合目标由 rustc 验证，错误沿既有构建结果传播，不静默丢弃。

### 1.2 来源优先级

Cargo 的 `CARGO_ENCODED_RUSTFLAGS`、`RUSTFLAGS` 与 target/build 配置不是无条件相加的来源。没有显式环境覆盖时，新增字段保持 target 配置来源，让生成的工具链参数与追加项共同生效。显式环境覆盖继续遵守 Cargo 既有优先级，追加项进入该有效来源；不会替调用者恢复被其主动遮蔽的默认参数。

因此，想在 axbuild 默认值之上增加参数的配置应使用根级 `rustflags`，而不是 `[env] RUSTFLAGS`。后者仍作为兼容的环境输入保留，不改变其现有覆盖行为。依据为 [Cargo Book 的 build.rustflags](https://doc.rust-lang.org/cargo/reference/config.html#buildrustflags) 与 `scripts/axbuild/src/build/info.rs::append_cargo_rustflags()`。

## 2. 工具链与部署边界

工具链默认值与部署选择分别由 axbuild 和 Build Config 维护，避免同一安全或调试参数在多个板卡文件中手工同步。

### 2.1 默认参数

`toolchain_rustflags_for_mmu()` 负责需要 MMU 的内核运行时栈保护。`toolchain_rustflags()` 将 `DWARF` 转为调试信息、保留符号与帧指针参数，将 `BACKTRACE` 单独转为帧指针参数。根级 `rustflags` 不接管这些默认值；freestanding 构建的链接与目标参数也继续由原构建路径提供。

### 2.2 固定客户机

task-switch 的两份客户机配置只追加 Cortex-A76、禁用 NEON 与短复制内联代价参数，不手工复制 `-Zstack-protector=strong`。这些参数只适用于固定 A76 的客户机，不传播到宿主，也不变为所有标准库构建的默认值。AArch64 裸机短复制默认仍由 `bare_build_target_for()` 独占；客户机的显式选择及测量边界见 [任务切换测量契约](task-switch-measurement.md)。

## 3. 兼容性

此次扩展不改变默认参数、调度逻辑或配置发现规则。旧配置缺少新字段时保持原行为；迁移后的客户机配置需要支持该字段的 axbuild，回退时须同时回退工具与配置，不能让旧工具静默忽略新增字段。
