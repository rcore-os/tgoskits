# ArceOS CPU测试

本套件验证真实CPU、异常入口、页表、PMU及虚拟化接口。统一使用`cargo xtask arceos test`构建和运行；普通用例按顺序执行，成功继续，失败中止当前启动。测试内保留真实断言，不能用启动成功代替功能成功。

## 1. 文件职责

```text
cpu/
├── Cargo.toml、src/              公共no_std测试库，每个功能一份实现
│   └── board/orangepi-5-plus/   此板专项，目前为cpufreq
├── normal/
│   ├── suite.toml               QEMU/板端启动顺序、批次和成员登记
│   ├── build.rs                 为选中批次生成顺序调用
│   ├── main.rs                  QEMU和板端共用的唯一main
│   ├── std/Cargo.toml           std-compat / kernel TLS构建声明
│   ├── freestanding/Cargo.toml  core/alloc / 无kernel TLS构建声明
│   └── configs/<arch>/<batch>/  build.toml、qemu.toml
└── README.md
```

宿主选择、list/plan以及启动适配位于`scripts/axbuild/src/test/normal/arceos/cpu/`；构建和QEMU执行复用其上层的`qemu.rs`、`runner.rs`。函数实现不放在宿主脚本中，也不通过`#[path]`复制或包含其他项目实现。

两个构建声明都通过bin.path引用normal/main.rs，只装配运行时，并依赖同一测试库。入口只声明运行时，测试成员features由`suite.toml`和本次选择启用，不在入口默认编入其他测试。公共库不固定启用std-compat或uspace；需要FP的单项feature同步启用CPU/HAL/runtime支持。两个构建声明的Clippy也读取同一清单，包括板端8核批次，按真实批次和SMP逐项展开，不提供无选择的空main或隐式测试批次。

## 2. 登记与启动

`normal/suite.toml`格式：

- `run_order`：默认QEMU批次顺序，每个QEMU批次恰好出现一次。
- 板端成员与启动顺序登记在对应板目录的`suite.toml`，本清单只负责CPU QEMU。
- `[batches.<id>]`：`arch`、`image`、`mode`、构建/运行配置及额外features。
- `[[batches.<id>.tests]]`：按顺序登记`id`、`name`、`entry`和旧名称`aliases`。
- 同一批次内ID唯一；同一功能可被不同硬件批次复用。QEMU别名在同一架构下唯一；板端按原board/case选择，不需要功能专属Cargo包装包。
- `id`对应库的单项feature，宿主自动启用`cpu-tests/<id>`；批次`features`补充hv、SSTC等能力，不另外维护成员清单。

普通测试入口必须是可调用的`fn() -> ()`，在函数内使用assert/expect判定。生成器强制检查返回类型，不能返回Result后被忽略。名称不强制叫run；`entry`必须指向真实库函数。

每项开始/返回时输出`CPU_TEST_BEGIN`、`CPU_TEST_PASS`；只有全部正常返回后才输出`CPU_BATCH_OK batch=<批次> selection=<all或成员>`。普通QEMU配置只有一个被动检查步骤，板端保留原功能/热控判定并在最后等待同一个runner的batch完成标记，panic判失败，卡死超时判失败。每项在返回前恢复其借用的IRQ、异常钩子、FP、页表或虚拟化状态；成员内部不得process::exit，不捕获panic后继续执行。

`mode = "expected-fault"`只允许一个终止式功能，独立启动；成功规则只接受指定异常，异常未发生必须失败。不得把预期panic与普通批次混用同一个成功/失败规则。

当前默认启动数：

| 架构 | 启动 | 保留的环境 |
| --- | ---: | --- |
| aarch64 | 3 | A53 EL1普通、max PMU、A53 EL2 |
| riscv64 | 5 | std普通、无TLS nofault、H关闭、SSTC支持、SSTC拒绝 |
| x86_64 | 4 | SVM、VMX/KVM、FXSAVE、KVM PGE回退 |
| loongarch64 | 3 | std普通、无TLS nofault、独立boot-trap |

合计15次；OrangePi另计2次（kernel含PMU/IPI/cpufreq，pmu-user独立）。成员与配置以list/plan为准。ARM stage2入口已经包含完整基础guest，不应再额外调用一次。RISC-V/Loong的nofault批次只验证kernel_access，不代表进入用户态。

## 3. 使用

```bash
cargo xtask arceos test qemu --arch aarch64 --test-group cpu --list
cargo xtask arceos test qemu --arch aarch64 --test-group cpu --plan
cargo xtask arceos test qemu --arch aarch64 --test-group cpu

# 整个批次
cargo xtask arceos test qemu --arch x86_64 --test-group cpu --test-case x86-svm

# 兼容旧单项名，或明确指定批次/成员
cargo xtask arceos test qemu --arch x86_64 --test-group cpu --test-case paging-kvm
cargo xtask arceos test qemu --arch x86_64 --test-group cpu --test-case x86-vmx/paging
```

