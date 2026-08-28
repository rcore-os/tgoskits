# Cargo 集成测试形式的 axtest

## 背景与目标

axtest 原先同时存在 crate 内 `src/axtest.rs`、ArceOS 目录扫描，以及
StarryOS/Axvisor 聚合测试三种发现方式。测试所有权、Cargo feature、运行时和架构选择
因此分散在目录约定与 runner 特例中；同一个 crate 的测试还可能被聚合内核重复带入。

本设计把 Cargo metadata 作为唯一事实来源：

- crate 在 `[dev-dependencies]` 中通过相对 path 直接依赖 workspace `axtest`，表示参加 workspace axtest；
- 每个测试入口都是 `tests/` 下的 `[[test]]`，并设置 `harness = false`；
- 一个 Cargo test bin 构建一次、启动一次 QEMU；
- `cargo xtask ktest qemu` 负责 workspace 发现、筛选、构建、运行与汇总；
- QEMU 支持矩阵来自 `[package.metadata.docs.rs].targets`，CI 按架构串行执行计划。

目标用户是维护通用 crate、ArceOS、StarryOS、Axvisor 与板卡测试的开发者。成功标准是
Cargo 能识别每个测试 target，workspace 运行不会遗漏或重复，显式选择错误能尽早报告，
并且每个 QEMU 运行单元可以独立定位失败与覆盖率产物。

本次不改变 axtest descriptor、KTAP 输出、StarryOS syscall/Linux ABI、平台启动顺序，
也不合并 ArceOS 原有 Rust/C suite。真实板卡调度仍由 `ktest board` 和 ostool 负责。

## 测试选择边界

axtest 只用于必须依赖 QEMU 或真实板卡才能成立的语义，例如目标架构指令行为、IRQ/
preempt/per-CPU 运行时状态、内核任务调度、真实块设备/文件系统以及板级外设。可以在宿主
标准 Rust harness 中确定性执行的算法、状态机、格式化、参数校验、编码/解码和内存布局
测试，一律使用普通 `#[test]`，通过 `cargo test` 或 `cargo xtask test` 运行。

迁移时先判断测试实际依赖的能力，而不是机械地把全部 `src/axtest.rs` 搬入 `tests/`。
上层内核、hypervisor 或板卡测试包可以同时拥有标准测试与一个 axtest target；若
host-test 能以项目已有的正式 adapter 安装所需上下文，则仍属于标准测试，不应仅为复用
旧入口而启动 QEMU。axtest 启动依赖链上的库本身不拥有 axtest target：纯逻辑留在所属
源码文件末尾，依赖真实 ArceOS 调度、IRQ、SMP 或目标架构的行为由
`test-suit/arceos/rust` 通过生产公开 API 覆盖。不得新增只为让测试通过而模拟内核全局
状态的临时 fake runtime。

标准测试继续遵守 Cargo/Rust 的测试边界：验证私有规则的单元测试放在所属实现源文件
末尾的 `#[cfg(test)] mod tests` 中，测试 helper 保持在该私有模块内；从 crate 外部验证
公开 API、feature 组合或链接契约的集成测试放在 `tests/`。不得为了让标准测试跨模块
转调而新增公开 `*_for_test` facade。目标运行时测试也只调用已有生产公开 API，或作为
上层 consumer 的源码内私有 `#[axtest]` 模块直接测试其实现；不得新增公开、
`doc(hidden)` 或 `pub(crate)` 的 `axtest_support` 转发层和测试状态注入接口。

## Manifest 契约

确认测试确实需要 QEMU/板卡、且 package 是拥有运行时的上层 consumer 或板卡测试包后，
使用以下模板：

```toml
[features]
axtest = []

[dev-dependencies]
axtest = { path = "<relative-path>/components/axtest/axtest" }
ax-hal = { path = "<relative-path>/os/arceos/modules/axhal" }
ax-std = { path = "<relative-path>/os/arceos/ulib/axstd" }

[[test]]
name = "axtest"
path = "tests/axtest.rs"
harness = false
required-features = ["axtest"]
```

仓库内 dev-dependency 必须使用相对于当前 package 的 path-only 声明，不得写 `version`，
也不得用 `workspace = true` 间接继承根 workspace 的版本。Cargo 发布时只保留带版本的
dev-dependency；path-only 依赖会从发布 manifest 中剥离，因此既能让本地集成测试依赖
workspace crate，又不会把允许的测试依赖环带入 registry 发布图。外部 registry
dev-dependency 仍按正常方式声明版本。

`axtest` package feature只承载被测代码所需的 alloc、IRQ 等前置能力；测试 target 需要的
feature 写入 `required-features`。workspace 发现只认可直接、development kind、路径解析到
workspace `axtest` package 的依赖，避免传递依赖造成误选。

声明了该依赖却没有 `harness = false` 的 `[[test]]` 属于 manifest 错误。workspace 扫描对
未声明者静默跳过；显式 `-p` 选择未声明者时返回错误。

外部 test target 只使用 crate 公共 API。确需验证上层 consumer 私有内核状态时，测试应
放在对应实现文件末尾的 `#[cfg(all(test, axtest))]` 模块，由专用 runner 与普通 `lib.rs`
共享同一个内部 crate root；runner 只负责启动和收集 descriptor。测试不得用
`#[path = "../src/..."]` 复制生产模块，也不得为了测试直接公开内部表示。

## 运行时所有权

默认运行时是 ArceOS。非默认 package 显式声明：

