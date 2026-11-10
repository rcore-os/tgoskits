# ArceOS C/POSIX测试

本套件采用normal的app-c入口，在真实ArceOS/QEMU中经过本仓库C头文件、ax-libc及POSIX适配层验证功能。默认每架构一次启动执行7项，四架构共4次；不为每项重新启动。

## 目录和职责

```text
c/
├── suite.toml                    唯一批次、顺序、成员、函数入口和能力登记
├── src/
│   ├── main.c                    顺序执行、失败退出及最终完成
│   ├── test.h                    共同类型、检查宏及生成声明接入
│   ├── mem.c
│   ├── pthread.c                 basic/parallel两个入口
│   ├── sleep.c
│   ├── pipe_epoll.c              pipe/epoll两个入口
│   └── net.c
├── build-<target>.toml            源码目录、核数、日志及目标追加配置
├── qemu-<arch>.toml               机器、设备、超时及HTTP夹具
└── README.md
```

`suite.toml`顶部run_order决定批次顺序，`batches.general.tests`按实际调用顺序登记mem、pthread-basic、pthread-parallel、pthread-sleep、pipe、epoll、net-http。all只是general的兼容别名，没有第二份all名单。

测试身份是函数入口，不是文件名：pthread.c及pipe_epoll.c各包含两项。测试函数返回0继续，非零停止，全部结束才输出带批次/选择的`C_BATCH_OK`；空选择、未知名称及非法登记报错，不默认为成功。

## 从命令到执行

宿主适配位于`scripts/axbuild/src/test/normal/arceos/c_suite.rs`及其catalog/bindings/listing模块，list、plan、run使用同一选择。宿主生成两份头文件：

- suite_declarations.h：全部登记函数原型，由test.h包含；所有实现必须包含test.h，使定义与声明在同一编译单元核对签名。
- suite_selected.h：只有所选函数表及完成身份，只由main.c包含。

生成物均在target/axbuild/arceos-c下，按内容身份隔离，内容不变不重写，不提交Git。公共`arceos/cbuild`只接收通用include目录、按文件追加编译参数和构建输入，不维护测试名称。不新增Cargo包或Rust build.rs。

同批成员所需features取并集，即使单项诊断也不关闭该兼容批次的运行能力；函数调用由所选表决定，不因依赖feature联动而增加。plan同时展示前端features、实际Cargo features/env、生成表身份、配置及实际完成规则。

## 使用

```bash
# 默认7项、1次启动
cargo xtask arceos test qemu --arch riscv64 --test-group c

# 整批，all兼容
cargo xtask arceos test qemu --arch riscv64 --test-group c --test-case general
cargo xtask arceos test qemu --arch riscv64 --test-group c --test-case all

# 单项、同批多项均一次启动；顺序按清单，重复名称去重
cargo xtask arceos test qemu --arch riscv64 --test-group c --test-case pipe
cargo xtask arceos test qemu --arch riscv64 --test-group c --test-case epoll,pipe,pipe

cargo xtask arceos test qemu --arch riscv64 --test-group c --list
cargo xtask arceos test qemu --arch riscv64 --test-group c --plan
```

--plan默认只向stdout输出JSON，保存时写入已忽略的target目录。测试必须走test --test-group c，由适配器生成头；普通C应用走通用app-c入口，不依赖这些测试生成物。

## 构建复用

公共C构建器先持有对应Cargo target/架构的构建锁，再构建Rust ax-libc、处理C对象、归档和链接，保护共享静态库及产物。不同配置/选择输出按真实构建身份区分。

每个C文件进行真实预处理并保存编译器依赖文件，代码生成缓存键包含预处理内容、源文件、参数、架构工具身份及相关环境。这样实际包含的头文件、宏、生成ABI布局和新增遮蔽头文件都经过真实预处理，不以猜测头文件变化代替编译器。

- 输入不变：复用对象，跳过代码生成、归档、链接及符号生成。
- 选中表变化：通常只重编main.c，其余对象复用；以实际日志为准。
- 改C/头/参数/编译器：相应缓存失效；对象损坏也会重建。
- 删除源码：从当前对象集合构建新归档，不向旧archive追加遗留成员。

预处理仍调用编译器，日志分别列出preprocessed、compiled、reused、archive、link，不把compiled=0说成没有编译器调用。工具身份每次按真实文件内容重新计算摘要；不能仅按长度、mtime或ctime复用摘要，因为同一时间粒度内的改写可能不改变这些属性。摘要仍有开销，不能为提速接受旧工具身份。

链接键包含真实对象/静态库、工具与递归INCLUDE脚本；输出完整性受校验。符号缓存同时要求AXBT和AXKS完整，生成器源码/依赖身份与ELF参与校验。临时文件成功后原子发布，失败不发布部分产物；修改缓存解释规则须更新缓存版本。

## 增改删

遵守[启动复用约束](../../../.agents/skills/test-quality/SKILL.md#36-优先复用启动)。

1. 在src中添加唯一实现，使用`int entry(char *reason, size_t reason_len)`，包含test.h；不手写另一份声明或调用名单。
2. 在清单的兼容general批次登记ID、名称和入口；新增所需features统一并入批次能力。仅feature不同不能新增启动。
3. 确有不兼容运行条件或判定时才建立独立批次，并解释原因，同步run_order和运行契约；不要把预期关机/异常并入普通顺序程序。
4. 用list/plan核对项数、成员、features和启动数，实跑整批/受影响单项及错误返回，确认失败停止后续且外层非零。
5. 改公共编译器时补普通C应用验证，检查冷/热、C/头/布局/参数、切换选择及删除源文件失效；不能只验证C测试自己的入口。
6. 删成员同步清理专属实现/依赖；删空批次同步顺序。保持四架构与CI路由，记录修改前后实际结果。

本次只优化组织与构建复用；mem、pthread、sleep、pipe、epoll、HTTP的原有效断言保持，不将Rust底层通过当成C ABI保护。