`--plan`不构建、不启动，输出选择、成员、请求features、入口包、构建配置、环境和启动次数。默认写stdout；需要文件时保存到已忽略的`target/axbuild/cpu/`，不向源码目录写JSON。单项选择和整批选择具有不同构建环境及完成标记，不能混用产物。

x86完整组要求Intel KVM、可访问的`/dev/kvm`及嵌套VMX；SVM/FXSAVE使用TCG。Loong guest使用CI固定的LVZ QEMU，不能用普通Loong QEMU替代。工具链/QEMU可按CI更新，不修改模拟器语义或放宽测试。

板端配置仍在`../board-orangepi-5-plus/`：

```bash
cargo xtask arceos test board --board orangepi-5-plus --test-case kernel
cargo xtask arceos test board --board orangepi-5-plus --test-case pmu-user
cargo xtask arceos test board --board orangepi-5-plus --test-case ipi
```

PMU与cpufreq由Rust标准库kernel批次复用CPU库，IPI复用原Rust实现及runner；pmu-user继续通过normal/freestanding和原main。板目录suite.toml统一登记两批次与成员，所需features取整批并集，AX_BOARD_TEST_SUITE/BATCH/CASES参与构建身份；单项选择不额外执行其他成员。旧包装包及boot独立测试已删除，Hello应用保留。完整维护方法见[OrangePi README](../board-orangepi-5-plus/README.md)。

人工指定服务时显式增加`--server <host> --port <port>`，不修改共享默认值。运行前确认可用性，结束确认租约释放。QEMU通过不替代板卡验收。

## 4. 添加测试

1. 先确认现有测试未以同等成本覆盖目标错误，确定实际断言和运行条件。
2. 在`src/`增加普通模块，注册到`lib.rs`；在库Cargo.toml增加对应单项feature和必要依赖。架构专属实现用cfg限定，不提供空实现或恒成功返回。
3. 在匹配批次末尾或合适位置登记`id/name/entry`；宿主会启用该单项feature，无须在main中补第二份调用表。
4. 优先复用同一环境的批次。只有硬件能力、特权级、运行时或退出契约确实不兼容时，才新增批次与配置，并加入run_order。
5. 验证list/plan中的成员、features和启动数，再运行单项及所在完整批次。修复缺陷时必须证明同一测试在错误实现上失败、恢复后通过。

新增或调整测试必须先遵守[启动复用约束](../../../.agents/skills/test-quality/SKILL.md#36-优先复用启动)：加入兼容批次、统一开启所需feature并集；无法合批须说明运行条件或判定差异。板端复用功能在对应board目录suite.toml登记，不新增功能专属main或包装包。CI按真实成员依赖与架构映射源码；新增配置或功能必须核对路由及实际启动数。

## 5. 修改、移除与合批

- 修改判定：保留明确故障信号；不得扩大成功正则、把失败改成skip或增加无限重试。
- 修改配置：核对CPU型号、SMP、EL、H/SSTC、KVM/LVZ及TLS模式；单纯改名不能作为删环境的依据。
- 移除成员：先证明其他测试承接其独特信号，再移除清单登记；检查别名、feature、模块、依赖和板端消费者。最后一个成员移除后，连同批次及run_order项一起移除。
- 合并批次：必须实跑全部成员，确认状态恢复、顺序、失败传播和超时。只从函数列表删一项，不等于减少一次启动。
- 板端case和QEMU旧case别名有实际消费者，调整时同步迁移其配置、CI和文档；QEMU与板端使用同一入口源码但分别登记运行环境。

正式修改后运行cargo fmt、受影响`cargo xtask clippy --package <包>`、定向QEMU/板卡及CI路由检查。公共执行器还需宿主回归，尤其确保中间步骤未完成不能被最后标记覆盖。不要为固定生产名称和数量增加镜像单元测试；运行器测试使用合成配置，清单迁移通过前后执行计划和运行证据核对。


板卡专项实现放在`src/board/<实际板名>/`，当前调频实现为`src/board/orangepi-5-plus/cpufreq.rs`；Rust模块路径为`cpu_tests::board::orangepi_5_plus::cpufreq::run`。目录名中的连字符仅在本crate内用普通模块路径映射为合法Rust标识符，不包含其他项目生产源码。PMU和user-entry同时被QEMU及实板使用，继续保留在共享模块。移动专项路径时必须同步清单与CI路由，确保该板源码不触发无关QEMU批次。

## LoongArch非对齐恢复入口

非对齐恢复测试唯一实现位于`src/unaligned_fixup.rs`，在`normal/suite.toml`的loong-standard批次登记，默认与该批次其他成员共用一次启动。旧独立loongarch目录及测试组已移除，统一使用：

```bash
cargo xtask arceos test qemu --arch loongarch64 --test-group cpu
cargo xtask arceos test qemu --arch loongarch64 --test-group cpu --test-case unaligned-fixup
```
