# axbuild宿主工具

axbuild实现cargo xtask的构建、运行和测试命令，保持单个软件包。命令分派、运行能力与测试机制分层，各OS提供必要适配。

## 1. 模块组织

从入口选择功能，再进入对应适配和共用能力。测试与应用共享运行技术，各自拥有选择和结果规则。

### 1.1 功能模块

主要职责对应以下实际源码入口。

| 模块 | 职责 |
| --- | --- |
| [cli](src/cli/mod.rs) | 参数与命令分派，lib.rs导出run/run_from |
| [execution](src/execution/mod.rs) | 共用case资产、构建、QEMU、板卡、HTTP和计时 |
| [test/normal](src/test/normal/mod.rs) | ArceOS、Starry和AxVisor普通测试适配 |
| [test/ktest](src/test/ktest/mod.rs) | Cargo目标发现、计划、运行及axtest契约 |
| [test/ota](src/test/ota/mod.rs) | Axloader宿主检查及OTA场景 |
| [app](src/app/mod.rs) | 应用选择与执行编排 |
| [profiling](src/profiling/mod.rs) | 性能采样和分析 |
| [checks](src/checks/mod.rs) | clippy及同步静态检查 |
| [tools](src/tools/mod.rs) | 板卡CLI、符号映射和独立审查评估 |

build、context、rootfs、image、support与各OS目录保留编译、请求上下文、镜像、辅助及平台适配职责。

### 1.2 依赖边界

测试、应用和性能编排调用OS适配及共用运行能力；execution不读取具体测试名单。AxVisor客户机准备由OS的guest_build适配提供，normal和ktest共用。

原axbuild::run、run_from及OS命令路径保留；OS模块通过重导出兼容现有xtask调用方。测试资产目录和原命令不随宿主模块归位迁移。

## 2. ArceOS Rust套件

[test-suit中的suite.toml](../../test-suit/arceos/rust/suite.toml)是单一登记，目标build.rs生成函数表，宿主[catalog](src/test/normal/arceos/rust_suite/catalog.rs)读取同一数据；工具不依赖目标测试库。suite顶部run_order指定默认启动顺序，下面具名batches表嵌套tests，单源维护启动成员及组内顺序；宿主按架构展开单项Cargo features，目标build.rs按相同run_order及成员顺序生成函数表。general是通用共享启动批次，Cargo不再维护all成员。NVMe+iTCO共用x86-devices，单项优先保留专用fixture，否则继承批次runtime。

```sh
cargo xtask arceos test qemu --arch riscv64 --test-group rust --list
cargo xtask arceos test qemu --arch riscv64 --test-group rust --plan
cargo xtask arceos test qemu --arch riscv64 --test-group rust
cargo xtask arceos test qemu --arch riscv64 --test-group rust --test-case task-mutex
```

--plan只读取配置，区分逻辑项、执行次数、构建身份和启动次数，列出实际运行规则及fixture要求；不构建、不准备rootfs、不分配板卡。其他测试和工具保持原使用契约。