```toml
[package.metadata.axtest]
runtime = "arceos" # 或 starry、axvisor、board
```

- `arceos` 复用 ArceOS build/QEMU 配置；
- `starry` 保留 StarryOS 的 rootfs、kallsyms 和镜像后处理；
- `axvisor` 保留 Axvisor 的 rootfs 与镜像契约；
- `board` 不进入 workspace QEMU 计划，只能通过 `cargo xtask ktest board` 执行。

runner 读取已有 TOML 中的 `uefi`、`to_bin`、CPU、设备、rootfs、成功正则等配置，
不按架构猜测或覆盖平台契约。package 目录中的 `build-<triple>.toml` 与
`qemu-<arch>.toml` 优先；不存在时才回退到对应运行时的配置目录。

## 架构矩阵

QEMU 只识别以下 bare-metal target：

| `--arch` | target triple |
| --- | --- |
| `x86_64` | `x86_64-unknown-none` |
| `aarch64` | `aarch64-unknown-none-softfloat` |
| `riscv64` | `riscv64gc-unknown-none-elf` |
| `loongarch64` | `loongarch64-unknown-none-softfloat` |

package 声明 `[package.metadata.docs.rs].targets` 时，runner 只展开其中可识别的
bare-metal target；一个也没有则报 manifest 错误。未声明 targets 的通用 crate 只展开
x86_64。只有经相应 QEMU 或板卡验证、确有架构绑定行为的 crate 才应增加额外 target。

`--arch` 与 `--target` 互斥。workspace 运行带 `--arch` 时，不支持该架构的 package 会被
过滤；显式 `-p` 选择不支持该架构的 package 会报错。这样 CI 可以在一个同架构 job 中
串行运行全部适用的 ArceOS axtest，而不会把默认 x86_64 的通用 crate错误地交给其他架构。

## 发现与执行流程

runner 分为五层：

1. 读取 Cargo metadata，发现 package、test target、runtime 与 docs.rs targets；
2. 应用 `--workspace`、`-p/--package`、`--exclude` 和 `--test` selector；
3. 展开并稳定排序 `(package, test target, target triple)` 执行计划；
4. 每个单元只选择对应 `--test` artifact，完成构建、平台后处理与一次 QEMU 运行；
5. 汇总执行结果和覆盖率路径。

无参数 `cargo xtask ktest qemu` 等价于 workspace 全量选择。排序固定为 package、test
target、arch/target。默认首个失败后停止；`--no-fail-fast` 继续其余单元并最终返回聚合
失败。由于 target 使用自定义 `harness = false`，命令不支持 Cargo `TESTNAME` 或 `--`
后的 libtest 参数。

features、profile、target-dir、locked/offline/frozen 等 Cargo 风格构建参数逐单元传递。
`--config` 和 `--qemu-config` 只允许用于唯一执行单元，避免一个平台配置被错误复用于
多个 package 或架构。

## 判定与覆盖率

普通执行必须到达 `AXTEST_SUITE_OK`；panic、`AXTEST_SUITE_FAIL` 或失败 case 都使当前
单元失败。覆盖率运行等待 `AXTEST_COVERAGE_DONE`，每个单元使用包含 package、test
target、target triple 的独立文件名：

```text
coverage/<package>-<test>-<target>.profraw
coverage/<package>-<test>-<target>.profdata
coverage/<package>-<test>-<target>-html/index.html
```

这避免同 package 的多个 test bin 或多个架构相互覆盖。workspace HTML 逐单元生成，
路径随该单元输出。

## CI 与迁移

ArceOS 的四个 QEMU 架构 job 运行 Rust/C suites。启动依赖库原先需要真实运行时的 case
迁入 Rust suite 后，不再追加 workspace 级 ktest；这避免为每个底层库重复启动同一套
ArceOS。StarryOS、Axvisor 和 SG2002 板卡测试包保留各自专属 axtest 验收；self-hosted CI
的 `cache_key` 继续为空字符串。

迁移时先把可在宿主运行的部分改为普通 `#[test]`，只有剩余的目标运行时测试才移入
`tests/axtest.rs`。Starry kernel 的白盒 case 位于所属实现文件末尾，由
`src/axtest_kernel.rs` 的单一 target 共享内部 root；该 runner 不包含测试 façade。
StarryOS/Axvisor 聚合 bin 不再通过 feature、dev-dependency 或 link-only import 捎带其他
crate descriptor。SG2002 测试标记为 `board` runtime。

旧的 `cargo xtask arceos test qemu --test-group axtest` 目录发现入口已移除并只给出迁移
提示。ArceOS 的 `rust`、`c` 和其他显式自定义 suite 不受影响。

## 备选方案

- **保留目录扫描**：无法让 Cargo metadata 表达 target、feature 与 package 所有权，且容易
  与聚合 bin 重复执行，因此不采用。
- **每个 OS 维护一个聚合 test bin**：QEMU 启动次数少，但任何链接、feature 或 case 失败
  都难以归属到原 crate，也会产生跨 crate private API 依赖，因此仅保留 OS 自身测试。
- **按 arch 猜测 platform/QEMU 配置**：会破坏 UEFI、to-bin、设备和 rootfs 契约，因此
  架构只用于选择已声明的 target，平台配置仍由 package/runtime TOML 所有。
- **使用 libtest harness**：目标环境没有 host libtest 运行时，且 axtest 已拥有 descriptor、
  executor、KTAP 与覆盖率协议，因此使用 Cargo target discovery，但关闭 Cargo harness。
