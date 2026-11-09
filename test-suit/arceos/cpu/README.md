# ArceOS CPU测试

本套件验证真实CPU、异常入口、页表、PMU及虚拟化接口。统一使用`cargo xtask arceos test`构建和运行；普通用例按顺序执行，成功继续，失败中止当前启动。测试内保留真实断言，不能用启动成功代替功能成功。

## 1. 文件职责

```text
cpu/
├── Cargo.toml、src/              公共no_std测试库，每个功能一份实现
├── normal/
│   ├── suite.toml               唯一启动顺序、批次和成员登记
│   ├── build.rs                 为选中批次生成顺序调用
│   ├── std/                     std-compat / kernel TLS入口
│   ├── freestanding/            core/alloc / 无kernel TLS入口
│   └── configs/<arch>/<batch>/  build.toml、qemu.toml
├── pmu/、user-entry/、cpufreq/   保留包名的板端薄入口
└── README.md
```

宿主选择、list/plan以及启动适配位于`scripts/axbuild/src/test/normal/arceos/cpu/`；构建和QEMU执行复用其上层的`qemu.rs`、`runner.rs`。函数实现不放在宿主脚本中，也不通过`#[path]`复制或包含其他项目实现。

两个入口只装配运行时，并依赖同一测试库。入口只声明运行时，测试成员features由`suite.toml`和本次选择启用，不在入口默认编入其他测试。公共库不固定启用std-compat或uspace；需要FP的单项feature同步启用CPU/HAL/runtime支持。两个入口的Clippy也读取同一清单，按真实批次和SMP逐项展开，不提供无选择的空main或隐式测试批次。

## 2. 登记与启动

`normal/suite.toml`格式：

- `run_order`：默认批次顺序，每个批次恰好出现一次。
- `[batches.<id>]`：`arch`、`image`、`mode`、构建/运行配置及额外features。
- `[[batches.<id>.tests]]`：按顺序登记`id`、`name`、`entry`和旧名称`aliases`。
- 同一批次内ID唯一；同一功能可被不同硬件批次复用。别名在同一架构下唯一。
- `id`对应库的单项feature，宿主自动启用`cpu-tests/<id>`；批次`features`补充hv、SSTC等能力，不另外维护成员清单。

普通测试入口必须是可调用的`fn() -> ()`，在函数内使用assert/expect判定。生成器强制检查返回类型，不能返回Result后被忽略。名称不强制叫run；`entry`必须指向真实库函数。

每项开始/返回时输出`CPU_TEST_BEGIN`、`CPU_TEST_PASS`；只有全部正常返回后才输出`CPU_BATCH_OK batch=<批次> selection=<all或成员>`。普通QEMU配置只有一个被动检查步骤，panic判失败，卡死超时判失败。每项在返回前恢复其借用的IRQ、异常钩子、FP、页表或虚拟化状态；成员内部不得process::exit，不捕获panic后继续执行。

`mode = "expected-fault"`只允许一个终止式功能，独立启动；成功规则只接受指定异常，异常未发生必须失败。不得把预期panic与普通批次混用同一个成功/失败规则。

当前默认启动数：

| 架构 | 启动 | 保留的环境 |
| --- | ---: | --- |
| aarch64 | 3 | A53 EL1普通、max PMU、A53 EL2 |
| riscv64 | 5 | std普通、无TLS nofault、H关闭、SSTC支持、SSTC拒绝 |
| x86_64 | 4 | SVM、VMX/KVM、FXSAVE、KVM PGE回退 |
| loongarch64 | 3 | std普通、无TLS nofault、独立boot-trap |

合计15次；板端原PMU、pmu-user、cpufreq另计3次。成员与配置以list/plan为准。ARM stage2入口已经包含完整基础guest，不应再额外调用一次。RISC-V/Loong的nofault批次只验证kernel_access，不代表进入用户态。

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
cargo xtask arceos test board --board orangepi-5-plus --test-case pmu
cargo xtask arceos test board --board orangepi-5-plus --test-case pmu-user
cargo xtask arceos test board --board orangepi-5-plus --test-case cpufreq
```

人工指定服务时显式增加`--server <host> --port <port>`，不修改共享默认值。运行前确认可用性，结束确认租约释放。QEMU通过不替代板卡验收。

## 4. 添加测试

1. 先确认现有测试未以同等成本覆盖目标错误，确定实际断言和运行条件。
2. 在`src/`增加普通模块，注册到`lib.rs`；在库Cargo.toml增加对应单项feature和必要依赖。架构专属实现用cfg限定，不提供空实现或恒成功返回。
3. 在匹配批次末尾或合适位置登记`id/name/entry`；宿主会启用该单项feature，无须在main中补第二份调用表。
4. 优先复用同一环境的批次。只有硬件能力、特权级、运行时或退出契约确实不兼容时，才新增批次与配置，并加入run_order。
5. 验证list/plan中的成员、features和启动数，再运行单项及所在完整批次。修复缺陷时必须证明同一测试在错误实现上失败、恢复后通过。

如新功能被板端复用，板端薄入口依赖此库，不复制函数体。CI根据成员feature、架构和板端依赖映射共享源码；没有独立feature的公共helper保守触发全部相关消费者。新增配置或功能后必须检查CI路由，不能只确认本地能运行。

## 5. 修改、移除与合批

- 修改判定：保留明确故障信号；不得扩大成功正则、把失败改成skip或增加无限重试。
- 修改配置：核对CPU型号、SMP、EL、H/SSTC、KVM/LVZ及TLS模式；单纯改名不能作为删环境的依据。
- 移除成员：先证明其他测试承接其独特信号，再移除清单登记；检查别名、feature、模块、依赖和板端消费者。最后一个成员移除后，连同批次及run_order项一起移除。
- 合并批次：必须实跑全部成员，确认状态恢复、顺序、失败传播和超时。只从函数列表删一项，不等于减少一次启动。
- 板端包名及旧case别名有实际消费者，调整时同步迁移其配置、CI和文档。

正式修改后运行cargo fmt、受影响`cargo xtask clippy --package <包>`、定向QEMU/板卡及CI路由检查。公共执行器还需宿主回归，尤其确保中间步骤未完成不能被最后标记覆盖。不要为固定生产名称和数量增加镜像单元测试；运行器测试使用合成配置，清单迁移通过前后执行计划和运行证据核对。
