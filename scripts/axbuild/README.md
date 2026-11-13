# axbuild 宿主工具库

axbuild 为 xtask 提供构建、运行、测试、镜像与静态检查能力。库运行在宿主机，具体操作系统的内核与测试程序由相应软件包提供。

## 1. 定位与边界

根 `tg-xtask` 调用 `axbuild::run()`，AxVisor 和 StarryOS 的包内 xtask 也直接调用 OS 接口。公开入口由 `src/lib.rs` 导出，库可以通过普通 Cargo 依赖交付。

### 1.1 独立交付

解包后的 axbuild 不需要 TGOSKits 父目录文件即可编译。依赖边界遵循[独立宿主工具库](../../.agents/skills/rust-api-design/references/modules-and-dependencies.md#13-独立宿主工具库)：编译输入在包内，跨包复用通过公开接口或明确数据；使用者可以提供工作区与运行资产。

### 1.2 声明所有权

通用执行机制读取显式声明，仓库拥有测试成员、包名、features、环境协议和完成条件。目标绑定由 `test-suit/arceos/build-support` 通过正常构建依赖生成，目标构建脚本不包含 axbuild 私有源码，axbuild 也不依赖该测试辅助包。

## 2. 代码分层

模块按命令、领域适配与执行技术组织。`arceos`、`starry`、`axvisor`、`axloader` 提供 OS 适配，`build` 与 `context` 负责公共构建和配置。

### 2.1 功能模块

主要功能入口与职责对应以下模块。

| 模块 | 职责 |
| --- | --- |
| `cli` | 参数解析与命令分派，导出 `run`、`run_from` |
| `execution` | 用例资产、公共构建、QEMU、板卡、HTTP和计时 |
| `test/normal` | ArceOS、StarryOS和AxVisor测试编排 |
| `test/ktest` | Cargo目标发现、axtest计划、运行与完成契约 |
| `test/ota` | Axloader宿主检查与OTA场景 |
| `app` | 应用选择和运行编排 |
| `profiling` | 性能采样和分析 |
| `checks` | Clippy与同步静态检查 |
| `image`、`rootfs` | 镜像管理和文件系统资产处理 |
| `tools`、`support` | 板卡CLI、符号映射、进程、下载与输出检查 |

测试与应用可以共享执行能力，但各自持有选择与结果契约。

### 2.2 解析与准备

`resolve_qemu_case()` 生成最终请求、Cargo配置和 `BuildFiles` 配方；计划读取同一结果计算稳定身份，实际运行再将配方落盘并准备资产。`FEATURES` 等有效覆盖进入计划和构建；计划不会生成rootfs、启动目标或申请板卡。

## 3. 声明式套件

普通目录发现继续适用于调用方自己的分组。通用执行器不因为分组叫 `cpu` 或 `drivers` 就改变规则；批次执行由分组软件包的 `package.metadata.axbuild.qemu_suite` 明确启用。

### 3.1 执行入口

入口声明选择机制和数据文件，不复制测试名单。例如CPU分组使用以下配置。

```toml
[package.metadata.axbuild]
qemu_suite = "normal/suite.toml"
```

`qemu_suite` 相对声明它的分组软件包解析并保持在分组内。不存在该元数据时，沿用普通目录发现；声明无效则明确失败，不隐式回退。

### 3.2 批次数据

`test/normal/arceos/declared` 接收 `run_order`、批次的 `package`、架构、std/freestanding模式、构建/QEMU配置与成员features；`execution` 声明环境键、成员选择方式和包含批次/选择的完成模板。CPU与drivers的数据仍位于各自测试目录。

成员按声明顺序执行，所需能力按声明范围取并集，未选成员不会因为功能并集而额外调用。预期终止保留原配置判定，选择了PCM成员时仍执行宿主波形核验。

### 3.3 计划与诊断

计划的 `cargo_features`、`build_environment`、`cargo_configuration` 来自最终解析结果；`build_identity` 对映射顺序不敏感，同时区分影响构建的配置变化。CPU单项别名及drivers成员选择继续可用。

```sh
cargo xtask arceos test qemu --arch riscv64 --test-group cpu --plan
cargo xtask arceos test qemu --arch riscv64 --test-group cpu --test-case paging
cargo xtask arceos test qemu --arch x86_64 --test-group drivers --test-case intel-hda
```

这些命令使用仓库提供的声明；独立项目可以采用不同包名与能力前缀。Rust与C套件原有的选择入口保持，调整清单时应同步核对目标生成和实际运行。

## 4. 验证与交付

库编译独立性和系统运行正确性分别验证。仓库内构建不能替代解包后的真实编译；成功的独立消费者编译也不能替代QEMU测试。

### 4.1 独立制品检查

`scripts/test/check_axbuild_independence.py` 暂存axbuild及本地依赖的规范化crate制品，通过Cargo校验的目录源在独立临时目录编译库和公开API消费者，并实际初始化普通单包项目的各OS上下文。脚本还检查调用方是否直接包含工具私有源码。

```sh
python3 scripts/test/check_axbuild_independence.py
```

暂存阶段使用 `--no-verify` 只为组装制品，随后必须真实编译；最终成功以 `AXBUILD_INDEPENDENCE_PASS` 为准。暂存尚未发布的依赖不等于它们已在crates.io可用，不执行真实发布。

### 4.2 定向行为检查

Rust修改使用 `cargo xtask clippy --package axbuild`，并运行受影响的宿主测试和QEMU路径。根工具、AxVisor及StarryOS自己的xtask都是消费者；公共入口变化必须核对这些调用方。套件合并不得削弱成员断言、成功条件或失败传播。
