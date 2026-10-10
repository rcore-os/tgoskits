# ACPICA AML 解释器

本软件包将 ACPICA 20260930 作为独立 AML 解释器构建，并提供 Rust OSL 边界。默认使用 `no_std`，当前支持 TGOSKits 的 x86_64 LP64 ABI。`vendor/` 中 ACPICA 源码保持原样；构建脚本在生成目录中选择 TGOSKits 平台头文件，不修改上游源码。

## 所有权与集成

调用方为每个内核实例提供一个 `Backend`。后端负责 ACPI 根表定位、内存映射、端口及 PCI 访问、中断注册与屏蔽、计时与休眠、延后工作和日志。必须在解释器初始化前安装，并在解释器生命周期内保持有效。ACPICA 使用进程级全局状态，因此同一时刻只能有一个 `Engine`。本软件包不负责设备发现、寄存器映射、平台策略，也不会默认启动解释器。

调用链为 `BackendRegistration` → `Engine::initialize`（或在 AML 加载前需要安装操作区处理器时使用 `initialize_with_tables`）→ 通过解释器返回的 Rust 自有值和资源。Rust 经 C ABI 桥接调用 ACPICA；ACPICA 的 OSL 回调再进入调用方后端。全局生命周期状态阻止并发创建解释器；子系统初始化后的失败会先静默并终止 ACPICA；`Drop` 会移除通知处理器并终止子系统。若 `AcpiInitializeSubsystem` 本身失败，则关闭全局初始化入口，因为 ACPICA 全局状态可能只完成了部分初始化，不能安全地再次初始化。

`Backend` 是 `unsafe` 实现边界：实现方必须提供有效且对齐的映射、同步中断移除、延后并排空工作、可嵌套的中断屏蔽，以及稳定的线程标识和单调计时值。内核设施的所有权仍属于调用方。在 `Mode::Hardware` 下，AML 可以执行固件描述的 MMIO、端口和 PCI 操作；调用方必须限制可访问范围，并仅在运行时服务就绪后调用 `unsafe` 初始化接口。

EC 操作区同样由调用方负责：调用方可以使用 `Engine::install_ec_handler` 注册命名空间处理器，并实现 `Backend::ec_access`。本 crate 的 EC 模块只提供可移植的 ECDT 解析和有界字节协议，不执行端口 I/O，也不负责控制器发现。

`Mode::Offline` 仅适用于能模拟全部硬件读写的后端，不能替代硬件验收。可选的 `host-test` 功能提供离线后端，用于测试本 crate 编写的合成 AML；它不会映射宿主物理内存或访问宿主 I/O 端口。仓库中的合成测试通过公开 API 调用真实 Rust/C 解释器，覆盖 AML 求值、命名空间遍历、表输出边界和延后通知。

## 范围与既有 AML 实现

工作区已经具备 AML 执行能力。本软件包增加的是调用方自有 OSL 服务边界上的 ACPICA 实现，不是首次为 TGOSKits 提供 AML，也不是修复现有 PCI 发现路径。

| 实现 | 当前职责与调用入口 | 与本软件包的重叠及归属 |
| --- | --- | --- |
| [`drivers/rdrive`](../../rdrive/src/probe/acpi.rs) 的 `acpi = "6.1.1"` / `acpi::aml::Interpreter` | `System::new` 装配动态平台 ACPI 发现；求值设备标识、资源和 PCI 路由，管理 PCI link IRQ 分配 | `_HID`、`_CRS`、`_PRS`、`_PRT` 等发现语义重叠。现有动态平台仍由这条路径产生内核可见的 PCI 路由和设备资源 |
| [`platforms/someboot`](../../../platforms/someboot/src/acpi/ram.rs) 的 `acpi = "6.0.1"` / `aml::AmlContext` | `get_memory_regions` 加载 DSDT/SSDT 并求值内存设备资源，服务早期启动内存发现 | AML 求值与 `_CRS` 资源发现重叠。现有启动内存信息仍以该路径为准，不改为调用 ACPICA |
| 本软件包的 ACPICA / `Backend` / `Engine` | 显式装配解释器与调用方的映射、IRQ、延后工作等 OSL 服务；当前仓库调用方仅为合成 AML 测试 | 方法求值、资源解析和 PCI 路由接口有重叠，但不参与产品固件发现，不发布第二份 PCI 路由、设备资源或启动内存信息 |

短期内，已有平台继续使用各自既有解释器；同一动态平台的 PCI `_PRT` 仍由 `rdrive` 解释，本 crate 不与其合并结果或充当失败时的备用解释器。本 PR 不退役任何旧实现，也不建立双后端选择或迁移期。后续若某个平台明确选择 ACPICA，应由独立接入变更确定唯一的 AML/路由所有者，替换并删除该平台对应的旧执行路径；不能让两个解释器对同一固件命名空间重复执行有副作用的方法，或混用两份路由结果。未发生平台接入前，移除本 crate 即可回滚本 PR，已有 AML、静态表解析和 PCI 发现行为不变。

### 方案选择与不包含项

仅保留现有实现是现有发现功能的最小方案，不需要为此引入 ACPICA；但不能交付调用方所需的 ACPICA Rust/C 与 OSL 装配边界。扩展 `rdrive` 或 `someboot` 适合它们已有的发现需求，本 PR 不否定该方案；将本次 ACPICA 装配塞进这些路径，则会同时改变平台初始化及解释器归属，超出独立可复用边界的交付范围。直接扩展现有 Rust AML 引擎也不是复用 ACPICA：需要另行承担引擎语义与 OS 服务接口的设计、实现和验证。本方案选择原样复用 ACPICA，通过调用方后端接线，而不另写 AML 引擎；代价是 Rust/C ABI、全局单实例和 OSL 生命周期契约，相关所有权与失败处理见上节。

ACPI 根表定位与表映射由调用方提供，可复用其现有表发现与所有权链路；本 crate 不再建立平台发现链路，也不携带 OEM 表。新增库与合成测试本身不改变现有平台功能。AML 在 `Mode::Hardware` 下具有平台级权限，睡眠、EC、GPE、SCI、通知及真正的平台接入仍需后端/运行时接线和独立领域审查、目标平台验收。合成测试不能证明固件、中断、MMIO、电源管理或板级行为。

## 验证边界

项目 host-test 配置会构建并执行 `tests/synthetic_aml.rs`。测试 AML 由本仓库编写，不加载抓取的固件。`cargo xtask clippy --package acpica-interpreter` 检查生产库和构建配置。

`test-suit/arceos/drivers/acpica-interpreter` 通过真实 ArceOS 客体运行生产 Rust/C 解释器。用例使用客体分配器持有合成表，验证 `_INI` 仅执行一次、方法与资源求值、延后 Notify 任务和析构排空；它不启用 `std-compat`，也不替换平台现有 ACPI 路径。运行：

```sh
cargo xtask arceos test qemu --test-group drivers --test-case acpica-interpreter --target x86_64-unknown-none
```

该 QEMU 用例使用 `Mode::Offline`，不是硬件 ACPI 验收。真实固件、EC、SCI/GPE、睡眠、电源管理及实体硬件工作先搁置。

## 许可证

ACPICA 上游源码采用 BSD-3-Clause，见 `LICENSE.BSD-3-Clause` 和 `NOTICE`。本 crate 的 Rust 代码及适配层采用 Apache-2.0，见 `LICENSE.APACHE-2.0`；软件包许可证为 `Apache-2.0 AND BSD-3-Clause`。
