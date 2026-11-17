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

## 5. AxVisor功能检查引用

AxVisor普通测试保持QEMU/板卡配置发现方式。运行配置的`[axbuild]`声明有序`checks`文件引用，场景名称默认取配置目录相对分组根的路径；不支持旧兼容别名，外部调用方仍可用`id`显式指定组名；功能定义保存原shell步骤或host probe，工具在内存展开成标准运行配置，必要的临时配置只存放于target目录并自动清理。组内任一失败或未完成均使整组失败，probe完成后才统一退出QEMU。

调用方可以通过`axvisor test qemu/board --test-root <套件根> --test-group <分组>`提供自己的数据树；不依据general或平台目录名接管测试，不要求工具安装于TGOSKits目录。检查引用相对运行配置，probe和asset_dir相对功能定义；构建/客机原路径契约保持。具体项目布局及增改规则见[test-suit/axvisor/README.md](../../test-suit/axvisor/README.md)。

应用也可通过`axvisor app qemu/board --app-root <应用根> --group <分组>`提供显式数据树，按目录路径选择应用；默认的`apps/axvisor`入口保持。仓库新增的手动套件位于`apps/suite`，由调用方传入，不在工具内硬编码其路径或成员。

`--test-case`可选择组内检查文件名或`<组路径>/<检查名>`，整组路径选择全部成员。先选择再展开、按运行配置合并为一次启动，执行顺序由checks决定。功能文件可用`requires`引用同目录或相对路径的准备文件；同一次启动执行一次，循环依赖和未知/歧义成员明确失败。准备文件仍属于调用方数据，不引入setup目录或TGOSKits硬编码。应用使用`--app`选场景，可再用`--test-case`选其中成员。

AxVisor运行时guest归档及其配置索引按内容寻址发布，避免等待板卡期间被其他调用覆盖。构建子进程通过`AXBUILD_GUEST_BUNDLE: ` JSON报告path/sha256，OTA等消费者读取并验证报告中的归档，不依赖固定arch.cpio文件名。

### AxVisor串口与HTTP共用启动

Unix宿主的混合组使用同一QEMU串口socket和HTTP转发，按调用方checks/requires的声明顺序执行，不依赖组名、架构目录或TGOSKits私有清单。QEMU以串口wait=on避免丢失启动输出；串口、探针和进程退出均成功后才通过，HTTP阶段与退出排空期间仍监控全局串口失败。纯串口和纯HTTP保留原执行路径，ostool未修改；非Unix宿主明确拒绝混合串口模式。

`[host_http_probe.env]`是可选的字符串映射，用于调用方的客机ID、目录等profile参数。框架的HTTP地址、case目录和连接/请求预算保持优先，不改变父进程环境。选择单项时，显式超时按已选成员及去重后的requires求和，并受整组预算约束；未声明成员预算的旧配置继续使用整组预算。

混合串口的待处理窗口和错误匹配记录各限制为16MiB，超限明确失败；不会截断后仍判通过。退出会读取已排队的数据，再释放串口监控器。
